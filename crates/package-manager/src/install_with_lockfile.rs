use crate::{
    InstallPackageFromRegistry, ResolvedPackages, WorkspacePackages,
    collect_runtime_lockfile_config, resolve_workspace_dependency, symlink_package,
};
use async_recursion::async_recursion;
use dashmap::DashMap;
use node_semver::Version;
use pacquet_lockfile::{
    ComVer, DependencyPath, Lockfile, LockfileResolution, MultiProjectSnapshot, PackageSnapshot,
    PackageSnapshotDependency, PkgName, PkgNameVerPeer, PkgVerPeer, ProjectSnapshot,
    RegistryResolution, ResolvedDependencyMap, ResolvedDependencySpec, ResolvedDependencyVersion,
    RootProjectSnapshot, TarballResolution,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::PackageVersion;
use pacquet_tarball::MemCache;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Install dependencies from package.json and update `pnpm-lock.yaml`.
#[must_use]
pub struct InstallWithLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub existing_lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub lockfile_importer_id: &'a str,
    pub workspace_packages: &'a WorkspacePackages,
    pub dependency_groups: DependencyGroupList,
}

#[derive(Clone)]
struct ResolvedPackage {
    version: ResolvedDependencyVersion,
}

impl<'a, DependencyGroupList> InstallWithLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) {
        let InstallWithLockfile {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            existing_lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            dependency_groups,
        } = self;

        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();
        let package_snapshots = DashMap::<DependencyPath, PackageSnapshot>::new();
        let mut resolved_direct_dependencies = HashMap::<(String, String), ResolvedPackage>::new();

        for (name, version_range) in manifest.dependencies(dependency_groups.iter().copied()) {
            let key = (name.to_string(), version_range.to_string());
            if resolved_direct_dependencies.contains_key(&key) {
                continue;
            }

            if let Some(resolved_package) =
                resolve_link_dependency(config, manifest.path(), name, version_range)
            {
                resolved_direct_dependencies.insert(key, resolved_package);
                continue;
            }

            if let Some(workspace_package) =
                resolve_workspace_dependency(workspace_packages, name, version_range)
            {
                let symlink_path = config.modules_dir.join(name);
                symlink_package(&workspace_package.root_dir, &symlink_path)
                    .expect("symlink workspace package");

                let project_dir = manifest.path().parent().unwrap_or_else(|| Path::new("."));
                let relative = to_relative_path(project_dir, &workspace_package.root_dir);
                let resolved_package = ResolvedPackage {
                    version: ResolvedDependencyVersion::Link(format!(
                        "link:{}",
                        relative.replace('\\', "/")
                    )),
                };
                resolved_direct_dependencies.insert(key, resolved_package);
                continue;
            }

            let resolved_package = Self::install_and_snapshot_package(
                tarball_mem_cache,
                resolved_packages,
                http_client,
                config,
                &package_snapshots,
                &config.modules_dir,
                name,
                version_range,
            )
            .await;

            resolved_direct_dependencies.insert(key, resolved_package);
        }

        let project_snapshot = Self::build_project_snapshot(
            manifest,
            dependency_groups.iter().copied(),
            &resolved_direct_dependencies,
        );

        let mut packages = existing_lockfile
            .and_then(|lockfile| lockfile.packages.as_ref())
            .cloned()
            .unwrap_or_default();
        packages.extend(package_snapshots.into_iter());

        let project_snapshot =
            merge_project_snapshot(existing_lockfile, lockfile_importer_id, project_snapshot);
        let project_snapshot =
            ensure_workspace_importers(project_snapshot, lockfile_dir, workspace_packages);
        let runtime_lockfile_config =
            collect_runtime_lockfile_config(config, manifest, lockfile_dir);

        let lockfile = Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: Some(runtime_lockfile_config.settings),
            never_built_dependencies: None,
            ignored_optional_dependencies: existing_lockfile
                .and_then(|lockfile| lockfile.ignored_optional_dependencies.clone()),
            overrides: runtime_lockfile_config.overrides,
            package_extensions_checksum: runtime_lockfile_config.package_extensions_checksum,
            patched_dependencies: existing_lockfile
                .and_then(|lockfile| lockfile.patched_dependencies.clone()),
            pnpmfile_checksum: runtime_lockfile_config.pnpmfile_checksum,
            catalogs: existing_lockfile.and_then(|lockfile| lockfile.catalogs.clone()),
            time: existing_lockfile.and_then(|lockfile| lockfile.time.clone()),
            project_snapshot,
            packages: (!packages.is_empty()).then_some(packages),
        };

        lockfile.save_to_dir(lockfile_dir).expect("save lockfile");
    }

    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn install_and_snapshot_package(
        tarball_mem_cache: &MemCache,
        resolved_packages: &ResolvedPackages,
        http_client: &ThrottledClient,
        config: &'static Npmrc,
        package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
        node_modules_dir: &Path,
        name: &str,
        version_range: &str,
    ) -> ResolvedPackage {
        let package_version = InstallPackageFromRegistry {
            tarball_mem_cache,
            http_client,
            config,
            node_modules_dir,
            name,
            version_range,
        }
        .run::<Version>()
        .await
        .expect("install package from registry");

        let ver_peer = Self::to_pkg_ver_peer(&package_version);
        let resolved_version = if package_version.name == name {
            ResolvedDependencyVersion::PkgVerPeer(ver_peer.clone())
        } else {
            ResolvedDependencyVersion::PkgNameVerPeer(PkgNameVerPeer::new(
                Self::parse_pkg_name(&package_version.name),
                ver_peer.clone(),
            ))
        };
        let dependency_path = Self::to_dependency_path(&package_version);
        let virtual_store_name = package_version.to_virtual_store_name();

        if resolved_packages.insert(virtual_store_name.clone()) {
            let virtual_node_modules_dir =
                config.virtual_store_dir.join(virtual_store_name).join("node_modules");

            let dependencies = package_version
                .dependencies(config.auto_install_peers)
                .map(|(name, version_range)| (name.to_string(), version_range.to_string()))
                .collect::<Vec<_>>();

            let mut snapshot_dependencies = HashMap::new();
            for (dependency_name, dependency_version_range) in dependencies {
                let resolved_dependency = Self::install_and_snapshot_package(
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    package_snapshots,
                    &virtual_node_modules_dir,
                    &dependency_name,
                    &dependency_version_range,
                )
                .await;

                snapshot_dependencies.insert(
                    Self::parse_pkg_name(&dependency_name),
                    match resolved_dependency.version {
                        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
                            PackageSnapshotDependency::PkgVerPeer(ver_peer)
                        }
                        ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
                            PackageSnapshotDependency::PkgNameVerPeer(name_ver_peer)
                        }
                        ResolvedDependencyVersion::Link(_) => {
                            panic!("workspace links are not supported in transitive dependencies")
                        }
                    },
                );
            }

            let package_snapshot =
                Self::to_package_snapshot(config, &package_version, snapshot_dependencies);
            package_snapshots.insert(dependency_path, package_snapshot);
        }

        ResolvedPackage { version: resolved_version }
    }

    fn build_project_snapshot(
        manifest: &PackageManifest,
        dependency_groups: impl IntoIterator<Item = DependencyGroup>,
        resolved_direct_dependencies: &HashMap<(String, String), ResolvedPackage>,
    ) -> ProjectSnapshot {
        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();

        let mut specifiers = HashMap::new();
        let mut dependencies = None;
        let mut optional_dependencies = None;
        let mut dev_dependencies = None;

        for group in dependency_groups {
            let mut map = ResolvedDependencyMap::new();
            for (name, specifier) in manifest.dependencies([group]) {
                let key = (name.to_string(), specifier.to_string());
                if let Some(resolved_dependency) = resolved_direct_dependencies.get(&key) {
                    map.insert(
                        Self::parse_pkg_name(name),
                        ResolvedDependencySpec {
                            specifier: specifier.to_string(),
                            version: resolved_dependency.version.clone(),
                        },
                    );
                    specifiers.insert(name.to_string(), specifier.to_string());
                }
            }

            if map.is_empty() {
                continue;
            }

            match group {
                DependencyGroup::Prod => dependencies = Some(map),
                DependencyGroup::Optional => optional_dependencies = Some(map),
                DependencyGroup::Dev => dev_dependencies = Some(map),
                DependencyGroup::Peer => {}
            }
        }

        ProjectSnapshot {
            specifiers: (!specifiers.is_empty()).then_some(specifiers),
            dependencies,
            optional_dependencies,
            dev_dependencies,
            dependencies_meta: None,
            publish_directory: None,
        }
    }

    fn to_dependency_path(package_version: &PackageVersion) -> DependencyPath {
        let package_specifier = PkgNameVerPeer::new(
            Self::parse_pkg_name(package_version.name.as_str()),
            Self::to_pkg_ver_peer(package_version),
        );
        DependencyPath { custom_registry: None, package_specifier }
    }

    fn to_pkg_ver_peer(package_version: &PackageVersion) -> PkgVerPeer {
        package_version.version.to_string().parse().expect("package version is always valid semver")
    }

    fn parse_pkg_name(package_name: &str) -> PkgName {
        package_name.parse().expect("package name from npm registry is valid")
    }

    fn to_package_snapshot(
        config: &'static Npmrc,
        package_version: &PackageVersion,
        dependencies: HashMap<PkgName, PackageSnapshotDependency>,
    ) -> PackageSnapshot {
        let integrity =
            package_version.dist.integrity.clone().expect("registry package has integrity field");
        let package_id = format!("{}@{}", package_version.name, package_version.version);
        let requires_build = config
            .store_dir
            .read_index_file(&integrity, &package_id)
            .and_then(|index| index.requires_build)
            .unwrap_or(false);

        let resolution = if config.lockfile_include_tarball_url {
            LockfileResolution::Tarball(TarballResolution {
                tarball: package_version.as_tarball_url().to_string(),
                integrity: Some(integrity),
            })
        } else {
            LockfileResolution::Registry(RegistryResolution { integrity })
        };

        PackageSnapshot {
            resolution,
            id: None,
            name: None,
            version: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: None,
            has_bin: package_version.has_bin().then_some(true),
            prepare: None,
            requires_build: requires_build.then_some(true),
            bundled_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            dependencies: (!dependencies.is_empty()).then_some(dependencies),
            optional_dependencies: None,
            transitive_peer_dependencies: None,
            dev: None,
            optional: None,
        }
    }
}

