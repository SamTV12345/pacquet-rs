use crate::symlink_package;
use pacquet_lockfile::{PackageSnapshotDependency, PkgName, PkgNameVerPeer};
use rayon::prelude::*;
use std::{collections::HashMap, path::Path};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// **NOTE:** `virtual_node_modules_dir` is assumed to already exist.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, PackageSnapshotDependency>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
) {
    dependencies.par_iter().for_each(|(alias, spec)| {
        let (virtual_store_name, target_package_name) = match spec {
            PackageSnapshotDependency::PkgVerPeer(ver_peer) => {
                let package_specifier = PkgNameVerPeer::new(alias.clone(), ver_peer.clone()); // TODO: remove copying here
                (package_specifier.to_virtual_store_name(), alias.to_string())
            }
            PackageSnapshotDependency::PkgNameVerPeer(package_specifier) => {
                (package_specifier.to_virtual_store_name(), package_specifier.name.to_string())
            }
            PackageSnapshotDependency::DependencyPath(dependency_path) => (
                dependency_path.package_specifier.to_virtual_store_name(),
                dependency_path.package_specifier.name.to_string(),
            ),
        };
        let alias_str = alias.to_string();
        symlink_package(
            &virtual_root.join(virtual_store_name).join("node_modules").join(target_package_name),
            &virtual_node_modules_dir.join(alias_str),
        )
        .expect("symlink pkg successful"); // TODO: properly propagate this error
    });
}
