use crate::{
    CreateCasFilesError, SymlinkPackageError, create_cas_files,
    fetch_package_from_registry_and_cache, fetch_package_with_metadata_cache, is_git_spec,
    is_tarball_spec, progress_reporter, read_cached_package_from_config,
    resolve_package_version_from_git_spec, resolve_package_version_from_tarball_spec,
    symlink_package,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry::{PackageTag, PackageVersion, RegistryError};
use pacquet_store_dir::PackageFileInfo;
use pacquet_tarball::{DownloadTarballToStore, MemCache, TarballError};
use std::{collections::HashMap, path::Path};

/// This subroutine executes the following and returns the package
/// * Retrieves the package from the registry
/// * Extracts the tarball to global store directory (~/Library/../pacquet)
/// * Links global store directory to virtual dir (node_modules/.pacquet/..)
///
/// `symlink_path` will be appended by the name of the package. Therefore,
/// it should be resolved into the node_modules folder of a subdependency such as
/// `node_modules/.pacquet/fastify@1.0.0/node_modules`.
#[must_use]
pub struct InstallPackageFromRegistry<'a> {
    pub tarball_mem_cache: &'a MemCache,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub node_modules_dir: &'a Path,
    pub name: &'a str,
    pub version_range: &'a str,
    pub prefer_offline: bool,
    pub offline: bool,
    pub force: bool,
}

