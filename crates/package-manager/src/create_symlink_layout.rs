use crate::link_package;
use pacquet_lockfile::{PackageSnapshotDependency, PkgName, PkgNameVerPeer};
use std::{collections::HashMap, fs, path::Path};
#[cfg(windows)]
use std::{thread, time::Duration};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// **NOTE:** `virtual_node_modules_dir` is assumed to already exist.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, PackageSnapshotDependency>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
    symlink: bool,
) {
    for (alias, spec) in dependencies {
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
        let symlink_target =
            virtual_root.join(virtual_store_name).join("node_modules").join(target_package_name);
        let symlink_path = virtual_node_modules_dir.join(alias_str);
        if path_points_to_target(&symlink_target, &symlink_path) {
            continue;
        }
        let symlink_result = link_package(symlink, &symlink_target, &symlink_path);
        #[cfg(windows)]
        let symlink_result = if symlink_result.is_err() {
            let mut retry = symlink_result;
            for _ in 0..2 {
                thread::sleep(Duration::from_millis(20));
                retry = link_package(symlink, &symlink_target, &symlink_path);
                if retry.is_ok() {
                    break;
                }
            }
            retry
        } else {
            symlink_result
        };
        symlink_result.unwrap_or_else(|error| panic!("symlink pkg should succeed: {error}"));
    }
}

fn path_points_to_target(target: &Path, link: &Path) -> bool {
    if !link.exists() {
        return false;
    }
    fs::canonicalize(link)
        .ok()
        .zip(fs::canonicalize(target).ok())
        .is_some_and(|(existing, wanted)| existing == wanted)
}
