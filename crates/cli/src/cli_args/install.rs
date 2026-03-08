use crate::State;
use clap::Args;
use miette::Context;
use pacquet_lockfile::Lockfile;
use pacquet_package_manager::Install;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
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
        let InstallArgs { dependency_options, frozen_lockfile } = self;
        let dependency_groups = dependency_options.dependency_groups().collect::<Vec<_>>();

        let mut install_targets = BTreeMap::<String, PathBuf>::new();
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
            let mut target_config = (*config).clone();
            target_config.modules_dir = project_dir.join("node_modules");
            let target_config = target_config.leak();

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
