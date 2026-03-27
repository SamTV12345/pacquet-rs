use crate::InstallPackageFromRegistry;
use async_recursion::async_recursion;
use dashmap::DashSet;
use futures_util::future;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::PackageVersion;
use pacquet_tarball::MemCache;
use pipe_trait::Pipe;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// In-memory cache for packages that have started resolving dependencies.
///
/// The contents of set is the package's virtual_store_name.
/// e.g. `@pnpm.e2e/dep-1@1.0.0` →  `@pnpm.e2e+dep-1@1.0.0`
pub type ResolvedPackages = DashSet<String>;

/// This subroutine install packages from a `package.json` without reading or writing a lockfile.
///
/// **Brief overview for each package:**
/// * Fetch a tarball of the package.
/// * Extract the tarball into the store directory.
/// * Import (by reflink, hardlink, or copy) the files from the store dir to `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at `node_modules/{name}`.
/// * Repeat the process for the dependencies of the package.
#[must_use]
pub struct InstallWithoutLockfile<'a, DependencyGroupList> {
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile_dir: &'a Path,
    pub dependency_groups: DependencyGroupList,
    pub force: bool,
    pub prefer_offline: bool,
    pub offline: bool,
    pub pnpmfile: Option<&'a Path>,
    pub ignore_pnpmfile: bool,
}

impl<'a, DependencyGroupList> InstallWithoutLockfile<'a, DependencyGroupList> {
    /// Execute the subroutine.
    pub async fn run(self)
    where
        DependencyGroupList: IntoIterator<Item = DependencyGroup>,
    {
        let InstallWithoutLockfile {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile_dir,
            dependency_groups,
            resolved_packages,
            force,
            prefer_offline,
            offline,
            pnpmfile,
            ignore_pnpmfile,
        } = self;
        let workspace_root_peer_overrides = workspace_root_peer_overrides(manifest.path());

        let hooked_manifest = crate::apply_read_package_hook_to_manifest(
            lockfile_dir,
            pnpmfile,
            ignore_pnpmfile,
            manifest.value(),
        )
        .expect("apply .pnpmfile.cjs readPackage hook to project manifest");

        let _: Vec<()> = crate::dependencies_from_manifest_value_grouped(
            &hooked_manifest,
            dependency_groups,
        )
        .into_iter()
        .map(|(group, name, version_range)| {
            let workspace_root_peer_overrides = workspace_root_peer_overrides.clone();
            async move {
                if let Some(link_target) = version_range.strip_prefix("link:") {
                    let project_dir = manifest.path().parent().unwrap_or_else(|| Path::new("."));
                    let link_target = if Path::new(link_target).is_absolute() {
                        Path::new(link_target).to_path_buf()
                    } else {
                        project_dir.join(link_target)
                    };
                    if config.symlink {
                        crate::link_package(
                            config.symlink,
                            &link_target,
                            &config.modules_dir.join(&name),
                        )
                        .expect("symlink local link dependency");
                    }
                    return;
                }
                if let Some(file_target) = version_range.strip_prefix("file:") {
                    let project_dir = manifest.path().parent().unwrap_or_else(|| Path::new("."));
                    let file_target = if Path::new(file_target).is_absolute() {
                        Path::new(file_target).to_path_buf()
                    } else {
                        project_dir.join(file_target)
                    };
                    crate::import_local_package_dir(
                        config.package_import_method,
                        &file_target,
                        &config.modules_dir.join(&name),
                    )
                    .expect("materialize local file dependency");
                    return;
                }
                let resolved_range = apply_workspace_root_peer_override(
                    config,
                    &workspace_root_peer_overrides,
                    None,
                    &name,
                    &version_range,
                );

                let is_optional = matches!(group, DependencyGroup::Optional);
                let dependency = match (InstallPackageFromRegistry {
                    tarball_mem_cache,
                    http_client,
                    config,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                    node_modules_dir: &config.modules_dir,
                    name: &name,
                    version_range: resolved_range.as_str(),
                    optional: is_optional,
                    prefer_offline,
                    offline,
                    force,
                })
                .run()
                .await
                {
                    Ok(dep) => dep,
                    Err(error) => {
                        if is_optional {
                            tracing::debug!(
                                %name,
                                "Skipping optional dependency that failed to resolve: {error}"
                            );
                            return;
                        }
                        tracing::error!(%name, "Failed to install dependency: {error}");
                        return;
                    }
                };

                if matches!(
                    crate::installability::check_package_version_installability(
                        &dependency,
                        matches!(group, DependencyGroup::Optional)
                    ),
                    crate::installability::Installability::SkipOptional
                ) {
                    return;
                }

                InstallWithoutLockfile {
                    tarball_mem_cache,
                    http_client,
                    config,
                    manifest,
                    lockfile_dir,
                    dependency_groups: (),
                    resolved_packages,
                    force,
                    prefer_offline,
                    offline,
                    pnpmfile,
                    ignore_pnpmfile,
                }
                .install_dependencies_from_registry(&dependency, &workspace_root_peer_overrides)
                .await;
            }
        })
        .pipe(future::join_all)
        .await;
    }
}

