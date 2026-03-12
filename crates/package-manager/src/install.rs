use crate::{
    InstallFrozenLockfile, InstallWithLockfile, InstallWithoutLockfile, ResolvedPackages,
    WorkspacePackages, collect_runtime_lockfile_config, get_outdated_lockfile_setting,
    hoist_virtual_store_packages, link_bins_for_manifest, progress_reporter,
    satisfies_package_manifest, write_modules_manifest_pruned_at, write_pnp_manifest_if_needed,
};
use pacquet_lockfile::{Lockfile, RootProjectSnapshot};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_tarball::MemCache;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub lockfile_importer_id: &'a str,
    pub workspace_packages: &'a WorkspacePackages,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
    pub lockfile_only: bool,
    pub force: bool,
    pub prefer_offline: bool,
    pub offline: bool,
    pub reporter: progress_reporter::InstallReporter,
    pub print_summary: bool,
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> miette::Result<()> {
        let start_time = std::time::Instant::now();
        let Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            dependency_groups,
            frozen_lockfile,
            lockfile_only,
            force,
            prefer_offline,
            offline,
            reporter,
            print_summary,
        } = self;

        if lockfile_only && !config.lockfile {
            miette::bail!("Cannot generate a pnpm-lock.yaml because lockfile is set to false");
        }

        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();
        let direct_dependencies = manifest.dependencies(dependency_groups.iter().copied()).count();
        progress_reporter::start(direct_dependencies, frozen_lockfile, reporter);
        let project_dir = manifest
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        config.store_dir.register_project(&project_dir)?;

        let result = async {
        tracing::info!(target: "pacquet::install", "Start all");

        match (config.lockfile, frozen_lockfile, lockfile) {
            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    manifest,
                    dependency_groups: dependency_groups.clone(),
                    force,
                    prefer_offline,
                    offline,
                }
                .run()
                .await;
            }
            (true, false, Some(lockfile)) => {
                let runtime_lockfile_config =
                    collect_runtime_lockfile_config(config, manifest, lockfile_dir);
                let maybe_project_snapshot = match &lockfile.project_snapshot {
                    RootProjectSnapshot::Single(snapshot) => Some(snapshot),
                    RootProjectSnapshot::Multi(snapshot) => {
                        snapshot.importers.get(lockfile_importer_id)
                    }
                };

                let lockfile_is_reusable = maybe_project_snapshot.is_some_and(|project_snapshot| {
                    get_outdated_lockfile_setting(lockfile, &runtime_lockfile_config).is_none()
                        && satisfies_package_manifest(
                            project_snapshot,
                            manifest,
                            config.auto_install_peers,
                            config.exclude_links_from_lockfile,
                            lockfile.lockfile_version.major >= 9,
                        )
                        .is_ok()
                });

                let has_local_directory_dependency_specs = manifest_has_local_directory_dependency(
                    manifest,
                    dependency_groups.iter().copied(),
                );

                if should_prefer_frozen_lockfile_path(
                    lockfile_is_reusable,
                    config.prefer_frozen_lockfile,
                    has_local_directory_dependency_specs,
                ) {
                    if !lockfile_only {
                        InstallFrozenLockfile {
                            http_client,
                            resolved_packages,
                            config,
                            project_snapshot: maybe_project_snapshot.expect("checked above"),
                            packages: lockfile.packages.as_ref(),
                            lockfile_dir,
                            dependency_groups: dependency_groups.clone(),
                            offline,
                            force,
                        }
                        .run()
                        .await;
                    }
                } else {
                    InstallWithLockfile {
                        tarball_mem_cache,
                        resolved_packages,
                        http_client,
                        config,
                        manifest,
                        existing_lockfile: Some(lockfile),
                        lockfile_dir,
                        lockfile_importer_id,
                        workspace_packages,
                        dependency_groups: dependency_groups.clone(),
                        lockfile_only,
                        force,
                        prefer_offline,
                        offline,
                    }
                    .run()
                    .await;
                }
            }
            (true, false, None) => {
                InstallWithLockfile {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    manifest,
                    existing_lockfile: lockfile,
                    lockfile_dir,
                    lockfile_importer_id,
                    workspace_packages,
                    dependency_groups: dependency_groups.clone(),
                    lockfile_only,
                    force,
                    prefer_offline,
                    offline,
                }
                .run()
                .await;
            }
            (true, true, None) => {
                miette::bail!(
                    "Cannot install with --frozen-lockfile because pnpm-lock.yaml was not found"
                );
            }
            (true, true, Some(lockfile)) => {
                let Lockfile { lockfile_version, project_snapshot, packages, .. } = lockfile;
                assert!(
                    lockfile_version.major == 6 || lockfile_version.major == 9,
                    "unsupported lockfile major version: {}",
                    lockfile_version.major
                );

                let project_snapshot = match project_snapshot {
                    RootProjectSnapshot::Single(snapshot) => snapshot,
                    RootProjectSnapshot::Multi(snapshot) => {
                        snapshot.importers.get(lockfile_importer_id).ok_or_else(|| {
                            miette::miette!(
                                "Cannot find importer `{}` in pnpm-lock.yaml",
                                lockfile_importer_id
                            )
                        })?
                    }
                };

                let runtime_lockfile_config =
                    collect_runtime_lockfile_config(config, manifest, lockfile_dir);
                if let Some(outdated_setting) =
                    get_outdated_lockfile_setting(lockfile, &runtime_lockfile_config)
                {
                    miette::bail!(
                        "Cannot proceed with the frozen installation. The current \"{outdated_setting}\" configuration doesn't match the value found in the lockfile"
                    );
                }
                if let Err(reason) = satisfies_package_manifest(
                    project_snapshot,
                    manifest,
                    config.auto_install_peers,
                    config.exclude_links_from_lockfile,
                    lockfile_version.major >= 9,
                ) {
                    miette::bail!(
                        "Cannot install with --frozen-lockfile because pnpm-lock.yaml is not up to date with {} ({reason})",
                        manifest.path().display(),
                    );
                }

                if !lockfile_only {
                    InstallFrozenLockfile {
                        http_client,
                        resolved_packages,
                        config,
                        project_snapshot,
                        packages: packages.as_ref(),
                        lockfile_dir,
                        dependency_groups: dependency_groups.clone(),
                        offline,
                        force,
                    }
                    .run()
                    .await;
                }
            }
        }

        tracing::info!(target: "pacquet::install", "Complete all");
        if !lockfile_only {
            hoist_virtual_store_packages(config)?;
            if config.strict_peer_dependencies {
                validate_strict_peer_dependencies(config, lockfile_dir)?;
            }
            link_bins_for_manifest(config, manifest, dependency_groups.iter().copied())?;
            write_modules_manifest_pruned_at(&config.modules_dir)
                .map_err(|error| miette::miette!("write node_modules/.modules.yaml: {error}"))?;
            write_pnp_manifest_if_needed(&config.node_linker, lockfile_dir)
                .map_err(|error| miette::miette!("write .pnp.cjs: {error}"))?;
        }

        Ok(())
        }
        .await;

        progress_reporter::finish(result.is_ok());

        if result.is_ok() && print_summary && reporter != progress_reporter::InstallReporter::Silent
        {
            print_pnpm_style_summary(manifest, &dependency_groups, &start_time);
        }

        result
    }
}