fn merge_project_snapshot(
    existing_lockfile: Option<&Lockfile>,
    lockfile_importer_id: &str,
    project_snapshot: ProjectSnapshot,
) -> RootProjectSnapshot {
    match existing_lockfile.map(|lockfile| &lockfile.project_snapshot) {
        Some(RootProjectSnapshot::Multi(snapshot)) => {
            let mut importers = snapshot.importers.clone();
            importers.insert(lockfile_importer_id.to_string(), project_snapshot);
            RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
        }
        Some(RootProjectSnapshot::Single(existing_snapshot)) => {
            if lockfile_importer_id == "." {
                RootProjectSnapshot::Single(project_snapshot)
            } else {
                let mut importers = HashMap::new();
                importers.insert(".".to_string(), existing_snapshot.clone());
                importers.insert(lockfile_importer_id.to_string(), project_snapshot);
                RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
            }
        }
        None => {
            if lockfile_importer_id == "." {
                RootProjectSnapshot::Single(project_snapshot)
            } else {
                let mut importers = HashMap::new();
                importers.insert(lockfile_importer_id.to_string(), project_snapshot);
                RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
            }
        }
    }
}

fn ensure_workspace_importers(
    project_snapshot: RootProjectSnapshot,
    lockfile_dir: &Path,
    workspace_packages: &WorkspacePackages,
) -> RootProjectSnapshot {
    if workspace_packages.is_empty() {
        return project_snapshot;
    }

    let mut importers = match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => HashMap::from([(".".to_string(), snapshot)]),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers,
    };

    for info in workspace_packages.values() {
        let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
        importers.entry(importer_id).or_insert_with(ProjectSnapshot::default);
    }

    RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
}

