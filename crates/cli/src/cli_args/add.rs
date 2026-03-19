use crate::State;
use crate::cli_args::install::parse_install_reporter;
use clap::Args;
use miette::Context;
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::{
    Add, current_lockfile_for_installers_preserving_unselected_importers,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub struct AddDependencyOptions {
    /// Install the specified packages as regular dependencies.
    #[clap(short = 'P', long)]
    save_prod: bool,
    /// Install the specified packages as devDependencies.
    #[clap(short = 'D', long)]
    save_dev: bool,
    /// Install the specified packages as optionalDependencies.
    #[clap(short = 'O', long)]
    save_optional: bool,
    /// Using --save-peer will add one or more packages to peerDependencies and install them as dev dependencies
    #[clap(long)]
    save_peer: bool,
}

impl AddDependencyOptions {
    /// Whether to add entry to `"dependencies"`.
    ///
    /// **NOTE:** no `--save-*` flags implies save as prod.
    #[inline(always)]
    fn save_prod(&self) -> bool {
        let &AddDependencyOptions { save_prod, save_dev, save_optional, save_peer } = self;
        save_prod || (!save_dev && !save_optional && !save_peer)
    }

    /// Whether to add entry to `"devDependencies"`.
    ///
    /// **NOTE:** `--save-peer` without any other `--save-*` flags implies save as dev.
    #[inline(always)]
    fn save_dev(&self) -> bool {
        let &AddDependencyOptions { save_prod, save_dev, save_optional, save_peer } = self;
        save_dev || (!save_prod && !save_optional && save_peer)
    }

    /// Whether to add entry to `"optionalDependencies"`.
    #[inline(always)]
    fn save_optional(&self) -> bool {
        self.save_optional
    }

    /// Whether to add entry to `"peerDependencies"`.
    #[inline(always)]
    fn save_peer(&self) -> bool {
        self.save_peer
    }

    /// Convert the `--save-*` flags to an iterator of [`DependencyGroup`]
    /// which selects which target group to save to.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        std::iter::empty()
            .chain(self.save_prod().then_some(DependencyGroup::Prod))
            .chain(self.save_dev().then_some(DependencyGroup::Dev))
            .chain(self.save_optional().then_some(DependencyGroup::Optional))
            .chain(self.save_peer().then_some(DependencyGroup::Peer))
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// One or more package specs
    #[arg(required = true)]
    pub packages: Vec<String>,
    /// --save-prod, --save-dev, --save-optional, --save-peer
    #[clap(flatten)]
    pub dependency_options: AddDependencyOptions,
    /// Saved dependencies will be configured with an exact version rather than using
    /// the default semver range operator.
    #[clap(short = 'E', long = "save-exact")]
    pub save_exact: bool,
    /// Only adds packages that are already present in the workspace.
    #[clap(long = "workspace")]
    pub workspace: bool,
    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    pub filter: Vec<String>,
    /// Add dependencies to every workspace project recursively (excluding workspace root).
    #[clap(short = 'r', long)]
    pub recursive: bool,
    /// The directory with links to the store (default is node_modules/.pacquet).
    /// All direct and indirect dependencies of the project are linked into this directory
    #[clap(long = "virtual-store-dir", default_value = "node_modules/.pacquet")]
    pub virtual_store_dir: Option<PathBuf>, // TODO: make use of this
    /// Add dependencies to the workspace root package even when this is not explicitly requested.
    #[clap(long = "ignore-workspace-root-check")]
    pub ignore_workspace_root_check: bool,
    /// Reporter name.
    #[clap(long)]
    pub reporter: Option<String>,
    /// Disable pnpm hooks defined in .pnpmfile.cjs.
    #[clap(long)]
    pub ignore_pnpmfile: bool,
    /// Use hooks from the specified pnpmfile instead of <lockfileDir>/.pnpmfile.cjs.
    #[clap(long)]
    pub pnpmfile: Option<PathBuf>,
    #[arg(skip)]
    pub invoked_with_workspace_root: bool,
}

impl AddArgs {
    /// Execute the subcommand.
    pub async fn run(self, mut state: State) -> miette::Result<()> {
        // TODO: if a package already exists in another dependency group, don't remove the existing entry.
        let AddArgs {
            packages,
            dependency_options,
            save_exact,
            workspace,
            filter,
            recursive,
            virtual_store_dir: _virtual_store_dir,
            ignore_workspace_root_check,
            reporter,
            ignore_pnpmfile,
            pnpmfile,
            invoked_with_workspace_root,
        } = self;
        let reporter = parse_install_reporter(reporter.as_deref())?;

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
        } = &mut state;

        if !filter.is_empty() || recursive {
            let mut targets = BTreeMap::<String, PathBuf>::new();
            for (name, info) in workspace_packages.iter() {
                let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                if recursive && importer_id == "." {
                    continue;
                }
                let matches = if filter.is_empty() {
                    true
                } else {
                    filter.iter().any(|selector| {
                        let normalized = selector.trim_start_matches("./").replace('\\', "/");
                        normalized == importer_id || normalized == *name
                    })
                };
                if matches {
                    targets.insert(importer_id, info.root_dir.join("package.json"));
                }
            }
            if targets.is_empty() {
                if recursive {
                    miette::bail!(
                        "No workspace projects found for --recursive. Ensure pnpm-workspace.yaml includes package patterns."
                    );
                }
                miette::bail!(
                    "No workspace projects matched --filter selectors: {}",
                    filter.join(", ")
                );
            }

            let selected_importers = targets.keys().cloned().collect::<HashSet<_>>();
            let mut skipped_dep_paths = HashSet::<String>::new();
            let mut current_lockfile = lockfile.clone();
            for (importer_id, manifest_path) in targets {
                let mut target_manifest = PackageManifest::from_path(manifest_path.clone())
                    .wrap_err_with(|| {
                        format!("load workspace manifest: {}", manifest_path.display())
                    })?;
                let project_dir = target_manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.to_path_buf());
                let target_config = config_for_project(config, &project_dir).leak();

                let skipped = Add {
                    tarball_mem_cache,
                    http_client,
                    config: target_config,
                    manifest: &mut target_manifest,
                    lockfile: current_lockfile.as_ref(),
                    lockfile_dir,
                    lockfile_importer_id: &importer_id,
                    workspace_packages,
                    list_dependency_groups: || dependency_options.dependency_groups(),
                    packages: &packages,
                    save_exact,
                    workspace_only: workspace,
                    pnpmfile: pnpmfile.as_deref(),
                    ignore_pnpmfile,
                    reporter,
                    resolved_packages,
                }
                .run()
                .await
                .wrap_err("adding a new package")?;
                skipped_dep_paths.extend(skipped);

                current_lockfile = if config.lockfile {
                    Lockfile::load_from_dir(lockfile_dir)
                        .wrap_err("reload lockfile after filtered workspace add")?
                } else {
                    None
                };
            }

            if config.lockfile
                && let Some(lockfile) = current_lockfile.as_ref()
            {
                let current_lockfile =
                    current_lockfile_for_installers_preserving_unselected_importers(
                        lockfile,
                        config,
                        &selected_importers,
                        &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
                        &skipped_dep_paths,
                    );
                current_lockfile
                    .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
                    .wrap_err("write node_modules/.pnpm/lock.yaml after add")?;
            }

            return Ok(());
        }

        let is_workspace_root_manifest =
            lockfile_importer_id == "." && lockfile_dir.join("pnpm-workspace.yaml").is_file();
        if is_workspace_root_manifest
            && !ignore_workspace_root_check
            && !invoked_with_workspace_root
        {
            miette::bail!(
                "Running this command at the workspace root may be unintended. Use -w to target the workspace root explicitly, or pass --ignore-workspace-root-check."
            );
        }

        let skipped = Add {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile: lockfile.as_ref(),
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            list_dependency_groups: || dependency_options.dependency_groups(),
            packages: &packages,
            save_exact,
            workspace_only: workspace,
            pnpmfile: pnpmfile.as_deref(),
            ignore_pnpmfile,
            reporter,
            resolved_packages,
        }
        .run()
        .await
        .wrap_err("adding a new package")?;

        if config.lockfile {
            let current_lockfile =
                Lockfile::load_from_dir(lockfile_dir).wrap_err("reload lockfile after add")?;
            if let Some(lockfile) = current_lockfile.as_ref() {
                let current_lockfile =
                    current_lockfile_for_installers_preserving_unselected_importers(
                        lockfile,
                        config,
                        &HashSet::from([lockfile_importer_id.clone()]),
                        &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
                        &skipped.into_iter().collect::<HashSet<_>>(),
                    );
                current_lockfile
                    .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
                    .wrap_err("write node_modules/.pnpm/lock.yaml after add")?;
            }
        }

        Ok(())
    }
}