impl<'a> InstallWithoutLockfile<'a, ()> {
    /// Install dependencies of a dependency.
    #[async_recursion]
    async fn install_dependencies_from_registry(
        &self,
        package: &PackageVersion,
        workspace_root_peer_overrides: &HashMap<String, String>,
    ) {
        let InstallWithoutLockfile {
            tarball_mem_cache,
            http_client,
            config,
            lockfile_dir,
            resolved_packages,
            force,
            prefer_offline,
            offline,
            pnpmfile,
            ignore_pnpmfile,
            ..
        } = self;

        // This package has already resolved, there is no need to reinstall again.
        if !resolved_packages.insert(package.to_virtual_store_name()) {
            tracing::info!(target: "pacquet::install", package = ?package.to_virtual_store_name(), "Skip subset");
            return;
        }

        let node_modules_path = self
            .config
            .virtual_store_dir
            .join(package.to_virtual_store_name())
            .join("node_modules");

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Start subset");

        let package = crate::apply_read_package_hook_to_package_version(
            lockfile_dir,
            *pnpmfile,
            *ignore_pnpmfile,
            package,
        )
        .unwrap_or_else(|_| package.clone());

        let mut dependencies = package
            .regular_dependencies()
            .map(|(name, version_range)| (false, name.to_string(), version_range.to_string()))
            .collect::<Vec<_>>();
        dependencies.extend(
            package
                .optional_dependencies_iter()
                .map(|(name, version_range)| (true, name.to_string(), version_range.to_string())),
        );
        if self.config.auto_install_peers {
            dependencies.extend(package.peer_dependencies_iter().map(|(name, version_range)| {
                let is_optional = package.is_peer_optional(name);
                (is_optional, name.to_string(), version_range.to_string())
            }));
        }
        let peer_dependencies = package.peer_dependencies.clone();

        dependencies
            .into_iter()
            .map(|(optional, name, version_range)| {
                let peer_dependencies = peer_dependencies.clone();
                let node_modules_path = node_modules_path.clone();
                async move {
                    let version_range = apply_workspace_root_peer_override(
                        self.config,
                        workspace_root_peer_overrides,
                        peer_dependencies.as_ref(),
                        &name,
                        &version_range,
                    );
                    let dependency = match (InstallPackageFromRegistry {
                        tarball_mem_cache,
                        http_client,
                        config,
                        lockfile_dir,
                        pnpmfile: *pnpmfile,
                        ignore_pnpmfile: *ignore_pnpmfile,
                        node_modules_dir: &node_modules_path,
                        name: &name,
                        version_range: &version_range,
                        optional,
                        prefer_offline: *prefer_offline,
                        offline: *offline,
                        force: *force,
                    })
                    .run()
                    .await
                    {
                        Ok(dep) => dep,
                        Err(error) => {
                            if optional {
                                tracing::debug!(
                                    %name,
                                    "Skipping optional sub-dependency that failed to resolve: {error}"
                                );
                            } else {
                                tracing::error!(%name, "Failed to install sub-dependency: {error}");
                            }
                            return;
                        }
                    };
                    if matches!(
                        crate::installability::check_package_version_installability(
                            &dependency,
                            optional
                        ),
                        crate::installability::Installability::SkipOptional
                    ) {
                        return;
                    }
                    self.install_dependencies_from_registry(
                        &dependency,
                        workspace_root_peer_overrides,
                    )
                    .await;
                }
            })
            .pipe(future::join_all)
            .await;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");
    }
}

