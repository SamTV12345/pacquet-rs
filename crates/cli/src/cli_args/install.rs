use crate::State;
use clap::Args;
use miette::Context;
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::{NodeLinker, Npmrc};
use pacquet_package_manager::{Install, WorkspacePackages};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_store_dir::StoreDir;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Args)]
pub struct InstallDependencyOptions {
    /// pacquet will not install any package listed in devDependencies and will remove those insofar
    /// they were already installed, if the NODE_ENV environment variable is set to production.
    /// Use this flag to instruct pacquet to ignore NODE_ENV and take its production status from this
    /// flag instead.
    #[arg(short = 'P', long)]
    prod: bool,
    /// Only devDependencies are installed and dependencies are removed insofar they were
    /// already installed, regardless of the NODE_ENV.
    #[arg(short = 'D', long)]
    dev: bool,
    /// optionalDependencies are not installed.
    #[arg(long)]
    no_optional: bool,
}

impl InstallDependencyOptions {
    /// Convert the dependency options to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &InstallDependencyOptions { prod, dev, no_optional } = self;
        let has_both = prod == dev;
        let has_prod = has_both || prod;
        let has_dev = has_both || dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Prod))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: InstallDependencyOptions,

    /// Don't generate a lockfile and fail if the lockfile is outdated.
    #[clap(long)]
    pub frozen_lockfile: bool,

    /// Fix broken lockfile entries and proceed even when frozen lockfile checks would fail.
    #[clap(long)]
    pub fix_lockfile: bool,

    /// Skip lifecycle scripts during installation.
    #[clap(long)]
    pub ignore_scripts: bool,

    /// Resolve dependencies and write pnpm-lock.yaml without installing into node_modules.
    #[clap(long)]
    pub lockfile_only: bool,

    /// Force reinstall dependencies and bypass local store/virtual-store reuse shortcuts.
    #[clap(long)]
    pub force: bool,

    /// Resolve dependencies only and write lockfile changes without installing.
    #[clap(long)]
    pub resolution_only: bool,

    /// Reporter name (accepted for compatibility).
    #[clap(long)]
    pub reporter: Option<String>,

    /// Starts a store server in the background (currently accepted for compatibility).
    #[clap(long)]
    pub use_store_server: bool,

    /// Hoist all dependencies to the root of the virtual store.
    #[clap(long)]
    pub shamefully_hoist: bool,

    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    pub filter: Vec<String>,
    /// Install recursively in every workspace project (including workspace root).
    #[clap(short = 'r', long)]
    pub recursive: bool,

    /// Skip staleness checks for cached metadata and prefer local metadata when possible.
    #[clap(long)]
    pub prefer_offline: bool,

    /// Disallow network requests and use only locally available lockfile/store data.
    #[clap(long)]
    pub offline: bool,
}

impl InstallArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        let State {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            resolved_packages,
        } = &state;
        let InstallArgs {
            dependency_options,
            frozen_lockfile,
            fix_lockfile,
            ignore_scripts: _ignore_scripts,
            lockfile_only,
            force,
            resolution_only,
            reporter: _reporter,
            use_store_server: _use_store_server,
            shamefully_hoist,
            filter,
            recursive,
            prefer_offline,
            offline,
        } = self;
        let lockfile_only = lockfile_only || resolution_only;
        let frozen_lockfile = frozen_lockfile && !fix_lockfile;
        let dependency_groups = dependency_options.dependency_groups().collect::<Vec<_>>();

        let mut install_targets = BTreeMap::<String, PathBuf>::new();
        if recursive && !workspace_packages.is_empty() {
            let workspace_root_manifest = lockfile_dir.join("package.json");
            if workspace_root_manifest.is_file() {
                install_targets.insert(".".to_string(), workspace_root_manifest);
            }
            for info in workspace_packages.values() {
                let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                install_targets.insert(importer_id, info.root_dir.join("package.json"));
            }
        } else {
            install_targets.insert(lockfile_importer_id.clone(), manifest.path().to_path_buf());

            let is_workspace_root = lockfile_importer_id == "."
                && manifest.path().parent().is_some_and(|parent| parent == lockfile_dir.as_path());
            if is_workspace_root {
                for info in workspace_packages.values() {
                    let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                    install_targets
                        .entry(importer_id)
                        .or_insert_with(|| info.root_dir.join("package.json"));
                }
            }
        }
        install_targets = apply_workspace_filters(
            install_targets,
            manifest,
            workspace_packages,
            lockfile_dir,
            &filter,
        )?;

        let mut current_lockfile = lockfile.clone();
        for (importer_id, manifest_path) in install_targets {
            let workspace_manifest = if manifest_path == manifest.path() {
                None
            } else {
                Some(PackageManifest::from_path(manifest_path.clone()).wrap_err_with(|| {
                    format!("load workspace manifest: {}", manifest_path.display())
                })?)
            };
            let target_manifest = workspace_manifest.as_ref().unwrap_or(manifest);
            let project_dir = target_manifest
                .path()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| lockfile_dir.to_path_buf());
            let target_config = config_for_project(config, &project_dir, shamefully_hoist).leak();

            Install {
                tarball_mem_cache,
                http_client,
                config: target_config,
                manifest: target_manifest,
                lockfile: current_lockfile.as_ref(),
                lockfile_dir,
                lockfile_importer_id: &importer_id,
                workspace_packages,
                dependency_groups: dependency_groups.iter().copied(),
                frozen_lockfile,
                lockfile_only,
                force,
                prefer_offline,
                offline,
                resolved_packages,
            }
            .run()
            .await?;

            current_lockfile = if config.lockfile {
                Lockfile::load_from_dir(lockfile_dir)
                    .wrap_err("reload lockfile after workspace install")?
            } else {
                None
            };
        }

        Ok(())
    }
}

