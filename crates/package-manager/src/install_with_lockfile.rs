use crate::{InstallPackageFromRegistry, ResolvedPackages};
use async_recursion::async_recursion;
use dashmap::DashMap;
use node_semver::Version;
use pacquet_lockfile::{
    ComVer, DependencyPath, Lockfile, LockfileResolution, PackageSnapshot,
    PackageSnapshotDependency, PkgName, PkgNameVerPeer, PkgVerPeer, ProjectSnapshot,
    RegistryResolution, ResolvedDependencyMap, ResolvedDependencySpec, RootProjectSnapshot,
    TarballResolution,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::PackageVersion;
use pacquet_tarball::MemCache;
use std::{collections::HashMap, path::Path};

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
    pub dependency_groups: DependencyGroupList,
}

#[derive(Clone)]
struct ResolvedPackage {
    ver_peer: PkgVerPeer,
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

        let lockfile = Lockfile {
            lockfile_version: ComVer::new(6, 0).try_into().expect("lockfile version compatible"),
            settings: None,
            never_built_dependencies: None,
            overrides: None,
            project_snapshot: RootProjectSnapshot::Single(project_snapshot),
            packages: Some(package_snapshots.into_iter().collect()),
        };

        let lockfile_dir = manifest.path().parent().unwrap_or(Path::new("."));
        lockfile.save_to_dir(lockfile_dir).expect("save lockfile");
    }

    #[async_recursion]
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
                    PackageSnapshotDependency::PkgVerPeer(resolved_dependency.ver_peer),
                );
            }

            let package_snapshot =
                Self::to_package_snapshot(config, &package_version, snapshot_dependencies);
            package_snapshots.insert(dependency_path, package_snapshot);
        }

        ResolvedPackage { ver_peer }
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
                            version: resolved_dependency.ver_peer.clone(),
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
            has_bin: None,
            prepare: None,
            requires_build: None,
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
