use crate::{
    CreateVirtualStore, ResolvedPackages, SymlinkDirectDependencies, package_dependency_map,
};
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer,
    ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencyVersion,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::{NodeLinker, Npmrc};
use pacquet_package_manifest::DependencyGroup;
use std::collections::{HashMap, HashSet};

/// This subroutine installs dependencies from a frozen lockfile.
///
/// **Brief overview:**
/// * Iterate over each package in [`Self::packages`].
/// * Fetch a tarball of each package.
/// * Extract each tarball into the store directory.
/// * Import (by reflink, hardlink, or copy) the files from the store dir to each `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in each `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at each `node_modules/{name}`.
#[must_use]
pub struct InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub http_client: &'a ThrottledClient,
    pub resolved_packages: &'a ResolvedPackages,
    pub config: &'static Npmrc,
    pub project_snapshot: &'a ProjectSnapshot,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub lockfile_dir: &'a std::path::Path,
    pub dependency_groups: DependencyGroupList,
    pub offline: bool,
    pub force: bool,
}

impl<'a, DependencyGroupList> InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) {
        let InstallFrozenLockfile {
            http_client,
            resolved_packages,
            config,
            project_snapshot,
            packages,
            lockfile_dir,
            dependency_groups,
            offline,
            force,
        } = self;
        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();

        // TODO: check if the lockfile is out-of-date

        if force
            || !importer_dependencies_ready(
                config,
                project_snapshot,
                packages,
                dependency_groups.iter().copied(),
            )
        {
            CreateVirtualStore {
                http_client,
                config,
                packages,
                lockfile_dir,
                resolved_packages: Some(resolved_packages),
                offline,
                force,
            }
            .run()
            .await;
        }

        let deduped_project_snapshot =
            dedupe_project_snapshot(project_snapshot, packages, config.dedupe_peer_dependents);
        SymlinkDirectDependencies {
            config,
            project_snapshot: &deduped_project_snapshot,
            packages,
            dependency_groups,
        }
        .run();
    }
}

fn dedupe_project_snapshot(
    project_snapshot: &ProjectSnapshot,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    dedupe_peer_dependents: bool,
) -> ProjectSnapshot {
    let Some(packages) = packages else {
        return project_snapshot.clone();
    };
    if !dedupe_peer_dependents {
        return project_snapshot.clone();
    }

    let mut snapshot = project_snapshot.clone();
    dedupe_resolved_dependency_map(snapshot.dependencies.as_mut(), packages);
    dedupe_resolved_dependency_map(snapshot.optional_dependencies.as_mut(), packages);
    dedupe_resolved_dependency_map(snapshot.dev_dependencies.as_mut(), packages);
    snapshot
}

fn dedupe_resolved_dependency_map(
    map: Option<&mut ResolvedDependencyMap>,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) {
    let Some(map) = map else {
        return;
    };

    for (alias, spec) in map.iter_mut() {
        let Some(candidate_path) = resolved_dependency_to_path(alias, &spec.version) else {
            continue;
        };
        let Some((resolved_path, _)) = resolve_package_snapshot_deduped(packages, &candidate_path)
        else {
            continue;
        };
        spec.version = resolved_path_to_version(alias, &resolved_path);
    }
}

fn resolved_dependency_to_path(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
) -> Option<DependencyPath> {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => Some(DependencyPath::registry(
            None,
            PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        )),
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            Some(DependencyPath::registry(None, specifier.clone()))
        }
        ResolvedDependencyVersion::Link(_) => None,
    }
}

fn resolved_path_to_version(
    alias: &PkgName,
    dependency_path: &DependencyPath,
) -> ResolvedDependencyVersion {
    let specifier = dependency_path
        .package_specifier
        .registry_specifier()
        .unwrap_or_else(|| panic!("resolved path to version only supports registry dependencies"));
    if &specifier.name == alias {
        ResolvedDependencyVersion::PkgVerPeer(specifier.suffix.clone())
    } else {
        ResolvedDependencyVersion::PkgNameVerPeer(specifier.clone())
    }
}