fn print_pnpm_style_summary(
    manifest: &PackageManifest,
    dependency_groups: &[DependencyGroup],
    start_time: &std::time::Instant,
) {
    use std::io::Write;

    let elapsed = start_time.elapsed();
    let elapsed_ms = elapsed.as_millis();

    // Collect dependencies by group
    let mut sections: Vec<(&str, Vec<(String, String)>)> = Vec::new();

    for &group in dependency_groups {
        let header = match group {
            DependencyGroup::Prod => "dependencies",
            DependencyGroup::Dev => "devDependencies",
            DependencyGroup::Optional => "optionalDependencies",
            DependencyGroup::Peer => "peerDependencies",
        };
        let mut deps: Vec<(String, String)> = manifest
            .dependencies(std::iter::once(group))
            .map(|(name, spec)| (name.to_string(), spec.to_string()))
            .collect();
        deps.sort();
        if !deps.is_empty() {
            sections.push((header, deps));
        }
    }

    let total_packages: usize = sections.iter().map(|(_, deps)| deps.len()).sum();

    let mut out = std::io::stdout().lock();

    if total_packages > 0 {
        let _ = writeln!(out, "Packages: +{total_packages}");
        let _ = writeln!(out, "{}", "+".repeat(total_packages.min(80)));
        let _ = writeln!(out);
    }

    for (header, deps) in &sections {
        let _ = writeln!(out, "{header}:");
        for (name, spec) in deps {
            let _ = writeln!(out, "{}", format_summary_dependency_line(name, spec));
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "Done in {elapsed_ms}ms");
}

pub fn format_summary_dependency_line(name: &str, spec: &str) -> String {
    format_summary_dependency_line_with_prefix('+', name, spec)
}

pub fn format_summary_dependency_line_with_prefix(prefix: char, name: &str, spec: &str) -> String {
    if let Some(alias_target) = spec.strip_prefix("npm:") {
        let (real_name, version) = split_package_name_and_version(alias_target);
        return version.map_or_else(
            || format!("{prefix} {name} <- {real_name}"),
            |version| {
                format!(
                    "{prefix} {name} <- {real_name} {}",
                    version.trim_start_matches('^').trim_start_matches('~')
                )
            },
        );
    }

    if let Some(local_path) = spec.strip_prefix("link:").or_else(|| spec.strip_prefix("file:")) {
        return format!("{prefix} {name} <- {}", Path::new(local_path).display());
    }

    let display_version = spec.trim_start_matches('^').trim_start_matches('~');
    format!("{prefix} {name} {display_version}")
}

fn split_package_name_and_version(value: &str) -> (&str, Option<&str>) {
    let search_start = usize::from(value.starts_with('@'));
    let Some(version_start) = value[search_start..].rfind('@').map(|index| index + search_start)
    else {
        return (value, None);
    };

    let (name, version) = value.split_at(version_start);
    (name, Some(version.trim_start_matches('@')))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StrictPeerPackageManifest {
    name: Option<String>,
    version: Option<String>,
    peer_dependencies: Option<HashMap<String, String>>,
    peer_dependencies_meta: Option<HashMap<String, StrictPeerDependencyMeta>>,
}

#[derive(Debug, Deserialize)]
struct StrictPeerDependencyMeta {
    optional: Option<bool>,
}

fn validate_strict_peer_dependencies(config: &Npmrc, workspace_root: &Path) -> miette::Result<()> {
    let package_dirs = collect_candidate_package_dirs(config);
    let mut issues = Vec::<String>::new();

    for package_dir in package_dirs {
        let manifest_path = package_dir.join("package.json");
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<StrictPeerPackageManifest>(&text) else {
            continue;
        };
        let Some(peer_dependencies) = manifest.peer_dependencies else {
            continue;
        };
        let Some(node_modules_dir) = containing_node_modules_dir(&package_dir) else {
            continue;
        };
        let package_name = manifest.name.unwrap_or_else(|| package_dir.display().to_string());

        for (peer_name, peer_range) in peer_dependencies {
            let is_optional = manifest
                .peer_dependencies_meta
                .as_ref()
                .and_then(|meta| meta.get(&peer_name))
                .and_then(|meta| meta.optional)
                .unwrap_or(false);
            if is_optional {
                continue;
            }

            let peer_version = resolve_peer_version(
                &peer_name,
                &node_modules_dir,
                workspace_root,
                config.resolve_peers_from_workspace_root,
            );
            let Some(peer_version) = peer_version else {
                issues.push(format!(
                    "{package_name} requires peer {peer_name}@{peer_range}, but it is not installed"
                ));
                continue;
            };
            if !peer_version_satisfies(&peer_version, &peer_range) {
                issues.push(format!(
                    "{package_name} requires peer {peer_name}@{peer_range}, but found {peer_version}"
                ));
            }
        }
    }

    if issues.is_empty() {
        return Ok(());
    }

    let details =
        issues.iter().take(10).map(|issue| format!("  - {issue}")).collect::<Vec<_>>().join("\n");
    miette::bail!("Cannot proceed because strict-peer-dependencies is enabled:\n{details}");
}

fn collect_candidate_package_dirs(config: &Npmrc) -> Vec<PathBuf> {
    let mut dirs = Vec::<PathBuf>::new();
    dirs.extend(collect_package_dirs_in_node_modules(&config.modules_dir));

    let Ok(virtual_store_entries) = fs::read_dir(&config.virtual_store_dir) else {
        return dirs;
    };
    for entry in virtual_store_entries.flatten() {
        let entry_path = entry.path();
        let Some(name) = entry_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "node_modules" {
            continue;
        }
        dirs.extend(collect_package_dirs_in_node_modules(&entry_path.join("node_modules")));
    }
    dirs
}

fn collect_package_dirs_in_node_modules(node_modules_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(node_modules_dir) else {
        return vec![];
    };
    let mut result = Vec::<PathBuf>::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".pnpm" || name == ".pacquet" || name == ".bin" {
            continue;
        }
        if name.starts_with('@') {
            let Ok(scope_entries) = fs::read_dir(&path) else {
                continue;
            };
            for scope_entry in scope_entries.flatten() {
                let scope_path = scope_entry.path();
                if scope_path.is_dir() {
                    result.push(scope_path);
                }
            }
            continue;
        }
        result.push(path);
    }
    result
}

fn containing_node_modules_dir(package_dir: &Path) -> Option<PathBuf> {
    package_dir.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "node_modules")
            .then(|| ancestor.to_path_buf())
    })
}

