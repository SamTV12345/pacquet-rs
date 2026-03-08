use crate::{
    InstallPackageBySnapshot, create_symlink_layout, package_dependency_map, symlink_package,
};
use futures_util::future;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, PkgNameVerPeer};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pipe_trait::Pipe;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
};

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) {
        let CreateVirtualStore { http_client, config, packages } = self;

        let packages = if let Some(packages) = packages {
            packages
        } else {
            return;
        };

        packages
            .iter()
            .map(|(dependency_path, package_snapshot)| async move {
                InstallPackageBySnapshot { http_client, config, dependency_path, package_snapshot }
                    .run()
                    .await
                    .unwrap(); // TODO: properly propagate this error
            })
            .pipe(future::join_all)
            .await;

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
            create_symlink_layout(
                &dependencies,
                &config.virtual_store_dir,
                &virtual_node_modules_dir,
            );
        }

        hoist_virtual_store_dependencies(config, packages);
    }
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

    for (name, package_specifier) in selected {
        let target = config
            .virtual_store_dir
            .join(package_specifier.to_virtual_store_name())
            .join("node_modules")
            .join(&name);
        if !target.exists() {
            continue;
        }
        let symlink_path = hoisted_modules_dir.join(&name);
        symlink_package(&target, &symlink_path)
            .unwrap_or_else(|error| panic!("hoisted dependency symlink should succeed: {error}"));
    }
}
