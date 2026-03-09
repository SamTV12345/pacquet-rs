use crate::{
    InstallPackageBySnapshot, ResolvedPackages, create_symlink_layout, package_dependency_map,
    symlink_package,
};
use futures_util::stream::{self, StreamExt};
use pacquet_lockfile::{DependencyPath, PackageSnapshot, PkgNameVerPeer};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::atomic::{AtomicBool, Ordering},
};

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub resolved_packages: Option<&'a ResolvedPackages>,
    pub offline: bool,
    pub force: bool,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) {
        let CreateVirtualStore { http_client, config, packages, resolved_packages, offline, force } =
            self;

        let packages = if let Some(packages) = packages {
            packages
        } else {
            return;
        };

        let should_link_layout = AtomicBool::new(false);
        let install_concurrency = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get().clamp(4, 32))
            .unwrap_or(16);

        stream::iter(packages.iter())
            .for_each_concurrent(install_concurrency, |(dependency_path, package_snapshot)| async {
                if let Some(seen) = resolved_packages {
                    let virtual_store_name =
                        dependency_path.package_specifier.to_virtual_store_name();
                    if !seen.insert(virtual_store_name) {
                        return;
                    }
                }
                let imported = InstallPackageBySnapshot {
                    http_client,
                    config,
                    dependency_path,
                    package_snapshot,
                    offline,
                    force,
                }
                .run()
                .await
                .unwrap(); // TODO: properly propagate this error
                if imported {
                    should_link_layout.store(true, Ordering::Relaxed);
                }
            })
            .await;

        let needs_layout_pass = should_link_layout.load(Ordering::Relaxed)
            || virtual_dependency_layout_missing(config, packages)
            || hoisted_links_missing(config, packages);
        if !needs_layout_pass {
            return;
        }

        // Second pass: after all virtual package directories exist, wire dependency links.
        for (dependency_path, package_snapshot) in packages {
            let dependencies = package_dependency_map(package_snapshot);
            if dependencies.is_empty() {
                continue;
            }
            let virtual_node_modules_dir = config
                .virtual_store_dir
                .join(dependency_path.package_specifier.to_virtual_store_name())
                .join("node_modules");
            if dependency_layout_is_present(&dependencies, &virtual_node_modules_dir) {
                continue;
            }
            create_symlink_layout(
                &dependencies,
                &config.virtual_store_dir,
                &virtual_node_modules_dir,
            );
        }

        hoist_virtual_store_dependencies(config, packages);
    }
}

fn virtual_dependency_layout_missing(
    config: &Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> bool {
    packages.iter().any(|(dependency_path, package_snapshot)| {
        let dependencies = package_dependency_map(package_snapshot);
        if dependencies.is_empty() {
            return false;
        }
        let virtual_node_modules_dir = config
            .virtual_store_dir
            .join(dependency_path.package_specifier.to_virtual_store_name())
            .join("node_modules");
        !dependency_layout_is_present(&dependencies, &virtual_node_modules_dir)
    })
}

fn hoisted_links_missing(
    config: &Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> bool {
    if !config.hoist && !config.shamefully_hoist {
        return false;
    }

    let hoisted_modules_dir = config.virtual_store_dir.join("node_modules");
    select_hoisted_packages(packages).into_iter().any(|(name, package_specifier)| {
        let target = config
            .virtual_store_dir
            .join(package_specifier.to_virtual_store_name())
            .join("node_modules")
            .join(&name);
        target.exists() && !hoisted_modules_dir.join(name).exists()
    })
}

fn hoist_virtual_store_dependencies(
    config: &Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) {
    // Matches pnpm's semistrict defaults: hoist into node_modules/.pnpm/node_modules.
    if !config.hoist && !config.shamefully_hoist {
        return;
    }

    let hoisted_modules_dir = config.virtual_store_dir.join("node_modules");
    fs::create_dir_all(&hoisted_modules_dir)
        .unwrap_or_else(|error| panic!("create hoisted modules directory should succeed: {error}"));

    for (name, package_specifier) in select_hoisted_packages(packages) {
        let target = config
            .virtual_store_dir
            .join(package_specifier.to_virtual_store_name())
            .join("node_modules")
            .join(&name);
        if !target.exists() {
            continue;
        }
        let symlink_path = hoisted_modules_dir.join(&name);
        if symlink_path.exists() {
            continue;
        }
        symlink_package(&target, &symlink_path)
            .unwrap_or_else(|error| panic!("hoisted dependency symlink should succeed: {error}"));
    }
}

fn select_hoisted_packages(
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> BTreeMap<String, PkgNameVerPeer> {
    let mut selected = BTreeMap::<String, PkgNameVerPeer>::new();
    for dependency_path in packages.keys() {
        let package_specifier = dependency_path.package_specifier.clone();
        let package_name = package_specifier.name.to_string();
        let should_replace = match selected.get(&package_name) {
            None => true,
            Some(current) => {
                let new_version = package_specifier.suffix.version();
                let current_version = current.suffix.version();
                if new_version != current_version {
                    new_version > current_version
                } else {
                    let new_no_peer_suffix = package_specifier.suffix.peer().is_empty();
                    let current_no_peer_suffix = current.suffix.peer().is_empty();
                    if new_no_peer_suffix != current_no_peer_suffix {
                        new_no_peer_suffix
                    } else {
                        package_specifier.to_virtual_store_name() < current.to_virtual_store_name()
                    }
                }
            }
        };
        if should_replace {
            selected.insert(package_name, package_specifier);
        }
    }
    selected
}

fn dependency_layout_is_present(
    dependencies: &HashMap<pacquet_lockfile::PkgName, pacquet_lockfile::PackageSnapshotDependency>,
    virtual_node_modules_dir: &std::path::Path,
) -> bool {
    dependencies.keys().all(|alias| virtual_node_modules_dir.join(alias.to_string()).exists())
}