fn config_for_project(config: &Npmrc, project_dir: &Path) -> Npmrc {
    let mut next = config.clone();
    next.modules_dir = project_dir.join("node_modules");
    next.virtual_store_dir = next.modules_dir.join(".pnpm");
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
        use DependencyGroup::{Dev, Optional, Peer, Prod};
        let create_list = |opts: AddDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: false,
                save_peer: false
            }),
            [Prod]
        );

        // --save-prod -> prod
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: true,
                save_dev: false,
                save_optional: false,
                save_peer: false
            }),
            [Prod]
        );

        // --save-dev -> dev
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: true,
                save_optional: false,
                save_peer: false
            }),
            [Dev]
        );

        // --save-optional -> optional
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: true,
                save_peer: false
            }),
            [Optional]
        );

        // --save-peer -> dev + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: false,
                save_peer: true
            }),
            [Dev, Peer]
        );

        // --save-prod --save-peer -> prod + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: true,
                save_dev: false,
                save_optional: false,
                save_peer: true
            }),
            [Prod, Peer]
        );

        // --save-dev --save-peer -> dev + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: true,
                save_optional: false,
                save_peer: true
            }),
            [Dev, Peer]
        );

        // --save-optional --save-peer -> optional + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: true,
                save_peer: true
            }),
            [Optional, Peer]
        );
    }
}