fn resolve_peer_version(
    peer_name: &str,
    package_node_modules_dir: &Path,
    workspace_root: &Path,
    resolve_from_workspace_root: bool,
) -> Option<String> {
    let mut candidate_dirs = vec![package_node_modules_dir.to_path_buf()];
    if resolve_from_workspace_root {
        let root_node_modules_dir = workspace_root.join("node_modules");
        if root_node_modules_dir != package_node_modules_dir {
            candidate_dirs.push(root_node_modules_dir);
        }
    }

    candidate_dirs.into_iter().find_map(|node_modules_dir| {
        let peer_manifest_path = node_modules_dir.join(peer_name).join("package.json");
        let peer_text = fs::read_to_string(peer_manifest_path).ok()?;
        let peer_manifest = serde_json::from_str::<StrictPeerPackageManifest>(&peer_text).ok()?;
        peer_manifest.version
    })
}

fn peer_version_satisfies(version: &str, range: &str) -> bool {
    let Ok(version) = version.parse::<node_semver::Version>() else {
        return true;
    };
    let Ok(range) = range.parse::<node_semver::Range>() else {
        return true;
    };
    version.satisfies(&range)
}

fn manifest_has_local_directory_dependency(
    manifest: &PackageManifest,
    dependency_groups: impl IntoIterator<Item = DependencyGroup>,
) -> bool {
    manifest.dependencies(dependency_groups).any(|(_, specifier)| {
        specifier
            .strip_prefix("file:")
            .is_some_and(|target| !(target.ends_with(".tgz") || target.ends_with(".tar.gz")))
    })
}