/// Error type of [`InstallPackageFromRegistry`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageFromRegistryError {
    FetchFromRegistry(#[error(source)] RegistryError),
    ResolveTarballSpec(#[error(not(source))] String),
    ResolveGitSpec(#[error(not(source))] String),
    DownloadTarballToStore(#[error(source)] TarballError),
    CreateCasFiles(#[error(source)] CreateCasFilesError),
    SymlinkPackage(#[error(source)] SymlinkPackageError),
}

impl<'a> InstallPackageFromRegistry<'a> {
    fn parse_npm_alias(version_range: &str) -> Option<(&str, &str)> {
        let alias = version_range.strip_prefix("npm:")?;
        let separator = alias.rfind('@');
        match separator {
            Some(index) if index > 0 => Some((&alias[..index], &alias[index + 1..])),
            _ => Some((alias, "latest")),
        }
    }

    fn resolve_requested_version(
        package: &pacquet_registry::Package,
        requested_name: &str,
        requested_range: &str,
    ) -> Result<PackageVersion, InstallPackageFromRegistryError> {
        if let Ok(version) = requested_range.parse::<node_semver::Version>() {
            return package.versions.get(&version.to_string()).cloned().ok_or_else(|| {
                InstallPackageFromRegistryError::FetchFromRegistry(
                    RegistryError::MissingVersionRelease(
                        version.to_string(),
                        requested_name.to_string(),
                    ),
                )
            });
        }

        if let Ok(tag) = requested_range.parse::<PackageTag>() {
            return Ok(match tag {
                PackageTag::Latest => package.latest().clone(),
                PackageTag::Version(version) => {
                    package.versions.get(&version.to_string()).cloned().ok_or_else(|| {
                        InstallPackageFromRegistryError::FetchFromRegistry(
                            RegistryError::MissingVersionRelease(
                                version.to_string(),
                                requested_name.to_string(),
                            ),
                        )
                    })?
                }
            });
        }

        package.pinned_version(requested_range).cloned().ok_or_else(|| {
            InstallPackageFromRegistryError::FetchFromRegistry(
                RegistryError::MissingVersionRelease(
                    requested_range.to_string(),
                    requested_name.to_string(),
                ),
            )
        })
    }

    /// Execute the subroutine.
    pub async fn run(self) -> Result<PackageVersion, InstallPackageFromRegistryError> {
        let &InstallPackageFromRegistry {
            http_client,
            config,
            name,
            version_range,
            prefer_offline,
            offline,
            ..
        } = &self;
        if is_tarball_spec(version_range) {
            let package_version =
                resolve_package_version_from_tarball_spec(config, http_client, version_range)
                    .await
                    .map_err(InstallPackageFromRegistryError::ResolveTarballSpec)?;
            progress_reporter::resolved();
            self.install_package_version(&package_version, name).await?;
            return Ok(package_version);
        }
        if is_git_spec(version_range) {
            let package_version =
                resolve_package_version_from_git_spec(config, http_client, version_range)
                    .await
                    .map_err(InstallPackageFromRegistryError::ResolveGitSpec)?;
            progress_reporter::resolved();
            self.install_package_version(&package_version, name).await?;
            return Ok(package_version);
        }
        let (requested_name, requested_range) =
            Self::parse_npm_alias(version_range).unwrap_or((name, version_range));
        let package = fetch_package_with_metadata_cache(
            config,
            http_client,
            requested_name,
            prefer_offline,
            offline,
        )
        .await;

        let maybe_cached = if prefer_offline && !offline {
            read_cached_package_from_config(config, requested_name)
        } else {
            None
        };
        let mut package_version =
            Self::resolve_requested_version(&package, requested_name, requested_range);
        if package_version.is_err() && maybe_cached.is_some() {
            let fresh = fetch_package_from_registry_and_cache(config, http_client, requested_name)
                .await
                .map_err(InstallPackageFromRegistryError::FetchFromRegistry)?;
            package_version =
                Self::resolve_requested_version(&fresh, requested_name, requested_range);
        }

        let package_version = package_version?;
        progress_reporter::resolved();
        self.install_package_version(&package_version, name).await?;
        Ok(package_version)
    }

    async fn install_package_version(
        self,
        package_version: &PackageVersion,
        symlink_name: &str,
    ) -> Result<(), InstallPackageFromRegistryError> {
        let InstallPackageFromRegistry {
            tarball_mem_cache,
            http_client,
            config,
            node_modules_dir,
            force,
            ..
        } = self;

        let store_folder_name = package_version.to_virtual_store_name();
        let package_id = format!("{}@{}", package_version.name, package_version.version);
        let save_path = config
            .virtual_store_dir
            .join(store_folder_name)
            .join("node_modules")
            .join(&package_version.name);
        let symlink_path = node_modules_dir.join(symlink_name);

        // Fast warm-install check: this package is already imported to the virtual store.
        if force && save_path.exists() {
            std::fs::remove_dir_all(&save_path).unwrap_or_else(|error| {
                panic!(
                    "remove existing virtual store package during --force should succeed: {error}"
                )
            });
        }

        if !force && save_path.join("package.json").is_file() {
            symlink_package(&save_path, &symlink_path)
                .map_err(InstallPackageFromRegistryError::SymlinkPackage)?;
            progress_reporter::linked();
            return Ok(());
        }

        if !force
            && config
                .store_dir
                .read_index_file(
                    package_version.dist.integrity.as_ref().expect("has integrity field"),
                    &package_id,
                )
                .is_some_and(|index| package_is_already_imported(&save_path, &index.files))
        {
            symlink_package(&save_path, &symlink_path)
                .map_err(InstallPackageFromRegistryError::SymlinkPackage)?;
            progress_reporter::linked();
            return Ok(());
        }

        // TODO: skip when it already exists in store?
        let cas_paths = DownloadTarballToStore {
            http_client,
            store_dir: &config.store_dir,
            package_id: &package_id,
            package_integrity: package_version
                .dist
                .integrity
                .as_ref()
                .expect("has integrity field"),
            package_unpacked_size: package_version.dist.unpacked_size,
            auth_header: config.auth_header_for_url(package_version.as_tarball_url()),
            package_url: package_version.as_tarball_url(),
            offline: self.offline,
            force,
        }
        .run_with_mem_cache(tarball_mem_cache)
        .await
        .map_err(InstallPackageFromRegistryError::DownloadTarballToStore)?;
        progress_reporter::fetched();

        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_path, "Import package");

        create_cas_files(config.package_import_method, &save_path, &cas_paths)
            .map_err(InstallPackageFromRegistryError::CreateCasFiles)?;

        symlink_package(&save_path, &symlink_path)
            .map_err(InstallPackageFromRegistryError::SymlinkPackage)?;
        progress_reporter::linked();

        Ok(())
    }
}

fn package_is_already_imported(
    save_path: &Path,
    index_files: &HashMap<String, PackageFileInfo>,
) -> bool {
    let Some(file_name) = representative_file_name(index_files.keys().map(String::as_str)) else {
        return false;
    };
    save_path.join(file_name).exists()
}

fn representative_file_name<'a>(file_names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut fallback: Option<&'a str> = None;
    for file_name in file_names {
        if file_name == "package.json" {
            return Some(file_name);
        }
        fallback = match fallback {
            Some(current) if current <= file_name => Some(current),
            _ => Some(file_name),
        };
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_npmrc::Npmrc;
    use pacquet_store_dir::StoreDir;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn create_config(store_dir: &Path, modules_dir: &Path, virtual_store_dir: &Path) -> Npmrc {
        let mut config = Npmrc::new();
        config.hoist = false;
        config.hoist_pattern = vec![];
        config.public_hoist_pattern = vec![];
        config.shamefully_hoist = false;
        config.store_dir = StoreDir::new(store_dir);
        config.modules_dir = modules_dir.to_path_buf();
        config.node_linker = Default::default();
        config.symlink = false;
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.package_import_method = Default::default();
        config.modules_cache_max_age = 0;
        config.network_concurrency = 16;
        config.lockfile = false;
        config.prefer_frozen_lockfile = false;
        config.lockfile_include_tarball_url = false;
        config.exclude_links_from_lockfile = false;
        config.inject_workspace_packages = false;
        config.peers_suffix_max_length = 1000;
        config.registry = "https://registry.npmjs.com/".to_string();
        config.auto_install_peers = false;
        config.dedupe_peer_dependents = false;
        config.strict_peer_dependencies = false;
        config.resolve_peers_from_workspace_root = false;
        config
    }

    #[tokio::test]
    pub async fn should_find_package_version_from_registry() {
        let store_dir = tempdir().unwrap();
        let modules_dir = tempdir().unwrap();
        let virtual_store_dir = tempdir().unwrap();
        let config: &'static Npmrc =
            create_config(store_dir.path(), modules_dir.path(), virtual_store_dir.path())
                .pipe(Box::new)
                .pipe(Box::leak);
        let http_client = ThrottledClient::new_from_cpu_count();
        let package = InstallPackageFromRegistry {
            tarball_mem_cache: &Default::default(),
            config,
            http_client: &http_client,
            name: "fast-querystring",
            version_range: "1.0.0",
            node_modules_dir: modules_dir.path(),
            prefer_offline: false,
            offline: false,
            force: false,
        }
        .run()
        .await
        .unwrap();

        assert_eq!(package.name, "fast-querystring");
        assert_eq!(
            package.version,
            node_semver::Version {
                major: 1,
                minor: 0,
                patch: 0,
                build: vec![],
                pre_release: vec![]
            }
        );

        let virtual_store_path = virtual_store_dir
            .path()
            .join(package.to_virtual_store_name())
            .join("node_modules")
            .join(&package.name);
        assert!(virtual_store_path.is_dir());

        // Make sure the symlink is resolving to the correct path
        assert_eq!(
            fs::canonicalize(modules_dir.path().join(&package.name)).unwrap(),
            fs::canonicalize(virtual_store_path).unwrap()
        );
    }
}