fn apply_workspace_filters(
    install_targets: BTreeMap<String, PathBuf>,
    root_manifest: &PackageManifest,
    workspace_packages: &WorkspacePackages,
    lockfile_dir: &Path,
    filters: &[String],
) -> miette::Result<BTreeMap<String, PathBuf>> {
    if filters.is_empty() {
        return Ok(install_targets);
    }

    let root_name = root_manifest
        .value()
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);

    let selected = install_targets
        .into_iter()
        .filter(|(importer_id, _)| {
            filters.iter().any(|selector| {
                let normalized_selector = selector.trim_start_matches("./").replace('\\', "/");
                if importer_id == &normalized_selector || importer_id == selector {
                    return true;
                }

                if importer_id == "." {
                    return root_name
                        .as_deref()
                        .is_some_and(|name| name == selector || name == normalized_selector);
                }

                workspace_packages.iter().any(|(name, info)| {
                    to_lockfile_importer_id(lockfile_dir, &info.root_dir) == *importer_id
                        && (name == selector || name == &normalized_selector)
                })
            })
        })
        .collect::<BTreeMap<_, _>>();

    if selected.is_empty() {
        miette::bail!("No workspace projects matched --filter selectors: {}", filters.join(", "));
    }

    Ok(selected)
}

fn config_for_project(config: &Npmrc, project_dir: &Path, shamefully_hoist: bool) -> Npmrc {
    let mut next = config.clone();
    next.store_dir = StoreDir::new(config.store_dir.display().to_string());
    next.modules_dir = project_dir.join("node_modules");
    next.node_linker = match config.node_linker {
        NodeLinker::Isolated => NodeLinker::Isolated,
        NodeLinker::Hoisted => NodeLinker::Hoisted,
        NodeLinker::Pnp => NodeLinker::Pnp,
    };
    if shamefully_hoist {
        next.shamefully_hoist = true;
        next.hoist = true;
    }
    next
}

fn to_lockfile_importer_id(workspace_root: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(workspace_root) else {
        return ".".to_string();
    };
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_package_manifest::DependencyGroup;
    use pretty_assertions::assert_eq;

    #[test]
    fn dependency_options_to_dependency_groups() {
        use DependencyGroup::{Dev, Optional, Prod};
        let create_list =
            |opts: InstallDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod + dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: false }),
            [Prod, Dev, Optional],
        );

        // --prod -> prod + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: false }),
            [Prod, Optional],
        );

        // --dev -> dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: false }),
            [Dev, Optional],
        );

        // --no-optional -> prod + dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: true }),
            [Prod, Dev],
        );

        // --prod --no-optional -> prod
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: true }),
            [Prod],
        );

        // --dev --no-optional -> dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: true }),
            [Dev],
        );

        // --prod --dev -> prod + dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: false }),
            [Prod, Dev, Optional],
        );

        // --prod --dev --no-optional -> prod + dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: true }),
            [Prod, Dev],
        );
    }
}