fn importer_dependencies_ready(
    config: &Npmrc,
    project_snapshot: &ProjectSnapshot,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    dependency_groups: impl IntoIterator<Item = DependencyGroup>,
) -> bool {
    if !config.virtual_store_dir.is_dir() {
        return false;
    }

    let direct_dependencies =
        project_snapshot.dependencies_by_groups(dependency_groups).collect::<Vec<_>>();
    let should_expect_direct_links =
        config.symlink || matches!(config.node_linker, NodeLinker::Hoisted);
    if !direct_dependencies.iter().all(|(alias, spec)| {
        if !config.disable_relink_local_dir_deps
            && matches!(&spec.version, ResolvedDependencyVersion::Link(link) if link.starts_with("file:"))
        {
            return false;
        }
        let is_link_dependency = matches!(spec.version, ResolvedDependencyVersion::Link(_));
        if should_expect_direct_links && !is_link_dependency {
            let direct_link = config.modules_dir.join(alias.to_string());
            if !direct_link.exists() {
                return false;
            }
        }
        direct_dependency_virtual_store_location(alias, &spec.version, packages).is_none_or(
            |(_, virtual_store_name, package_name)| {
                let package_in_virtual_store = config
                    .virtual_store_dir
                    .join(virtual_store_name)
                    .join("node_modules")
                    .join(package_name);
                package_in_virtual_store.exists()
            },
        )
    }) {
        return false;
    }

    let Some(packages) = packages else {
        return true;
    };

    let mut queue = direct_dependencies
        .iter()
        .filter_map(|(alias, spec)| direct_dependency_path(alias, &spec.version, packages))
        .collect::<Vec<_>>();
    let mut seen = HashSet::<DependencyPath>::new();

    while let Some(candidate_path) = queue.pop() {
        let Some((resolved_dependency_path, package_snapshot)) =
            resolve_package_snapshot(packages, &candidate_path)
        else {
            return false;
        };

        if !seen.insert(resolved_dependency_path.clone()) {
            continue;
        }

        let virtual_store_name = resolved_dependency_path.to_virtual_store_name();
        let package_name = resolved_dependency_path.package_name().to_string();
        let package_in_virtual_store = config
            .virtual_store_dir
            .join(&virtual_store_name)
            .join("node_modules")
            .join(&package_name);
        if !package_in_virtual_store.exists() {
            return false;
        }

        let virtual_node_modules =
            config.virtual_store_dir.join(&virtual_store_name).join("node_modules");
        let dependencies = package_dependency_map(package_snapshot);
        if !dependency_links_ready(&dependencies, &virtual_node_modules) {
            return false;
        }

        for (alias, dependency_spec) in dependencies {
            let dependency_path =
                dependency_path_from_snapshot_dependency(&alias, &dependency_spec);
            if dependency_path.as_ref().is_some_and(|dependency_path| {
                resolve_package_snapshot_deduped(packages, dependency_path).is_none()
            }) {
                return false;
            }
            if let Some(dependency_path) = dependency_path {
                queue.push(dependency_path);
            }
        }
    }

    true
}

fn dependency_links_ready(
    dependencies: &HashMap<PkgName, PackageSnapshotDependency>,
    virtual_node_modules: &std::path::Path,
) -> bool {
    dependencies.keys().all(|alias| virtual_node_modules.join(alias.to_string()).exists())
}

fn dependency_path_from_snapshot_dependency(
    alias: &PkgName,
    dependency_spec: &PackageSnapshotDependency,
) -> Option<DependencyPath> {
    match dependency_spec {
        PackageSnapshotDependency::PkgVerPeer(ver_peer) => Some(DependencyPath::registry(
            None,
            PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        )),
        PackageSnapshotDependency::PkgNameVerPeer(package_specifier) => {
            Some(DependencyPath::registry(None, package_specifier.clone()))
        }
        PackageSnapshotDependency::DependencyPath(path) => Some(path.clone()),
        PackageSnapshotDependency::Link(link) => {
            if link.starts_with("file:") {
                Some(DependencyPath::local_file(alias.clone(), link.clone()))
            } else {
                None
            }
        }
    }
}