fn should_prefer_frozen_lockfile_path(
    lockfile_is_reusable: bool,
    prefer_frozen_lockfile: bool,
    has_local_directory_dependency_specs: bool,
) -> bool {
    lockfile_is_reusable && prefer_frozen_lockfile && !has_local_directory_dependency_specs
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::{DependencyGroup, PackageManifest};
    use pacquet_registry_mock::AutoMockInstance;
    use pacquet_testing_utils::fs::{
        get_all_folders, get_filenames_in_folder, is_symlink_or_junction,
    };
    use std::{fs, path::Path};
    use tempfile::tempdir;

    #[test]
    fn should_install_dependencies() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules"); // TODO: we shouldn't have to define this
        let virtual_store_dir = modules_dir.join(".pacquet"); // TODO: we shouldn't have to define this

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        manifest
            .add_dependency("@pnpm.e2e/hello-world-js-bin", "1.0.0", DependencyGroup::Prod)
            .unwrap();
        manifest.add_dependency("@pnpm/xyz", "1.0.0", DependencyGroup::Dev).unwrap();

        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.registry = mock_instance.url();
        let config = config.leak();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
            .block_on(async {
                Install {
                    tarball_mem_cache: &Default::default(),
                    http_client: &Default::default(),
                    config,
                    manifest: &manifest,
                    lockfile: None,
                    lockfile_dir: dir.path(),
                    lockfile_importer_id: ".",
                    workspace_packages: &Default::default(),
                    dependency_groups: [
                        DependencyGroup::Prod,
                        DependencyGroup::Dev,
                        DependencyGroup::Optional,
                    ],
                    frozen_lockfile: false,
                    lockfile_only: false,
                    force: false,
                    prefer_offline: false,
                    offline: false,
                    reporter: progress_reporter::InstallReporter::Default,
                    print_summary: true,
                    resolved_packages: &Default::default(),
                }
                .run()
                .await
                .unwrap();
            });

        // Make sure the package is installed
        let path = project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm.e2e+hello-world-js-bin@1.0.0");
        assert!(path.exists());
        // Make sure we install dev-dependencies as well
        let path = project_root.join("node_modules/@pnpm/xyz");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm+xyz@1.0.0");
        assert!(path.is_dir());

        insta::assert_debug_snapshot!(get_all_folders(&project_root));

        drop((dir, mock_instance)); // cleanup
    }

    #[test]
    fn pnp_node_linker_should_write_pnp_manifest_and_skip_root_dependency_links() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().expect("tempdir");
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");

        let manifest_path = project_root.join("package.json");
        fs::create_dir_all(&project_root).expect("create project root");
        let mut manifest = PackageManifest::create_if_needed(manifest_path).expect("manifest");
        manifest
            .add_dependency("@pnpm.e2e/hello-world-js-bin", "1.0.0", DependencyGroup::Prod)
            .expect("add dependency");
        manifest.save().expect("save manifest");

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.registry = mock_instance.url();
        config.node_linker = pacquet_npmrc::NodeLinker::Pnp;
        config.symlink = false;
        let config = config.leak();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
            .block_on(async {
                Install {
                    tarball_mem_cache: &Default::default(),
                    http_client: &Default::default(),
                    config,
                    manifest: &manifest,
                    lockfile: None,
                    lockfile_dir: project_root.as_path(),
                    lockfile_importer_id: ".",
                    workspace_packages: &Default::default(),
                    dependency_groups: [
                        DependencyGroup::Prod,
                        DependencyGroup::Dev,
                        DependencyGroup::Optional,
                    ],
                    frozen_lockfile: false,
                    lockfile_only: false,
                    force: false,
                    prefer_offline: false,
                    offline: false,
                    reporter: progress_reporter::InstallReporter::Default,
                    print_summary: true,
                    resolved_packages: &Default::default(),
                }
                .run()
                .await
                .expect("run install");
            });

        assert!(project_root.join(".pnp.cjs").exists());
        assert_eq!(get_filenames_in_folder(&modules_dir), [".modules.yaml", ".pnpm"]);
        assert!(!modules_dir.join("@pnpm.e2e").exists());
        assert!(!modules_dir.join(".bin").exists());
        assert!(virtual_store_dir.join("@pnpm.e2e+hello-world-js-bin@1.0.0").exists());

        drop((dir, mock_instance));
    }

    #[test]
    fn strict_peer_validation_fails_for_missing_required_peer() {
        let dir = tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.modules_dir = dir.path().join("node_modules");
        config.virtual_store_dir = config.modules_dir.join(".pnpm");
        config.strict_peer_dependencies = true;

        let pkg_dir = config.modules_dir.join("a");
        fs::create_dir_all(&pkg_dir).expect("create package dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "a",
                "version": "1.0.0",
                "peerDependencies": {
                    "peer-a": "^1.0.0"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let error = validate_strict_peer_dependencies(&config, dir.path())
            .expect_err("missing peer should fail");
        let message = error.to_string();
        assert!(message.contains("strict-peer-dependencies"));
        assert!(message.contains("peer-a"));
    }

    #[test]
    fn strict_peer_validation_ignores_optional_peer() {
        let dir = tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.modules_dir = dir.path().join("node_modules");
        config.virtual_store_dir = config.modules_dir.join(".pnpm");
        config.strict_peer_dependencies = true;

        let pkg_dir = config.modules_dir.join("a");
        fs::create_dir_all(&pkg_dir).expect("create package dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "a",
                "version": "1.0.0",
                "peerDependencies": {
                    "peer-a": "^1.0.0"
                },
                "peerDependenciesMeta": {
                    "peer-a": {
                        "optional": true
                    }
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        validate_strict_peer_dependencies(&config, dir.path())
            .expect("optional missing peer should pass");
    }

    #[test]
    fn strict_peer_validation_fails_for_incompatible_peer_version() {
        let dir = tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.modules_dir = dir.path().join("node_modules");
        config.virtual_store_dir = config.modules_dir.join(".pnpm");
        config.strict_peer_dependencies = true;

        let pkg_dir = config.modules_dir.join("a");
        let peer_dir = config.modules_dir.join("peer-a");
        fs::create_dir_all(&pkg_dir).expect("create package dir");
        fs::create_dir_all(&peer_dir).expect("create peer package dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "a",
                "version": "1.0.0",
                "peerDependencies": {
                    "peer-a": "^2.0.0"
                }
            })
            .to_string(),
        )
        .expect("write package.json");
        fs::write(
            peer_dir.join("package.json"),
            serde_json::json!({
                "name": "peer-a",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write peer package.json");

        let error = validate_strict_peer_dependencies(&config, dir.path())
            .expect_err("incompatible peer should fail");
        let message = error.to_string();
        assert!(message.contains("found 1.0.0"));
    }

    #[test]
    fn strict_peer_validation_resolves_from_workspace_root_when_enabled() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_dir = workspace_root.join("packages/app");
        let project_modules = project_dir.join("node_modules");
        let root_modules = workspace_root.join("node_modules");

        let mut config = Npmrc::new();
        config.modules_dir = project_modules.clone();
        config.virtual_store_dir = project_modules.join(".pnpm");
        config.strict_peer_dependencies = true;
        config.resolve_peers_from_workspace_root = true;

        let pkg_dir = project_modules.join("a");
        let peer_dir = root_modules.join("peer-a");
        fs::create_dir_all(&pkg_dir).expect("create package dir");
        fs::create_dir_all(&peer_dir).expect("create root peer dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "a",
                "version": "1.0.0",
                "peerDependencies": {
                    "peer-a": "^1.0.0"
                }
            })
            .to_string(),
        )
        .expect("write package.json");
        fs::write(
            peer_dir.join("package.json"),
            serde_json::json!({
                "name": "peer-a",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write root peer package.json");

        validate_strict_peer_dependencies(&config, &workspace_root)
            .expect("peer from workspace root should satisfy strict-peer-dependencies");
    }

    #[test]
    fn strict_peer_validation_fails_without_workspace_root_resolution() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_dir = workspace_root.join("packages/app");
        let project_modules = project_dir.join("node_modules");
        let root_modules = workspace_root.join("node_modules");

        let mut config = Npmrc::new();
        config.modules_dir = project_modules.clone();
        config.virtual_store_dir = project_modules.join(".pnpm");
        config.strict_peer_dependencies = true;
        config.resolve_peers_from_workspace_root = false;

        let pkg_dir = project_modules.join("a");
        let peer_dir = root_modules.join("peer-a");
        fs::create_dir_all(&pkg_dir).expect("create package dir");
        fs::create_dir_all(&peer_dir).expect("create root peer dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "a",
                "version": "1.0.0",
                "peerDependencies": {
                    "peer-a": "^1.0.0"
                }
            })
            .to_string(),
        )
        .expect("write package.json");
        fs::write(
            peer_dir.join("package.json"),
            serde_json::json!({
                "name": "peer-a",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write root peer package.json");

        let error = validate_strict_peer_dependencies(&config, &workspace_root)
            .expect_err("peer from workspace root should not be used when disabled");
        assert!(error.to_string().contains("peer-a"));
    }

    #[test]
    fn manifest_has_local_directory_dependency_ignores_registry_and_tarball_specs() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest(
            dir.path(),
            serde_json::json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "registry-dep": "1.0.0",
                    "tarball-dep": "file:../pkg.tgz",
                    "local-dir-dep": "file:../pkg"
                }
            }),
        );

        assert!(manifest_has_local_directory_dependency(&manifest, [DependencyGroup::Prod]));
    }

    #[test]
    fn manifest_has_local_directory_dependency_returns_false_without_file_directory_specs() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest(
            dir.path(),
            serde_json::json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "registry-dep": "1.0.0",
                    "linked-dep": "link:../pkg",
                    "tarball-dep": "file:../pkg.tgz"
                }
            }),
        );

        assert!(!manifest_has_local_directory_dependency(&manifest, [DependencyGroup::Prod]));
    }

    #[test]
    fn should_prefer_frozen_lockfile_path_allows_linked_dependencies() {
        assert!(should_prefer_frozen_lockfile_path(true, true, false));
    }

    #[test]
    fn should_prefer_frozen_lockfile_path_disables_headless_for_local_file_directories() {
        assert!(!should_prefer_frozen_lockfile_path(true, true, true));
    }

    #[test]
    fn summary_line_formats_npm_alias_like_pnpm() {
        assert_eq!(
            format_summary_dependency_line(
                "hello-alias",
                "npm:@pnpm.e2e/hello-world-js-bin@^1.0.0"
            ),
            "+ hello-alias <- @pnpm.e2e/hello-world-js-bin 1.0.0"
        );
    }

    #[test]
    fn summary_line_formats_local_link_like_pnpm() {
        #[cfg(windows)]
        let spec = r"link:..\local-pkg";
        #[cfg(not(windows))]
        let spec = "link:../local-pkg";

        let line = format_summary_dependency_line("local-pkg", spec);
        assert!(line.starts_with("+ local-pkg <- "));
        assert!(line.contains("local-pkg"));
    }

    fn load_manifest(dir: &Path, value: serde_json::Value) -> PackageManifest {
        let manifest_path = dir.join("package.json");
        fs::write(&manifest_path, value.to_string()).expect("write package.json");
        PackageManifest::from_path(manifest_path).expect("load manifest")
    }
}