fn to_lockfile_importer_id(lockfile_dir: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(lockfile_dir) else {
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

fn to_relative_path(from: &Path, to: &Path) -> String {
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();

    let common_len = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(left, right)| left == right)
        .count();

    if common_len == 0 {
        return to.to_string_lossy().into_owned();
    }

    let mut relative_parts = Vec::<String>::new();
    for _ in common_len..from_components.len() {
        relative_parts.push("..".to_string());
    }
    for component in to_components.iter().skip(common_len) {
        relative_parts.push(component.as_os_str().to_string_lossy().into_owned());
    }

    if relative_parts.is_empty() { ".".to_string() } else { relative_parts.join("/") }
}

fn resolve_link_dependency(
    config: &'static Npmrc,
    manifest_path: &Path,
    name: &str,
    version_range: &str,
) -> Option<ResolvedPackage> {
    let link_target = version_range.strip_prefix("link:")?;
    let project_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let local_dep_path = normalize_local_dependency_path(project_dir, link_target);
    let symlink_path = config.modules_dir.join(name);
    symlink_package(&local_dep_path, &symlink_path).expect("symlink local link dependency");
    Some(ResolvedPackage {
        version: ResolvedDependencyVersion::Link(format!(
            "link:{}",
            link_target.replace('\\', "/")
        )),
    })
}

fn normalize_local_dependency_path(project_dir: &Path, target: &str) -> PathBuf {
    let candidate = Path::new(target);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    project_dir.join(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspacePackageInfo;

    fn empty_lockfile(project_snapshot: RootProjectSnapshot) -> Lockfile {
        Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: None,
            never_built_dependencies: None,
            ignored_optional_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            project_snapshot,
            packages: None,
        }
    }

    #[test]
    fn merge_without_existing_lockfile_non_workspace() {
        let received = merge_project_snapshot(None, ".", ProjectSnapshot::default());
        assert!(matches!(received, RootProjectSnapshot::Single(_)));
    }

    #[test]
    fn merge_without_existing_lockfile_workspace_importer() {
        let received = merge_project_snapshot(None, "packages/app", ProjectSnapshot::default());
        let RootProjectSnapshot::Multi(snapshot) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(snapshot.importers.contains_key("packages/app"));
    }

    #[test]
    fn merge_with_existing_multi_lockfile_keeps_other_importers() {
        let mut importers = HashMap::new();
        importers.insert("packages/old".to_string(), ProjectSnapshot::default());
        let existing =
            empty_lockfile(RootProjectSnapshot::Multi(MultiProjectSnapshot { importers }));

        let received =
            merge_project_snapshot(Some(&existing), "packages/new", ProjectSnapshot::default());
        let RootProjectSnapshot::Multi(snapshot) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(snapshot.importers.contains_key("packages/old"));
        assert!(snapshot.importers.contains_key("packages/new"));
    }

    #[test]
    fn relative_path_from_project_to_workspace_package() {
        let from = Path::new("/repo/packages/app");
        let to = Path::new("/repo/packages/lib");
        assert_eq!(to_relative_path(from, to), "../lib".to_string());
    }

    #[test]
    fn ensure_workspace_importers_adds_missing_packages() {
        let mut workspace_packages = WorkspacePackages::new();
        workspace_packages.insert(
            "@repo/app".to_string(),
            WorkspacePackageInfo {
                root_dir: Path::new("/repo/packages/app").to_path_buf(),
                version: "1.0.0".to_string(),
            },
        );
        workspace_packages.insert(
            "@repo/lib".to_string(),
            WorkspacePackageInfo {
                root_dir: Path::new("/repo/packages/lib").to_path_buf(),
                version: "1.0.0".to_string(),
            },
        );

        let mut importers = HashMap::new();
        importers.insert("packages/app".to_string(), ProjectSnapshot::default());
        let snapshot = RootProjectSnapshot::Multi(MultiProjectSnapshot { importers });

        let received =
            ensure_workspace_importers(snapshot, Path::new("/repo"), &workspace_packages);
        let RootProjectSnapshot::Multi(received) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(received.importers.contains_key("packages/app"));
        assert!(received.importers.contains_key("packages/lib"));
    }
}