fn resolve_package_snapshot<'a>(
    packages: &'a HashMap<DependencyPath, PackageSnapshot>,
    candidate_path: &DependencyPath,
) -> Option<(DependencyPath, &'a PackageSnapshot)> {
    if let Some(snapshot) = packages.get(candidate_path) {
        return Some((candidate_path.clone(), snapshot));
    }
    packages
        .iter()
        .find(|(dependency_path, _)| {
            dependency_path.package_specifier == candidate_path.package_specifier
        })
        .map(|(dependency_path, snapshot)| (dependency_path.clone(), snapshot))
}

fn resolve_package_snapshot_deduped<'a>(
    packages: &'a HashMap<DependencyPath, PackageSnapshot>,
    candidate_path: &DependencyPath,
) -> Option<(DependencyPath, &'a PackageSnapshot)> {
    let (resolved_path, resolved_snapshot) = resolve_package_snapshot(packages, candidate_path)?;
    let mut best_path = resolved_path.clone();
    let mut best_snapshot = resolved_snapshot;

    for (other_path, other_snapshot) in packages {
        if *other_path == resolved_path {
            continue;
        }
        if !same_base_package(&resolved_path, other_path) {
            continue;
        }
        if !is_compatible_and_has_more_deps(other_snapshot, best_snapshot) {
            continue;
        }
        let better = dependency_score(other_snapshot) > dependency_score(best_snapshot)
            || (dependency_score(other_snapshot) == dependency_score(best_snapshot)
                && other_path.to_string() < best_path.to_string());
        if better {
            best_path = other_path.clone();
            best_snapshot = other_snapshot;
        }
    }

    Some((best_path, best_snapshot))
}

fn same_base_package(left: &DependencyPath, right: &DependencyPath) -> bool {
    match (
        left.package_specifier.registry_specifier(),
        right.package_specifier.registry_specifier(),
    ) {
        (Some(left_specifier), Some(right_specifier)) => {
            left.custom_registry == right.custom_registry
                && left_specifier.name == right_specifier.name
                && left_specifier.suffix.version() == right_specifier.suffix.version()
        }
        _ => left == right,
    }
}

fn dependency_score(snapshot: &PackageSnapshot) -> usize {
    let dependency_count = snapshot.dependencies.as_ref().map_or(0, HashMap::len);
    let transitive_peer_count = snapshot.transitive_peer_dependencies.as_ref().map_or(0, Vec::len);
    dependency_count + transitive_peer_count
}

fn is_compatible_and_has_more_deps(candidate: &PackageSnapshot, current: &PackageSnapshot) -> bool {
    if dependency_score(candidate) < dependency_score(current) {
        return false;
    }

    let candidate_deps = candidate.dependencies.as_ref();
    let current_deps = current.dependencies.as_ref();
    if let Some(current_deps) = current_deps {
        let Some(candidate_deps) = candidate_deps else {
            return false;
        };
        if !current_deps.iter().all(|(alias, dep)| {
            candidate_deps.get(alias).is_some_and(|candidate_dep| candidate_dep == dep)
        }) {
            return false;
        }
    }

    let candidate_peers = candidate
        .transitive_peer_dependencies
        .as_ref()
        .map_or_else(HashSet::new, |peers| peers.iter().cloned().collect::<HashSet<_>>());
    let current_peers = current
        .transitive_peer_dependencies
        .as_ref()
        .map_or_else(HashSet::new, |peers| peers.iter().cloned().collect::<HashSet<_>>());

    current_peers.is_subset(&candidate_peers)
}

