use crate::{
    InstallPackageBySnapshot, ResolvedPackages, create_symlink_layout, package_dependency_map,
    should_prune_orphaned_virtual_store_entries,
};
use futures_util::stream::{self, StreamExt};
use pacquet_lockfile::{DependencyPath, PackageSnapshot, PkgNameVerPeer};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::{NodeLinker, Npmrc};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::Path,
};

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub lockfile_dir: &'a std::path::Path,
    pub resolved_packages: Option<&'a ResolvedPackages>,
    pub offline: bool,
    pub force: bool,
    /// When true, skip the orphan-pruning step at the end.
    /// This MUST be set when installing a single workspace importer
    /// sequentially, because the `packages` map only contains that
    /// importer's transitive deps -- pruning would delete other
    /// importers' packages from the shared virtual store.
    pub skip_prune: bool,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    ///
    /// Follows pnpm's headless install pipelining model:
    /// - Phase 0: Pre-create all virtual store directories
    /// - Phase 1: `tokio::join!` of two concurrent tasks:
    ///   - Task A (`link_all_symlink_layouts`): Create symlink layouts using lockfile data only
    ///   - Task B (`download_and_import_all_packages`): Download, extract, and import packages
    /// - Phase 2: Hoist dependencies and prune orphans
    pub async fn run(self) {
        let CreateVirtualStore {
            http_client,
            config,
            packages,
            lockfile_dir,
            resolved_packages,
            offline,
            force,
            skip_prune,
        } = self;

        let packages = if let Some(packages) = packages {
            packages
        } else {
            return;
        };

        // Phase 0: Pre-create all virtual store node_modules directories.
        // This is needed so that symlinks created in Task A have valid target
        // directories (required for Windows junctions, harmless on Unix).
        pre_create_virtual_store_dirs(config, packages);

        // Phase 1: Run symlink layouts and package downloads IN PARALLEL,
        // matching pnpm's Promise.all([linkAllModules(), linkAllPkgs()]) pattern.
        //
        // Task A (symlink layouts) is pure synchronous filesystem work and runs
        // on a dedicated OS thread so it doesn't block tokio workers.
        // Task B (downloads) runs on the tokio async runtime concurrently.
        let needs_layout = force
            || virtual_dependency_layout_missing(config, packages)
            || hoisted_links_missing(config, packages);

        let layout_handle = if needs_layout {
            Some(std::thread::spawn({
                let packages = packages.clone();
                let lockfile_dir = lockfile_dir.to_path_buf();
                move || {
                    link_all_symlink_layouts_sync(config, &packages, &lockfile_dir);
                }
            }))
        } else {
            None
        };

        download_and_import_all_packages(
            http_client,
            config,
            packages,
            lockfile_dir,
            resolved_packages,
            offline,
            force,
        )
        .await;

        // Wait for the layout thread to finish (it usually finishes before downloads).
        if let Some(handle) = layout_handle {
            handle.join().expect("symlink layout thread should not panic");
        }

        // Phase 2: Hoist dependencies and prune orphaned entries.
        hoist_virtual_store_dependencies(config, packages);
        if !skip_prune {
            prune_orphaned_virtual_store_entries(config, packages);
        }
    }
}

/// Phase 0: Pre-create all `node_modules/.pnpm/<pkg@ver>/node_modules/` directories.
/// On Windows, junctions require the target directory to exist. On Unix, dangling
/// symlinks are valid but pre-creating directories avoids race conditions.
fn pre_create_virtual_store_dirs(
    config: &Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) {
    packages.par_iter().for_each(|(dependency_path, _)| {
        let virtual_node_modules_dir = config
            .virtual_store_dir
            .join(dependency_path.package_specifier.to_virtual_store_name())
            .join("node_modules");
        let _ = fs::create_dir_all(&virtual_node_modules_dir);
        // Also create the leaf package directory (needed for Windows junctions)
        let package_name = dependency_path.package_specifier.name().to_string();
        let _ = fs::create_dir_all(virtual_node_modules_dir.join(package_name));
    });
}

/// Task A (pnpm: linkAllModules): Create symlink layouts for all packages.
/// This only uses lockfile data to compute paths - no downloaded files needed.
/// Purely synchronous - runs on a dedicated OS thread alongside tokio downloads.
fn link_all_symlink_layouts_sync(
    config: &'static Npmrc,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    lockfile_dir: &Path,
) {
    let work: Vec<_> = packages
        .iter()
        .filter_map(|(dependency_path, package_snapshot)| {
            let dependencies = package_dependency_map(package_snapshot);
            if dependencies.is_empty() {
                return None;
            }
            if !config.symlink && !matches!(config.node_linker, NodeLinker::Hoisted) {
                return None;
            }
            let virtual_node_modules_dir = config
                .virtual_store_dir
                .join(dependency_path.package_specifier.to_virtual_store_name())
                .join("node_modules");
            Some((dependencies, virtual_node_modules_dir))
        })
        .collect();

    if work.is_empty() {
        return;
    }

    let virtual_store_dir = &config.virtual_store_dir;
    let symlink = config.symlink;

    work.par_iter().for_each(|(dependencies, virtual_node_modules_dir)| {
        if dependency_layout_is_present(dependencies, virtual_node_modules_dir) {
            return;
        }
        create_symlink_layout(
            dependencies,
            virtual_store_dir,
            lockfile_dir,
            virtual_node_modules_dir,
            symlink,
        );
    });
}