fn apply_workspace_root_peer_override(
    config: &Npmrc,
    workspace_root_peer_overrides: &HashMap<String, String>,
    peer_dependencies: Option<&HashMap<String, String>>,
    name: &str,
    requested_range: &str,
) -> String {
    if !config.resolve_peers_from_workspace_root {
        return requested_range.to_string();
    }

    let is_peer = peer_dependencies.is_some_and(|peers| peers.contains_key(name));
    if !is_peer {
        return requested_range.to_string();
    }

    workspace_root_peer_overrides.get(name).cloned().unwrap_or_else(|| requested_range.to_string())
}

fn workspace_root_peer_overrides(manifest_path: &Path) -> HashMap<String, String> {
    let Some(start_dir) = manifest_path.parent() else {
        return HashMap::new();
    };
    let Some(workspace_root) = find_workspace_root(start_dir) else {
        return HashMap::new();
    };
    read_dependency_specs(&workspace_root.join("package.json"))
}

fn find_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join("pnpm-workspace.yaml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn read_dependency_specs(package_json_path: &Path) -> HashMap<String, String> {
    let Ok(text) = fs::read_to_string(package_json_path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };

    ["dependencies", "optionalDependencies", "devDependencies", "peerDependencies"]
        .into_iter()
        .flat_map(|field| {
            value
                .get(field)
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, spec)| {
                    spec.as_str().map(|spec| (name.to_string(), spec.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workspace_root_peer_overrides_reads_root_package_dependencies() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_dir = workspace_root.join("packages/app");
        fs::create_dir_all(&project_dir).expect("create project dir");
        fs::write(workspace_root.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
            .expect("write workspace manifest");
        fs::write(
            workspace_root.join("package.json"),
            serde_json::json!({
                "name": "workspace-root",
                "version": "1.0.0",
                "dependencies": {
                    "react": "^18.3.0"
                }
            })
            .to_string(),
        )
        .expect("write root package.json");
        fs::write(project_dir.join("package.json"), "{\"name\":\"app\",\"version\":\"1.0.0\"}")
            .expect("write app package.json");

        let overrides = workspace_root_peer_overrides(&project_dir.join("package.json"));
        assert_eq!(overrides.get("react").map(String::as_str), Some("^18.3.0"));
    }

    #[test]
    fn apply_workspace_root_peer_override_uses_workspace_spec_for_peer() {
        let mut config = Npmrc::new();
        config.resolve_peers_from_workspace_root = true;
        let root = HashMap::from([("react".to_string(), "^18.3.0".to_string())]);
        let peers = HashMap::from([("react".to_string(), "^18.0.0".to_string())]);

        let resolved =
            apply_workspace_root_peer_override(&config, &root, Some(&peers), "react", "^18.0.0");
        assert_eq!(resolved, "^18.3.0");
    }

    #[test]
    fn apply_workspace_root_peer_override_keeps_non_peer_range() {
        let mut config = Npmrc::new();
        config.resolve_peers_from_workspace_root = true;
        let root = HashMap::from([("react".to_string(), "^18.3.0".to_string())]);
        let peers = HashMap::from([("typescript".to_string(), "^5.0.0".to_string())]);

        let resolved =
            apply_workspace_root_peer_override(&config, &root, Some(&peers), "react", "^18.0.0");
        assert_eq!(resolved, "^18.0.0");
    }
}
