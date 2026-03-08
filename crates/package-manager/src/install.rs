use crate::{
    InstallFrozenLockfile, InstallWithLockfile, InstallWithoutLockfile, ResolvedPackages,
    WorkspacePackages, collect_runtime_lockfile_config, get_outdated_lockfile_setting,
    satisfies_package_manifest,
};
use pacquet_lockfile::{Lockfile, RootProjectSnapshot};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_tarball::MemCache;
use std::path::Path;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub lockfile_importer_id: &'a str,
    pub workspace_packages: &'a WorkspacePackages,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> miette::Result<()> {
        let Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            dependency_groups,
            frozen_lockfile,
        } = self;

        tracing::info!(target: "pacquet::install", "Start all");

        match (config.lockfile, frozen_lockfile, lockfile) {
            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    manifest,
                    dependency_groups,
                }
                .run()
                .await;
            }
            (true, false, Some(_)) | (true, false, None) => {
                InstallWithLockfile {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    manifest,
                    existing_lockfile: lockfile,
                    lockfile_dir,
                    lockfile_importer_id,
                    workspace_packages,
                    dependency_groups,
                }
                .run()
                .await;
            }
            (true, true, None) => {
                miette::bail!(
                    "Cannot install with --frozen-lockfile because pnpm-lock.yaml was not found"
                );
            }
            (true, true, Some(lockfile)) => {
                let Lockfile { lockfile_version, project_snapshot, packages, .. } = lockfile;
                assert!(
                    lockfile_version.major == 6 || lockfile_version.major == 9,
                    "unsupported lockfile major version: {}",
                    lockfile_version.major
                );

                let project_snapshot = match project_snapshot {
                    RootProjectSnapshot::Single(snapshot) => snapshot,
                    RootProjectSnapshot::Multi(snapshot) => {
                        snapshot.importers.get(lockfile_importer_id).ok_or_else(|| {
                            miette::miette!(
                                "Cannot find importer `{}` in pnpm-lock.yaml",
                                lockfile_importer_id
                            )
                        })?
                    }
                };

                let runtime_lockfile_config =
                    collect_runtime_lockfile_config(config, manifest, lockfile_dir);
                if let Some(outdated_setting) =
                    get_outdated_lockfile_setting(lockfile, &runtime_lockfile_config)
                {
                    miette::bail!(
                        "Cannot proceed with the frozen installation. The current \"{outdated_setting}\" configuration doesn't match the value found in the lockfile"
                    );
                }
                if let Err(reason) = satisfies_package_manifest(
                    project_snapshot,
                    manifest,
                    config.auto_install_peers,
                    config.exclude_links_from_lockfile,
                    lockfile_version.major >= 9,
                ) {
                    miette::bail!(
                        "Cannot install with --frozen-lockfile because pnpm-lock.yaml is not up to date with {} ({reason})",
                        manifest.path().display(),
                    );
                }

                InstallFrozenLockfile {
                    http_client,
                    config,
                    project_snapshot,
                    packages: packages.as_ref(),
                    dependency_groups,
                }
                .run()
                .await;
            }
        }

        tracing::info!(target: "pacquet::install", "Complete all");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::{DependencyGroup, PackageManifest};
    use pacquet_registry_mock::AutoMockInstance;
    use pacquet_testing_utils::fs::{get_all_folders, is_symlink_or_junction};
    use tempfile::tempdir;

    #[test]
    fn should_install_dependencies() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules"); // TODO: we shouldn't have to define this
        let virtual_store_dir = modules_dir.join(".pacquet"); // TODO: we shouldn't have to define this

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        manifest
            .add_dependency("@pnpm.e2e/hello-world-js-bin", "1.0.0", DependencyGroup::Prod)
            .unwrap();
        manifest.add_dependency("@pnpm/xyz", "1.0.0", DependencyGroup::Dev).unwrap();

        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.registry = mock_instance.url();
        let config = config.leak();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
            .block_on(async {
                Install {
                    tarball_mem_cache: &Default::default(),
                    http_client: &Default::default(),
                    config,
                    manifest: &manifest,
                    lockfile: None,
                    lockfile_dir: dir.path(),
                    lockfile_importer_id: ".",
                    workspace_packages: &Default::default(),
                    dependency_groups: [
                        DependencyGroup::Prod,
                        DependencyGroup::Dev,
                        DependencyGroup::Optional,
                    ],
                    frozen_lockfile: false,
                    resolved_packages: &Default::default(),
                }
                .run()
                .await
                .unwrap();
            });

        // Make sure the package is installed
        let path = project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm.e2e+hello-world-js-bin@1.0.0");
        assert!(path.exists());
        // Make sure we install dev-dependencies as well
        let path = project_root.join("node_modules/@pnpm/xyz");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm+xyz@1.0.0");
        assert!(path.is_dir());

        insta::assert_debug_snapshot!(get_all_folders(&project_root));

        drop((dir, mock_instance)); // cleanup
    }
}