fn direct_dependency_path(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> Option<DependencyPath> {
    let dependency_path = match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            DependencyPath::registry(None, PkgNameVerPeer::new(alias.clone(), ver_peer.clone()))
        }
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            DependencyPath::registry(None, specifier.clone())
        }
        ResolvedDependencyVersion::Link(link) => {
            if !link.starts_with("file:") {
                return None;
            }
            DependencyPath::local_file(alias.clone(), link.clone())
        }
    };
    resolve_package_snapshot(packages, &dependency_path).map(|(resolved_path, _)| resolved_path)
}

fn direct_dependency_virtual_store_location(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
) -> Option<(String, String, String)> {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            let specifier = PkgNameVerPeer::new(alias.clone(), ver_peer.clone());
            Some((alias.to_string(), specifier.to_virtual_store_name(), alias.to_string()))
        }
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            Some((alias.to_string(), specifier.to_virtual_store_name(), specifier.name.to_string()))
        }
        ResolvedDependencyVersion::Link(link) => packages.and_then(|packages| {
            if !link.starts_with("file:") {
                return None;
            }
            let dependency_path = DependencyPath::local_file(alias.clone(), link.clone());
            resolve_package_snapshot(packages, &dependency_path).map(|(resolved_path, _)| {
                (
                    alias.to_string(),
                    resolved_path.to_virtual_store_name(),
                    resolved_path.package_name().to_string(),
                )
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::{
        LockfileResolution, PkgVerPeer, ResolvedDependencySpec, TarballResolution,
    };

    fn dummy_snapshot_with_dependencies(
        dependencies: Option<HashMap<PkgName, PackageSnapshotDependency>>,
        transitive_peers: Option<Vec<String>>,
    ) -> PackageSnapshot {
        PackageSnapshot {
            resolution: LockfileResolution::Tarball(TarballResolution {
                tarball: "file:dummy.tgz".to_string(),
                integrity: None,
            }),
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
            dependencies,
            optional_dependencies: None,
            transitive_peer_dependencies: transitive_peers,
            dev: None,
            optional: None,
        }
    }

    #[test]
    fn direct_dependency_virtual_store_location_for_alias() {
        let alias: PkgName = "@scope/dep".parse().expect("alias");
        let target = PkgNameVerPeer::new(
            "@scope/real".parse().expect("name"),
            "1.2.3".parse().expect("version"),
        );
        let received = direct_dependency_virtual_store_location(
            &alias,
            &ResolvedDependencyVersion::PkgNameVerPeer(target.clone()),
            None,
        )
        .expect("resolved location");
        assert_eq!(received.0, "@scope/dep");
        assert_eq!(received.1, target.to_virtual_store_name());
        assert_eq!(received.2, "@scope/real");
    }

    #[test]
    fn direct_dependency_virtual_store_location_for_link_is_none() {
        let alias: PkgName = "dep".parse().expect("alias");
        let received = direct_dependency_virtual_store_location(
            &alias,
            &ResolvedDependencyVersion::Link("link:../dep".to_string()),
            None,
        );
        assert!(received.is_none());
    }

    #[test]
    fn direct_dependency_virtual_store_location_for_regular_dep() {
        let alias: PkgName = "dep".parse().expect("alias");
        let version: PkgVerPeer = "1.0.0".parse().expect("version");
        let received = direct_dependency_virtual_store_location(
            &alias,
            &ResolvedDependencyVersion::PkgVerPeer(version.clone()),
            None,
        )
        .expect("resolved location");
        assert_eq!(received.0, "dep");
        assert_eq!(received.2, "dep");
        assert_eq!(received.1, PkgNameVerPeer::new(alias, version).to_virtual_store_name());
    }

    #[test]
    fn dedupe_project_snapshot_prefers_compatible_variant_with_more_deps() {
        let alias: PkgName = "foo".parse().expect("alias");
        let current_path: DependencyPath =
            "/foo@1.0.0(peer-a@1.0.0)".parse().expect("current dependency path");
        let better_path: DependencyPath =
            "/foo@1.0.0(peer-a@1.0.0)(peer-b@1.0.0)".parse().expect("better dependency path");

        let mut packages = HashMap::new();
        packages.insert(
            current_path.clone(),
            dummy_snapshot_with_dependencies(
                Some(HashMap::from([(
                    "bar".parse().expect("bar"),
                    PackageSnapshotDependency::PkgVerPeer("1.0.0".parse().expect("bar version")),
                )])),
                Some(vec!["peer-a".to_string()]),
            ),
        );
        packages.insert(
            better_path.clone(),
            dummy_snapshot_with_dependencies(
                Some(HashMap::from([
                    (
                        "bar".parse().expect("bar"),
                        PackageSnapshotDependency::PkgVerPeer(
                            "1.0.0".parse().expect("bar version"),
                        ),
                    ),
                    (
                        "baz".parse().expect("baz"),
                        PackageSnapshotDependency::PkgVerPeer(
                            "1.0.0".parse().expect("baz version"),
                        ),
                    ),
                ])),
                Some(vec!["peer-a".to_string(), "peer-b".to_string()]),
            ),
        );

        let mut dependencies = ResolvedDependencyMap::new();
        dependencies.insert(
            alias.clone(),
            ResolvedDependencySpec {
                specifier: "^1.0.0".to_string(),
                version: ResolvedDependencyVersion::PkgVerPeer(
                    current_path
                        .package_specifier
                        .registry_specifier()
                        .expect("registry specifier")
                        .suffix
                        .clone(),
                ),
            },
        );
        let snapshot =
            ProjectSnapshot { dependencies: Some(dependencies), ..ProjectSnapshot::default() };

        let deduped = dedupe_project_snapshot(&snapshot, Some(&packages), true);
        let resolved = deduped
            .dependencies
            .as_ref()
            .and_then(|deps| deps.get(&alias))
            .expect("resolved dependency");
        let ResolvedDependencyVersion::PkgVerPeer(ver_peer) = &resolved.version else {
            panic!("expected pkgverpeer");
        };
        assert_eq!(
            ver_peer.to_string(),
            better_path
                .package_specifier
                .registry_specifier()
                .expect("registry specifier")
                .suffix
                .to_string()
        );
    }

    #[test]
    fn dedupe_project_snapshot_keeps_original_when_disabled() {
        let alias: PkgName = "foo".parse().expect("alias");
        let current_path: DependencyPath =
            "/foo@1.0.0(peer-a@1.0.0)".parse().expect("current dependency path");
        let better_path: DependencyPath =
            "/foo@1.0.0(peer-a@1.0.0)(peer-b@1.0.0)".parse().expect("better dependency path");

        let mut packages = HashMap::new();
        packages.insert(current_path.clone(), dummy_snapshot_with_dependencies(None, None));
        packages.insert(better_path, dummy_snapshot_with_dependencies(None, None));

        let mut dependencies = ResolvedDependencyMap::new();
        dependencies.insert(
            alias.clone(),
            ResolvedDependencySpec {
                specifier: "^1.0.0".to_string(),
                version: ResolvedDependencyVersion::PkgVerPeer(
                    current_path
                        .package_specifier
                        .registry_specifier()
                        .expect("registry specifier")
                        .suffix
                        .clone(),
                ),
            },
        );
        let snapshot =
            ProjectSnapshot { dependencies: Some(dependencies), ..ProjectSnapshot::default() };

        let deduped = dedupe_project_snapshot(&snapshot, Some(&packages), false);
        let resolved = deduped
            .dependencies
            .as_ref()
            .and_then(|deps| deps.get(&alias))
            .expect("resolved dependency");
        let ResolvedDependencyVersion::PkgVerPeer(ver_peer) = &resolved.version else {
            panic!("expected pkgverpeer");
        };
        assert_eq!(
            ver_peer.to_string(),
            current_path
                .package_specifier
                .registry_specifier()
                .expect("registry specifier")
                .suffix
                .to_string()
        );
    }
}
