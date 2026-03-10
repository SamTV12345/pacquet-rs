use crate::{
    InstallPackageBySnapshot, ResolvedPackages, create_symlink_layout, package_dependency_map,
};
use futures_util::stream::{self, StreamExt};
use pacquet_lockfile::{DependencyPath, PackageSnapshot, PkgNameVerPeer};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::{NodeLinker, Npmrc};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
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
            if !config.symlink && !matches!(config.node_linker, NodeLinker::Hoisted) {
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
                config.symlink,
            );
        }

        hoist_virtual_store_dependencies(config, packages);
        prune_orphaned_virtual_store_entries(config, packages);
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
    if !config.symlink {
        return false;
    }

    if !config.hoist && !config.shamefully_hoist {
        return false;
    }

    let hoisted_modules_dir = config.virtual_store_dir.join("node_modules");
    select_hoisted_packages(packages, config.dedupe_peer_dependents, &config.hoist_pattern)
        .into_iter()
        .any(|(name, package_specifier)| {
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
    if !config.symlink {
        return;
    }

    // Matches pnpm's semistrict defaults: hoist into node_modules/.pnpm/node_modules.
    if !config.hoist && !config.shamefully_hoist {
        return;
    }

    let hoisted_modules_dir = config.virtual_store_dir.join("node_modules");
    fs::create_dir_all(&hoisted_modules_dir)
        .unwrap_or_else(|error| panic!("create hoisted modules directory should succeed: {error}"));

    for (name, package_specifier) in
        select_hoisted_packages(packages, config.dedupe_peer_dependents, &config.hoist_pattern)
    {
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
        crate::link_package(config.symlink, &target, &symlink_path)
            .unwrap_or_else(|error| panic!("hoisted dependency symlink should succeed: {error}"));
    }
}

fn select_hoisted_packages(
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    dedupe_peer_dependents: bool,
    hoist_patterns: &[String],
) -> BTreeMap<String, PkgNameVerPeer> {
    let mut selected = BTreeMap::<String, PkgNameVerPeer>::new();
    for dependency_path in packages.keys() {
        let package_specifier = dependency_path.package_specifier.clone();
        let package_name = package_specifier.name.to_string();
        if !matches_patterns(&package_name, hoist_patterns) {
            continue;
        }
        let should_replace = match selected.get(&package_name) {
            None => true,
            Some(current) => {
                let new_version = package_specifier.suffix.version();
                let current_version = current.suffix.version();
                if new_version != current_version {
                    new_version > current_version
                } else if dedupe_peer_dependents {
                    let new_no_peer_suffix = package_specifier.suffix.peer().is_empty();
                    let current_no_peer_suffix = current.suffix.peer().is_empty();
                    if new_no_peer_suffix != current_no_peer_suffix {
                        new_no_peer_suffix
                    } else {
                        package_specifier.to_virtual_store_name() < current.to_virtual_store_name()
                    }
                } else {
                    let new_has_peer_suffix = !package_specifier.suffix.peer().is_empty();
                    let current_has_peer_suffix = !current.suffix.peer().is_empty();
                    if new_has_peer_suffix != current_has_peer_suffix {
                        new_has_peer_suffix
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

fn matches_patterns(name: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }

    let mut matched = false;
    for pattern in patterns {
        if let Some(negative) = pattern.strip_prefix('!') {
            if wildcard_match(name, negative) {
                matched = false;
            }
            continue;
        }
        if wildcard_match(name, pattern) {
            matched = true;
        }
    }

    matched
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let mut remainder = value;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }
        if first {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            first = false;
            continue;
        }
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }

    if !pattern.ends_with('*')
        && let Some(last) = pattern.split('*').next_back()
    {
        return value.ends_with(last);
    }

    true
}

fn dependency_layout_is_present(
    dependencies: &HashMap<pacquet_lockfile::PkgName, pacquet_lockfile::PackageSnapshotDependency>,
    virtual_node_modules_dir: &std::path::Path,
) -> bool {
    dependencies.keys().all(|alias| virtual_node_modules_dir.join(alias.to_string()).exists())
}

fn prune_orphaned_virtual_store_entries(
    config: &Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) {
    let max_age_minutes = config.modules_cache_max_age;
    let Ok(entries) = fs::read_dir(&config.virtual_store_dir) else {
        return;
    };

    let wanted = packages
        .keys()
        .map(|dependency_path| dependency_path.package_specifier.to_virtual_store_name())
        .collect::<HashSet<_>>();

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "node_modules" || wanted.contains(&name) {
            continue;
        }
        if !is_stale_enough(&path, max_age_minutes) {
            continue;
        }
        let _ = fs::remove_dir_all(&path);
    }
}

fn is_stale_enough(path: &std::path::Path, max_age_minutes: u64) -> bool {
    if max_age_minutes == 0 {
        return true;
    }

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed >= Duration::from_secs(max_age_minutes.saturating_mul(60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::{LockfileResolution, TarballResolution};

    fn dummy_snapshot() -> PackageSnapshot {
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
            dependencies: None,
            optional_dependencies: None,
            transitive_peer_dependencies: None,
            dev: None,
            optional: None,
        }
    }

    #[test]
    fn select_hoisted_packages_prefers_no_peer_suffix_when_dedupe_enabled() {
        let with_peer: DependencyPath = "/foo@1.0.0(bar@1.0.0)".parse().expect("with peer path");
        let without_peer: DependencyPath = "/foo@1.0.0".parse().expect("without peer path");
        let packages = HashMap::from([
            (with_peer, dummy_snapshot()),
            (without_peer.clone(), dummy_snapshot()),
        ]);

        let selected = select_hoisted_packages(&packages, true, &["*".to_string()]);
        let chosen = selected.get("foo").expect("selected package");
        assert_eq!(chosen.to_string(), without_peer.package_specifier.to_string());
    }

    #[test]
    fn select_hoisted_packages_keeps_peer_variant_when_dedupe_disabled() {
        let with_peer: DependencyPath = "/foo@1.0.0(bar@1.0.0)".parse().expect("with peer path");
        let without_peer: DependencyPath = "/foo@1.0.0".parse().expect("without peer path");
        let packages = HashMap::from([
            (with_peer.clone(), dummy_snapshot()),
            (without_peer, dummy_snapshot()),
        ]);

        let selected = select_hoisted_packages(&packages, false, &["*".to_string()]);
        let chosen = selected.get("foo").expect("selected package");
        assert_eq!(chosen.to_string(), with_peer.package_specifier.to_string());
    }

    #[test]
    fn select_hoisted_packages_filters_by_hoist_pattern() {
        let foo: DependencyPath = "/foo@1.0.0".parse().expect("foo path");
        let bar: DependencyPath = "/bar@1.0.0".parse().expect("bar path");
        let packages = HashMap::from([(foo, dummy_snapshot()), (bar, dummy_snapshot())]);

        let selected = select_hoisted_packages(&packages, true, &["foo".to_string()]);
        assert!(selected.contains_key("foo"));
        assert!(!selected.contains_key("bar"));
    }

    #[test]
    fn select_hoisted_packages_supports_negative_hoist_pattern() {
        let foo: DependencyPath = "/foo@1.0.0".parse().expect("foo path");
        let bar: DependencyPath = "/bar@1.0.0".parse().expect("bar path");
        let packages = HashMap::from([(foo, dummy_snapshot()), (bar, dummy_snapshot())]);

        let selected =
            select_hoisted_packages(&packages, true, &["*".to_string(), "!bar".to_string()]);
        assert!(selected.contains_key("foo"));
        assert!(!selected.contains_key("bar"));
    }

    #[test]
    fn prune_orphaned_virtual_store_entries_removes_orphans_when_cache_age_is_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let virtual_store_dir = dir.path().join("node_modules/.pnpm");
        fs::create_dir_all(virtual_store_dir.join("kept@1.0.0/node_modules/kept"))
            .expect("create kept package dir");
        fs::create_dir_all(virtual_store_dir.join("orphan@1.0.0/node_modules/orphan"))
            .expect("create orphan package dir");

        let mut config = Npmrc::new();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.modules_cache_max_age = 0;

        let kept_path: DependencyPath = "/kept@1.0.0".parse().expect("kept dep path");
        let packages = HashMap::from([(kept_path, dummy_snapshot())]);
        prune_orphaned_virtual_store_entries(&config, &packages);

        assert!(virtual_store_dir.join("kept@1.0.0").exists());
        assert!(!virtual_store_dir.join("orphan@1.0.0").exists());
    }

    #[test]
    fn prune_orphaned_virtual_store_entries_keeps_orphans_when_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let virtual_store_dir = dir.path().join("node_modules/.pnpm");
        fs::create_dir_all(virtual_store_dir.join("orphan@1.0.0/node_modules/orphan"))
            .expect("create orphan package dir");

        let mut config = Npmrc::new();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.modules_cache_max_age = 10_000;

        prune_orphaned_virtual_store_entries(&config, &HashMap::new());

        assert!(virtual_store_dir.join("orphan@1.0.0").exists());
    }
}