/// Task B (pnpm: linkAllPkgs): Download, extract, and import all packages concurrently.
/// Each package independently awaits its own download, matching pnpm's per-node
/// `await depNode.fetching()` pattern.
async fn download_and_import_all_packages<'a>(
    http_client: &'a ThrottledClient,
    config: &'static Npmrc,
    packages: &'a HashMap<DependencyPath, PackageSnapshot>,
    lockfile_dir: &'a Path,
    resolved_packages: Option<&'a ResolvedPackages>,
    offline: bool,
    force: bool,
) {
    let install_concurrency = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().clamp(4, 32))
        .unwrap_or(16);

    stream::iter(packages.iter())
        .for_each_concurrent(install_concurrency, |(dependency_path, package_snapshot)| async {
            if let Some(seen) = resolved_packages {
                let virtual_store_name = dependency_path.package_specifier.to_virtual_store_name();
                if !seen.insert(virtual_store_name) {
                    return;
                }
            }
            match (InstallPackageBySnapshot {
                http_client,
                config,
                dependency_path,
                package_snapshot,
                lockfile_dir,
                offline,
                force,
            })
            .run()
            .await
            {
                Ok(_imported) => {}
                Err(error) => {
                    let is_optional = package_snapshot.optional.unwrap_or(false);
                    let dep_path_str = dependency_path.to_string();
                    if is_optional {
                        tracing::debug!(
                            dep_path = %dep_path_str,
                            "Skipping optional package that failed to install: {error}"
                        );
                    } else {
                        tracing::error!(
                            dep_path = %dep_path_str,
                            "Failed to install package: {error}"
                        );
                    }
                }
            }
        })
        .await;
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

pub(crate) fn select_hoisted_packages(
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    dedupe_peer_dependents: bool,
    hoist_patterns: &[String],
) -> BTreeMap<String, PkgNameVerPeer> {
    let mut selected = BTreeMap::<String, PkgNameVerPeer>::new();
    for dependency_path in packages.keys() {
        let Some(package_specifier) = dependency_path.package_specifier.registry_specifier() else {
            continue;
        };
        let package_specifier = package_specifier.clone();
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

    let parts = pattern.split('*').filter(|part| !part.is_empty()).collect::<Vec<_>>();
    if parts.is_empty() {
        return false;
    }

    let mut search_start = 0usize;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 && !pattern.starts_with('*') {
            if !value[search_start..].starts_with(part) {
                return false;
            }
            search_start += part.len();
            continue;
        }
        let Some(offset) = value[search_start..].find(part) else {
            return false;
        };
        search_start += offset + part.len();
    }

    pattern.ends_with('*') || value.ends_with(parts.last().expect("checked above"))
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
    if !should_prune_orphaned_virtual_store_entries(
        &config.modules_dir,
        config.modules_cache_max_age,
    ) {
        return;
    }

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
        let _ = fs::remove_dir_all(&path);
    }
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
    fn wildcard_match_supports_leading_star() {
        assert!(wildcard_match("@pnpm.e2e/hello-world-js-bin", "*hello-world-js-bin"));
    }

    #[test]
    fn wildcard_match_empty_pattern_does_not_match_everything() {
        assert!(!wildcard_match("eslint", ""));
    }

    #[test]
    fn prune_orphaned_virtual_store_entries_removes_orphans_when_cache_age_is_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("kept@1.0.0/node_modules/kept"))
            .expect("create kept package dir");
        fs::create_dir_all(virtual_store_dir.join("orphan@1.0.0/node_modules/orphan"))
            .expect("create orphan package dir");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir;
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
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("orphan@1.0.0/node_modules/orphan"))
            .expect("create orphan package dir");
        fs::create_dir_all(&modules_dir).expect("create modules dir");
        fs::write(modules_dir.join(".modules.yaml"), "prunedAt: '9999999999'\n")
            .expect("write fresh modules manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir;
        config.virtual_store_dir = virtual_store_dir.clone();
        config.modules_cache_max_age = 10_000;

        prune_orphaned_virtual_store_entries(&config, &HashMap::new());

        assert!(virtual_store_dir.join("orphan@1.0.0").exists());
    }
}
