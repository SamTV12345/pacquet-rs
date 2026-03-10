use crate::{CreateVirtualDirBySnapshot, CreateVirtualDirError, progress_reporter};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, LockfileResolution, PackageSnapshot, PkgNameVerPeer};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::PackageFileInfo;
use pacquet_tarball::{DownloadTarballToStore, TarballError};
use pipe_trait::Pipe;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

/// This subroutine downloads a package tarball, extracts it, installs it to a virtual dir,
/// then creates the symlink layout for the package.
#[must_use]
pub struct InstallPackageBySnapshot<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub dependency_path: &'a DependencyPath,
    pub package_snapshot: &'a PackageSnapshot,
    pub offline: bool,
    pub force: bool,
}

/// Error type of [`InstallPackageBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageBySnapshotError {
    DownloadTarball(TarballError),
    CreateVirtualDir(CreateVirtualDirError),
}

impl<'a> InstallPackageBySnapshot<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<bool, InstallPackageBySnapshotError> {
        let InstallPackageBySnapshot {
            http_client,
            config,
            dependency_path,
            package_snapshot,
            offline,
            force,
        } = self;
        let PackageSnapshot { resolution, .. } = package_snapshot;
        let DependencyPath { custom_registry, package_specifier } = dependency_path;

        let (tarball_url, integrity) = match resolution {
            LockfileResolution::Tarball(tarball_resolution) => {
                let integrity = tarball_resolution.integrity.as_ref().unwrap_or_else(|| {
                    // TODO: how to handle the absent of integrity field?
                    panic!("Current implementation requires integrity, but {dependency_path} doesn't have it");
                });
                (tarball_resolution.tarball.as_str().pipe(Cow::Borrowed), integrity)
            }
            LockfileResolution::Registry(registry_resolution) => {
                let registry = registry_for_dependency_path(
                    config,
                    custom_registry.as_deref(),
                    &package_specifier.name.to_string(),
                );
                let registry = registry.strip_suffix('/').unwrap_or(&registry);
                let PkgNameVerPeer { name, suffix: ver_peer } = package_specifier;
                let version = ver_peer.version();
                let bare_name = name.bare.as_str();
                let tarball_url = format!("{registry}/{name}/-/{bare_name}-{version}.tgz");
                let integrity = &registry_resolution.integrity;
                (Cow::Owned(tarball_url), integrity)
            }
            LockfileResolution::Directory(_) | LockfileResolution::Git(_) => {
                panic!(
                    "Only TarballResolution and RegistryResolution is supported at the moment, but {dependency_path} requires {resolution:?}"
                );
            }
        };

        // TODO: skip when already exists in store?
        let package_id =
            format!("{}@{}", package_specifier.name, package_specifier.suffix.version());
        let save_path = config
            .virtual_store_dir
            .join(package_specifier.to_virtual_store_name())
            .join("node_modules")
            .join(package_specifier.name.to_string());

        if force && save_path.exists() {
            std::fs::remove_dir_all(&save_path).unwrap_or_else(|error| {
                panic!(
                    "remove existing virtual store package during --force should succeed: {error}"
                )
            });
        }

        // Fast warm-install check: if package.json already exists in the virtual store package
        // directory, the package contents are present and can be reused.
        if !force && save_path.join("package.json").is_file() {
            return Ok(false);
        }

        // pnpm skips import work when package contents are already present in virtual store.
        // Keep the check cheap by probing a representative file from the store index.
        if !force
            && config
                .store_dir
                .read_index_file(integrity, &package_id)
                .is_some_and(|index| package_is_already_imported(&save_path, &index.files))
        {
            return Ok(false);
        }

        let cas_paths = DownloadTarballToStore {
            http_client,
            store_dir: &config.store_dir,
            package_id: &package_id,
            package_integrity: integrity,
            package_unpacked_size: None,
            auth_header: config.auth_header_for_url(&tarball_url),
            package_url: &tarball_url,
            offline,
            force,
        }
        .run_without_mem_cache()
        .await
        .map_err(InstallPackageBySnapshotError::DownloadTarball)?;
        progress_reporter::fetched();

        CreateVirtualDirBySnapshot {
            virtual_store_dir: &config.virtual_store_dir,
            cas_paths: &cas_paths,
            import_method: config.package_import_method,
            dependency_path,
            package_snapshot,
        }
        .run()
        .map_err(InstallPackageBySnapshotError::CreateVirtualDir)?;
        progress_reporter::linked();

        Ok(true)
    }
}

fn registry_for_dependency_path(
    config: &Npmrc,
    custom_registry: Option<&str>,
    package_name: &str,
) -> String {
    custom_registry
        .map(|registry| {
            if registry.starts_with("http://") || registry.starts_with("https://") {
                registry.to_string()
            } else {
                format!("https://{registry}")
            }
        })
        .unwrap_or_else(|| config.registry_for_package_name(package_name))
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
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn representative_prefers_package_json() {
        let files = ["index.js", "package.json", "README.md"];
        assert_eq!(representative_file_name(files.into_iter()), Some("package.json"));
    }

    #[test]
    fn representative_falls_back_to_lexicographically_smallest() {
        let files = ["z.js", "a.js", "m.js"];
        assert_eq!(representative_file_name(files.into_iter()), Some("a.js"));
    }

    #[test]
    fn registry_for_dependency_path_prefers_scoped_registry_from_config() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join(".npmrc"),
            "registry=https://default.example/\n@foo:registry=https://foo.example/\n",
        )
        .expect("write .npmrc");
        let config = Npmrc::current(|| Ok::<_, ()>(dir.path().to_path_buf()), || None, Npmrc::new)
            .expect("load npmrc");

        assert_eq!(registry_for_dependency_path(&config, None, "@foo/pkg"), "https://foo.example/");
    }

    #[test]
    fn registry_for_dependency_path_preserves_absolute_custom_registry() {
        let config = Npmrc::new();
        assert_eq!(
            registry_for_dependency_path(&config, Some("http://localhost:4873"), "pkg"),
            "http://localhost:4873"
        );
    }
}
