use crate::{
    CreateVirtualStore, ResolvedPackages, SymlinkDirectDependencies, package_dependency_map,
};
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer,
    ProjectSnapshot, ResolvedDependencyVersion,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
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
    pub dependency_groups: DependencyGroupList,
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
            dependency_groups,
        } = self;
        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();

        // TODO: check if the lockfile is out-of-date

        assert!(config.prefer_frozen_lockfile, "Non frozen lockfile is not yet supported");

        if !importer_dependencies_ready(
            config,
            project_snapshot,
            packages,
            dependency_groups.iter().copied(),
        ) {
            CreateVirtualStore {
                http_client,
                config,
                packages,
                resolved_packages: Some(resolved_packages),
            }
            .run()
            .await;
        }

        SymlinkDirectDependencies { config, project_snapshot, dependency_groups }.run();
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
    if !direct_dependencies.iter().all(|(alias, spec)| {
        let direct_link = config.modules_dir.join(alias.to_string());
        if !direct_link.exists() {
            return false;
        }
        direct_dependency_virtual_store_location(alias, &spec.version).is_none_or(
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

        let virtual_store_name = resolved_dependency_path.package_specifier.to_virtual_store_name();
        let package_name = resolved_dependency_path.package_specifier.name.to_string();
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
            if resolve_package_snapshot(packages, &dependency_path).is_none() {
                return false;
            }
            queue.push(dependency_path);
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
) -> DependencyPath {
    match dependency_spec {
        PackageSnapshotDependency::PkgVerPeer(ver_peer) => DependencyPath {
            custom_registry: None,
            package_specifier: PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        },
        PackageSnapshotDependency::PkgNameVerPeer(package_specifier) => {
            DependencyPath { custom_registry: None, package_specifier: package_specifier.clone() }
        }
        PackageSnapshotDependency::DependencyPath(path) => path.clone(),
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

fn direct_dependency_path(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> Option<DependencyPath> {
    let dependency_path = match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => DependencyPath {
            custom_registry: None,
            package_specifier: PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        },
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            DependencyPath { custom_registry: None, package_specifier: specifier.clone() }
        }
        ResolvedDependencyVersion::Link(_) => return None,
    };
    resolve_package_snapshot(packages, &dependency_path).map(|(resolved_path, _)| resolved_path)
}

fn direct_dependency_virtual_store_location(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
) -> Option<(String, String, String)> {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            let specifier = PkgNameVerPeer::new(alias.clone(), ver_peer.clone());
            Some((alias.to_string(), specifier.to_virtual_store_name(), alias.to_string()))
        }
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            Some((alias.to_string(), specifier.to_virtual_store_name(), specifier.name.to_string()))
        }
        ResolvedDependencyVersion::Link(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::PkgVerPeer;

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
        )
        .expect("resolved location");
        assert_eq!(received.0, "dep");
        assert_eq!(received.2, "dep");
        assert_eq!(received.1, PkgNameVerPeer::new(alias, version).to_virtual_store_name());
    }
}
