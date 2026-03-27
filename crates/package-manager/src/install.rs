use crate::{
    CreateVirtualStore, InstallFrozenLockfile, InstallWithLockfile, InstallWithoutLockfile,
    ResolvedPackages, SymlinkDirectDependencies, WorkspacePackages,
    collect_runtime_lockfile_config, dedupe_project_snapshot,
    direct_dependency_virtual_store_location, filter_installable_optional_dependencies,
    get_outdated_lockfile_setting, hoist_virtual_store_packages, importer_dependencies_ready,
    included_dependencies, link_bins_for_manifest, progress_reporter, read_modules_manifest,
    satisfies_package_manifest, satisfies_root_lockfile_config, write_modules_manifest,
    write_pnp_manifest_if_needed,
};
use pacquet_lockfile::{
    DependencyPath, Lockfile, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer,
    ProjectSnapshot, ResolvedDependencyVersion, RootProjectSnapshot,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_tarball::MemCache;
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
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
    pub preferred_versions: Option<&'a crate::PreferredVersions>,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
    pub lockfile_only: bool,
    pub force: bool,
    pub prefer_offline: bool,
    pub offline: bool,
    pub pnpmfile: Option<&'a Path>,
    pub ignore_pnpmfile: bool,
    pub reporter_prefix: Option<&'a str>,
    pub reporter: progress_reporter::InstallReporter,
    pub print_summary: bool,
    pub manage_progress_reporter: bool,
}

pub struct WorkspaceFrozenInstallTarget {
    pub importer_id: String,
    pub config: &'static Npmrc,
    pub manifest: PackageManifest,
    pub project_snapshot: pacquet_lockfile::ProjectSnapshot,
}

pub struct InstallFrozenWorkspace<'a> {
    pub http_client: &'a ThrottledClient,
    pub resolved_packages: &'a ResolvedPackages,
    pub shared_config: &'static Npmrc,
    pub lockfile: &'a Lockfile,
    pub targets: Vec<WorkspaceFrozenInstallTarget>,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub lockfile_dir: &'a Path,
    pub dependency_groups: Vec<DependencyGroup>,
    pub offline: bool,
    pub force: bool,
    pub pnpmfile: Option<&'a Path>,
    pub ignore_pnpmfile: bool,
}

impl<'a> InstallFrozenWorkspace<'a> {
    pub async fn run(self) -> miette::Result<Vec<String>> {
        let InstallFrozenWorkspace {
            http_client,
            resolved_packages,
            shared_config,
            lockfile,
            targets,
            packages,
            lockfile_dir,
            dependency_groups,
            offline,
            force,
            pnpmfile,
            ignore_pnpmfile,
        } = self;

        let targets = targets
            .into_iter()
            .map(|target| {
                let runtime_lockfile_config = collect_runtime_lockfile_config(
                    target.config,
                    &target.manifest,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                );
                if let Some(outdated_setting) =
                    get_outdated_lockfile_setting(lockfile, &runtime_lockfile_config)
                {
                    return Err(miette::miette!(
                        "Cannot proceed with the frozen installation. The current \"{outdated_setting}\" configuration doesn't match the value found in the lockfile"
                    ));
                }
                if let Err(reason) = satisfies_package_manifest(
                    &target.project_snapshot,
                    &target.manifest,
                    target.config.auto_install_peers,
                    target.config.exclude_links_from_lockfile,
                    lockfile.lockfile_version.major >= 9,
                ) {
                    return Err(miette::miette!(
                        "Cannot install with --frozen-lockfile because pnpm-lock.yaml is not up to date with {} ({reason})",
                        target.manifest.path().display(),
                    ));
                }
                if let Err(reason) =
                    satisfies_root_lockfile_config(lockfile, &target.manifest, lockfile_dir)
                {
                    return Err(miette::miette!(
                        "Cannot install with --frozen-lockfile because pnpm-lock.yaml is not up to date with {} ({reason})",
                        target.manifest.path().display(),
                    ));
                }
                let project_dir = target
                    .manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.to_path_buf());
                target.config.store_dir.register_project(&project_dir)?;
                recreate_modules_dir_if_incompatible(target.config, &dependency_groups).map_err(
                    |error| miette::miette!("recreate incompatible node_modules: {error}"),
                )?;
                Ok::<_, miette::Report>(target)
            })
            .collect::<miette::Result<Vec<_>>>()?;

        let (filtered_targets, filtered_packages, skipped) =
            filter_installable_workspace_targets(targets, packages, &dependency_groups);
        let selected_importer_ids = filtered_targets
            .iter()
            .map(|target| target.importer_id.clone())
            .collect::<HashSet<_>>();
        let (filtered_packages, skipped) = preserve_current_packages_for_unselected_importers(
            shared_config,
            filtered_packages,
            skipped,
            &selected_importer_ids,
            &dependency_groups,
        );

        if force
            || filtered_targets.iter().any(|target| {
                !importer_dependencies_ready(
                    shared_config,
                    &target.project_snapshot,
                    filtered_packages.as_ref(),
                    dependency_groups.iter().copied(),
                )
            })
        {
            CreateVirtualStore {
                http_client,
                config: shared_config,
                packages: filtered_packages.as_ref(),
                lockfile_dir,
                resolved_packages: Some(resolved_packages),
                offline,
                force,
            }
            .run()
            .await;
        }

        filtered_targets.par_iter().for_each(|target| {
            let deduped_project_snapshot = dedupe_project_snapshot(
                &target.project_snapshot,
                filtered_packages.as_ref(),
                target.config.dedupe_peer_dependents,
            );
            SymlinkDirectDependencies {
                config: target.config,
                project_snapshot: &deduped_project_snapshot,
                packages: filtered_packages.as_ref(),
                dependency_groups: dependency_groups.iter().copied(),
            }
            .run();
        });

        hoist_virtual_store_packages(shared_config)?;
        if shared_config.strict_peer_dependencies {
            validate_strict_peer_dependencies(shared_config, lockfile_dir)?;
        }

        filtered_targets
            .par_iter()
            .try_for_each(|target| {
                link_bins_for_manifest(
                    target.config,
                    &target.manifest,
                    dependency_groups.iter().copied(),
                )
                .map_err(|error| error.to_string())?;
                let direct_dependency_names = target
                    .manifest
                    .dependencies(dependency_groups.iter().copied())
                    .map(|(name, _)| name.to_string())
                    .collect::<Vec<_>>();
                write_modules_manifest(
                    &target.config.modules_dir,
                    target.config,
                    &dependency_groups,
                    &skipped,
                    filtered_packages.as_ref(),
                    Some(&direct_dependency_names),
                )
                .map_err(|error| error.to_string())?;
                Ok::<_, String>(())
            })
            .map_err(|error| miette::miette!("{error}"))?;

        write_pnp_manifest_if_needed(&shared_config.node_linker, lockfile_dir)
            .map_err(|error| miette::miette!("write .pnp.cjs: {error}"))?;

        Ok(skipped)
    }
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> miette::Result<Vec<String>> {
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
            preferred_versions,
            dependency_groups,
            frozen_lockfile,
            lockfile_only,
            force,
            prefer_offline,
            offline,
            pnpmfile,
            ignore_pnpmfile,
            reporter_prefix,
            reporter,
            print_summary,
            manage_progress_reporter,
        } = self;

        if lockfile_only && !config.lockfile {
            miette::bail!("Cannot generate a pnpm-lock.yaml because lockfile is set to false");
        }

        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();

        // pnpm fast path: if lockfile exists, is reusable, and the persisted
        // install state on disk already matches, skip ALL work (resolution,
        // linking, hoisting, bin-linking, manifest writing).  This is what
        // gives pnpm a 1-2s warm install.
        if !force
            && !lockfile_only
            && config.lockfile
            && !frozen_lockfile
            && lockfile.is_some_and(|lockfile| {
                let maybe_project_snapshot = match &lockfile.project_snapshot {
                    RootProjectSnapshot::Single(snapshot) => Some(snapshot),
                    RootProjectSnapshot::Multi(snapshot) => {
                        snapshot.importers.get(lockfile_importer_id)
                    }
                };
                let runtime_lockfile_config = collect_runtime_lockfile_config(
                    config,
                    manifest,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                );
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
                        && satisfies_root_lockfile_config(lockfile, manifest, lockfile_dir).is_ok()
                });
                let has_local_dir_deps = manifest_has_local_directory_dependency(
                    manifest,
                    dependency_groups.iter().copied(),
                );
                lockfile_is_reusable
                    && config.prefer_frozen_lockfile
                    && !has_local_dir_deps
                    && persisted_install_state_is_reusable(
                        lockfile,
                        config,
                        lockfile_importer_id,
                        &dependency_groups,
                    )
            })
        {
            tracing::info!(target: "pacquet::install", "Lockfile is up to date, resolution step is skipped");
            return Ok(Vec::new());
        }

        let direct_dependencies = manifest.dependencies(dependency_groups.iter().copied()).count();
        if manage_progress_reporter {
            progress_reporter::start(
                direct_dependencies,
                frozen_lockfile,
                reporter,
                reporter_prefix,
            );
        }
        let project_dir = manifest
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        config.store_dir.register_project(&project_dir)?;
        if !lockfile_only {
            recreate_modules_dir_if_incompatible(config, &dependency_groups)
                .map_err(|error| miette::miette!("recreate incompatible node_modules: {error}"))?;
        }

        let result = async {
        tracing::info!(target: "pacquet::install", "Start all");

        let skipped = match (config.lockfile, frozen_lockfile, lockfile) {

            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config,
                    manifest,
                    lockfile_dir,
                    dependency_groups: dependency_groups.clone(),
                    force,
                    prefer_offline,
                    offline,
                    pnpmfile,
                    ignore_pnpmfile,
                }
                .run()
                .await;
                Vec::new()
            }
            (true, false, Some(lockfile)) => {
                let runtime_lockfile_config = collect_runtime_lockfile_config(
                    config,
                    manifest,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                );
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
                        && direct_workspace_links_match_current_config(
                            config,
                            manifest,
                            project_snapshot,
                            workspace_packages,
                            &dependency_groups,
                        )
                        && satisfies_root_lockfile_config(lockfile, manifest, lockfile_dir).is_ok()
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
                    if !force
                        && !lockfile_only
                        && persisted_install_state_is_reusable(
                            lockfile,
                            config,
                            lockfile_importer_id,
                            &dependency_groups,
                        )
                    {
                        Vec::new()
                    } else if !lockfile_only {
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
                            pnpmfile,
                            ignore_pnpmfile,
                        }
                        .run()
                        .await
                    } else {
                        Vec::new()
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
                        preferred_versions,
                        dependency_groups: dependency_groups.clone(),
                        lockfile_only,
                        force,
                        prefer_offline,
                        offline,
                        pnpmfile,
                        ignore_pnpmfile,
                    }
                    .run()
                    .await?
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
                    preferred_versions,
                    dependency_groups: dependency_groups.clone(),
                    lockfile_only,
                    force,
                    prefer_offline,
                    offline,
                    pnpmfile,
                    ignore_pnpmfile,
                }
                .run()
                .await?
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

                let runtime_lockfile_config = collect_runtime_lockfile_config(
                    config,
                    manifest,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                );
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
                if let Err(reason) = satisfies_root_lockfile_config(lockfile, manifest, lockfile_dir)
                {
                    miette::bail!(
                        "Cannot install with --frozen-lockfile because pnpm-lock.yaml is not up to date with {} ({reason})",
                        manifest.path().display(),
                    );
                }

                if !force
                    && !lockfile_only
                    && persisted_install_state_is_reusable(
                        lockfile,
                        config,
                        lockfile_importer_id,
                        &dependency_groups,
                    )
                {
                    Vec::new()
                } else if !lockfile_only {
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
                        pnpmfile,
                        ignore_pnpmfile,
                    }
                    .run()
                    .await
                } else {
                    Vec::new()
                }
            }
        };

        tracing::info!(target: "pacquet::install", "Complete all");
        if !lockfile_only {
            let direct_dependency_names = manifest
                .dependencies(dependency_groups.iter().copied())
                .map(|(name, _)| name.to_string())
                .collect::<Vec<_>>();
            let current_lockfile_packages =
                Lockfile::load_from_path(&lockfile_dir.join("pnpm-lock.yaml"))
                    .ok()
                    .flatten()
                    .map(|lockfile| lockfile.packages);
            hoist_virtual_store_packages(config)?;
            if config.strict_peer_dependencies {
                validate_strict_peer_dependencies(config, lockfile_dir)?;
            }
            link_bins_for_manifest(config, manifest, dependency_groups.iter().copied())?;
            write_modules_manifest(
                &config.modules_dir,
                config,
                &dependency_groups,
                &skipped,
                current_lockfile_packages.as_ref().and_then(|packages| packages.as_ref()),
                Some(&direct_dependency_names),
            )
                .map_err(|error| miette::miette!("write node_modules/.modules.yaml: {error}"))?;
            write_pnp_manifest_if_needed(&config.node_linker, lockfile_dir)
                .map_err(|error| miette::miette!("write .pnp.cjs: {error}"))?;
        }

        Ok(skipped)
        }
        .await;

        if manage_progress_reporter {
            let progress = progress_reporter::finish(result.is_ok()).unwrap_or_default();

            if result.is_ok()
                && print_summary
                && reporter != progress_reporter::InstallReporter::Silent
            {
                print_pnpm_style_summary(
                    manifest,
                    &dependency_groups,
                    &start_time,
                    progress,
                    reporter_prefix,
                );
            }
        }

        result
    }
}

fn filter_installable_workspace_targets(
    targets: Vec<WorkspaceFrozenInstallTarget>,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    dependency_groups: &[DependencyGroup],
) -> (
    Vec<WorkspaceFrozenInstallTarget>,
    Option<HashMap<DependencyPath, PackageSnapshot>>,
    Vec<String>,
) {
    let Some(packages) = packages else {
        return (targets, None, Vec::new());
    };

    let mut filtered_packages = HashMap::<DependencyPath, PackageSnapshot>::new();
    let mut skipped = HashSet::<String>::new();
    let filtered_targets = targets
        .into_iter()
        .map(|target| {
            let (project_snapshot, target_packages, target_skipped) =
                filter_installable_optional_dependencies(
                    &target.project_snapshot,
                    Some(packages),
                    dependency_groups,
                );
            if let Some(target_packages) = target_packages {
                filtered_packages.extend(target_packages);
            }
            skipped.extend(target_skipped);
            WorkspaceFrozenInstallTarget {
                importer_id: target.importer_id,
                config: target.config,
                manifest: target.manifest,
                project_snapshot,
            }
        })
        .collect::<Vec<_>>();

    let mut skipped = skipped.into_iter().collect::<Vec<_>>();
    skipped.sort();
    (filtered_targets, Some(filtered_packages), skipped)
}

fn preserve_current_packages_for_unselected_importers(
    config: &Npmrc,
    filtered_packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
    skipped: Vec<String>,
    selected_importer_ids: &HashSet<String>,
    dependency_groups: &[DependencyGroup],
) -> (Option<HashMap<DependencyPath, PackageSnapshot>>, Vec<String>) {
    let Some(mut merged_packages) = filtered_packages else {
        return (None, skipped);
    };

    let Some(current_lockfile) =
        Lockfile::load_from_path(&config.virtual_store_dir.join("lock.yaml")).ok().flatten()
    else {
        return (Some(merged_packages), skipped);
    };

    let current_importer_ids = match &current_lockfile.project_snapshot {
        RootProjectSnapshot::Single(_) => HashSet::from([".".to_string()]),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.keys().cloned().collect(),
    };
    let extra_importer_ids =
        current_importer_ids.difference(selected_importer_ids).cloned().collect::<HashSet<_>>();
    if extra_importer_ids.is_empty() {
        return (Some(merged_packages), skipped);
    }

    let existing_skipped = read_modules_manifest(&config.modules_dir)
        .map(|manifest| manifest.skipped().iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let preserved_lockfile = current_lockfile_for_installers(
        &current_lockfile,
        &extra_importer_ids,
        dependency_groups,
        &existing_skipped,
    );

    if let Some(current_packages) = preserved_lockfile.packages {
        for (dependency_path, snapshot) in current_packages {
            merged_packages.entry(dependency_path).or_insert(snapshot);
        }
    }

    let mut merged_skipped = skipped.into_iter().collect::<HashSet<_>>();
    merged_skipped.extend(existing_skipped);
    let mut merged_skipped = merged_skipped.into_iter().collect::<Vec<_>>();
    merged_skipped.sort();

    (Some(merged_packages), merged_skipped)
}

pub fn current_lockfile_for_installers(
    lockfile: &Lockfile,
    importer_ids: &std::collections::HashSet<String>,
    dependency_groups: &[DependencyGroup],
    skipped: &std::collections::HashSet<String>,
) -> Lockfile {
    let Some(packages) = lockfile.packages.as_ref() else {
        let project_snapshot = filter_current_lockfile_project_snapshot(
            &lockfile.project_snapshot,
            importer_ids,
            dependency_groups,
            None,
        );
        return Lockfile { project_snapshot, packages: None, ..lockfile.clone() };
    };

    let mut selected_importer_ids = importer_ids.clone();
    let mut importer_queue = importer_ids.iter().cloned().collect::<Vec<_>>();
    let mut queue = Vec::<(DependencyPath, bool)>::new();
    let mut seen = std::collections::HashSet::<DependencyPath>::new();
    let mut filtered_packages = HashMap::<DependencyPath, PackageSnapshot>::new();
    while !importer_queue.is_empty() || !queue.is_empty() {
        while let Some(importer_id) = importer_queue.pop() {
            let Some(project_snapshot) =
                project_snapshot_by_id(&lockfile.project_snapshot, &importer_id)
            else {
                continue;
            };
            enqueue_project_snapshot_dependencies(
                project_snapshot,
                packages,
                &lockfile.project_snapshot,
                &mut selected_importer_ids,
                &mut importer_queue,
                &mut queue,
                dependency_groups,
            );
        }

        let Some((candidate_path, parent_installable)) = queue.pop() else {
            continue;
        };
        let Some((resolved_path, package_snapshot)) =
            resolve_package_snapshot_deduped(packages, &candidate_path)
        else {
            continue;
        };
        if !seen.insert(resolved_path.clone()) {
            continue;
        }
        let installable = parent_installable && !skipped.contains(&resolved_path.to_string());
        if !installable {
            continue;
        }
        filtered_packages.insert(resolved_path.clone(), package_snapshot.clone());
        for (alias, dependency_spec) in crate::package_dependency_map(package_snapshot) {
            if let Some(linked_importer_id) =
                importer_id_from_snapshot_dependency(&dependency_spec, &lockfile.project_snapshot)
            {
                if selected_importer_ids.insert(linked_importer_id.clone()) {
                    importer_queue.push(linked_importer_id);
                }
                continue;
            }
            if let Some(path) = dependency_path_from_snapshot_dependency(&alias, &dependency_spec) {
                queue.push((path, installable));
            }
        }
    }

    let project_snapshot = filter_current_lockfile_project_snapshot(
        &lockfile.project_snapshot,
        &selected_importer_ids,
        dependency_groups,
        Some(&filtered_packages),
    );
    Lockfile { project_snapshot, packages: Some(filtered_packages), ..lockfile.clone() }
}

pub fn current_lockfile_for_installers_preserving_unselected_importers(
    lockfile: &Lockfile,
    config: &Npmrc,
    importer_ids: &HashSet<String>,
    dependency_groups: &[DependencyGroup],
    skipped: &HashSet<String>,
) -> Lockfile {
    let mut filtered =
        current_lockfile_for_installers(lockfile, importer_ids, dependency_groups, skipped);

    let Some(existing_current_lockfile) =
        Lockfile::load_from_path(&config.virtual_store_dir.join("lock.yaml")).ok().flatten()
    else {
        return filtered;
    };

    let existing_importer_ids = match &existing_current_lockfile.project_snapshot {
        RootProjectSnapshot::Single(_) => HashSet::from([".".to_string()]),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.keys().cloned().collect(),
    };
    let extra_importer_ids =
        existing_importer_ids.difference(importer_ids).cloned().collect::<HashSet<_>>();
    if extra_importer_ids.is_empty() {
        return filtered;
    }

    let existing_skipped = read_modules_manifest(&config.modules_dir)
        .map(|manifest| manifest.skipped().iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let preserved = current_lockfile_for_installers(
        &existing_current_lockfile,
        &extra_importer_ids,
        dependency_groups,
        &existing_skipped,
    );

    match (&mut filtered.project_snapshot, preserved.project_snapshot) {
        (
            RootProjectSnapshot::Multi(filtered_snapshot),
            RootProjectSnapshot::Multi(preserved_snapshot),
        ) => {
            filtered_snapshot.importers.extend(preserved_snapshot.importers);
        }
        (RootProjectSnapshot::Single(_), RootProjectSnapshot::Single(_)) => {}
        _ => {}
    }

    if let Some(preserved_packages) = preserved.packages {
        let filtered_packages = filtered.packages.get_or_insert_with(HashMap::new);
        for (dependency_path, snapshot) in preserved_packages {
            filtered_packages.entry(dependency_path).or_insert(snapshot);
        }
    }

    filtered
}

fn project_snapshot_by_id<'a>(
    project_snapshot: &'a RootProjectSnapshot,
    importer_id: &str,
) -> Option<&'a ProjectSnapshot> {
    match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => (importer_id == ".").then_some(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(importer_id),
    }
}

fn enqueue_project_snapshot_dependencies(
    project_snapshot: &ProjectSnapshot,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    root_project_snapshot: &RootProjectSnapshot,
    selected_importer_ids: &mut std::collections::HashSet<String>,
    importer_queue: &mut Vec<String>,
    queue: &mut Vec<(DependencyPath, bool)>,
    dependency_groups: &[DependencyGroup],
) {
    for (alias, spec) in project_snapshot.dependencies_by_groups(dependency_groups.iter().copied())
    {
        if let Some(linked_importer_id) =
            importer_id_from_resolved_dependency_version(&spec.version, root_project_snapshot)
        {
            if selected_importer_ids.insert(linked_importer_id.clone()) {
                importer_queue.push(linked_importer_id);
            }
            continue;
        }
        if let Some(path) =
            direct_dependency_path_for_current_lockfile(alias, &spec.version, packages)
        {
            queue.push((path, true));
        }
    }
}

fn importer_id_from_resolved_dependency_version(
    resolved_version: &ResolvedDependencyVersion,
    project_snapshot: &RootProjectSnapshot,
) -> Option<String> {
    let ResolvedDependencyVersion::Link(link) = resolved_version else {
        return None;
    };
    importer_id_from_link(link, project_snapshot)
}

fn importer_id_from_snapshot_dependency(
    dependency_spec: &PackageSnapshotDependency,
    project_snapshot: &RootProjectSnapshot,
) -> Option<String> {
    let PackageSnapshotDependency::Link(link) = dependency_spec else {
        return None;
    };
    importer_id_from_link(link, project_snapshot)
}

fn importer_id_from_link(link: &str, project_snapshot: &RootProjectSnapshot) -> Option<String> {
    let importer_id = link.strip_prefix("link:")?;
    match project_snapshot {
        RootProjectSnapshot::Single(_) => (importer_id == ".").then(|| importer_id.to_string()),
        RootProjectSnapshot::Multi(snapshot) => {
            snapshot.importers.contains_key(importer_id).then(|| importer_id.to_string())
        }
    }
}

fn direct_dependency_path_for_current_lockfile(
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
    resolve_package_snapshot_deduped(packages, &dependency_path)
        .map(|(resolved_path, _)| resolved_path)
}

fn filter_current_lockfile_project_snapshot(
    root_project_snapshot: &RootProjectSnapshot,
    importer_ids: &std::collections::HashSet<String>,
    dependency_groups: &[DependencyGroup],
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
) -> RootProjectSnapshot {
    match root_project_snapshot {
        RootProjectSnapshot::Single(snapshot) => {
            RootProjectSnapshot::Single(filter_project_snapshot(
                snapshot,
                root_project_snapshot,
                importer_ids,
                dependency_groups,
                packages,
            ))
        }
        RootProjectSnapshot::Multi(snapshot) => {
            RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers: snapshot
                    .importers
                    .iter()
                    .filter(|(importer_id, _)| importer_ids.contains(*importer_id))
                    .map(|(importer_id, snapshot)| {
                        (
                            importer_id.clone(),
                            filter_project_snapshot(
                                snapshot,
                                root_project_snapshot,
                                importer_ids,
                                dependency_groups,
                                packages,
                            ),
                        )
                    })
                    .collect(),
            })
        }
    }
}

fn filter_project_snapshot(
    project_snapshot: &ProjectSnapshot,
    root_project_snapshot: &RootProjectSnapshot,
    importer_ids: &std::collections::HashSet<String>,
    dependency_groups: &[DependencyGroup],
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
) -> ProjectSnapshot {
    let mut filtered = project_snapshot.clone();
    filter_resolved_dependency_map(
        filtered.dependencies.as_mut(),
        root_project_snapshot,
        importer_ids,
        packages,
        dependency_groups.contains(&DependencyGroup::Prod),
    );
    filter_resolved_dependency_map(
        filtered.optional_dependencies.as_mut(),
        root_project_snapshot,
        importer_ids,
        packages,
        dependency_groups.contains(&DependencyGroup::Optional),
    );
    filter_resolved_dependency_map(
        filtered.dev_dependencies.as_mut(),
        root_project_snapshot,
        importer_ids,
        packages,
        dependency_groups.contains(&DependencyGroup::Dev),
    );

    let kept_specifiers = filtered
        .dependencies
        .iter()
        .flatten()
        .chain(filtered.optional_dependencies.iter().flatten())
        .chain(filtered.dev_dependencies.iter().flatten())
        .map(|(alias, _)| alias.to_string())
        .collect::<std::collections::HashSet<_>>();
    filtered.specifiers = project_snapshot.specifiers.as_ref().and_then(|specifiers| {
        let filtered_specifiers = specifiers
            .iter()
            .filter(|(alias, _)| kept_specifiers.contains(alias.as_str()))
            .map(|(alias, specifier)| (alias.clone(), specifier.clone()))
            .collect::<HashMap<_, _>>();
        (!filtered_specifiers.is_empty()).then_some(filtered_specifiers)
    });
    filtered
}

fn filter_resolved_dependency_map(
    map: Option<&mut HashMap<PkgName, pacquet_lockfile::ResolvedDependencySpec>>,
    root_project_snapshot: &RootProjectSnapshot,
    importer_ids: &std::collections::HashSet<String>,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    include: bool,
) {
    let Some(map) = map else {
        return;
    };
    if !include {
        map.clear();
        return;
    }
    map.retain(|alias, spec| {
        if matches!(&spec.version, ResolvedDependencyVersion::Link(link) if link.starts_with("link:")) {
            return true;
        }
        if let Some(importer_id) =
            importer_id_from_resolved_dependency_version(&spec.version, root_project_snapshot)
        {
            return importer_ids.contains(&importer_id);
        }
        let Some(packages) = packages else {
            return true;
        };
        direct_dependency_path(alias, &spec.version, packages)
            .is_some_and(|dependency_path| packages.contains_key(&dependency_path))
    });
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

fn print_pnpm_style_summary(
    manifest: &PackageManifest,
    dependency_groups: &[DependencyGroup],
    start_time: &std::time::Instant,
    progress: progress_reporter::ProgressStats,
    reporter_prefix: Option<&str>,
) {
    use std::io::Write;

    if let Some(prefix) = reporter_prefix {
        if progress.added == 0 {
            return;
        }

        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", format_prefixed_summary_stats(prefix, progress.added, 0));
        return;
    }

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

    let mut out = std::io::stdout().lock();

    if progress.added == 0 {
        let _ = writeln!(out, "Already up to date");
        let _ = writeln!(out);
    } else {
        let _ = writeln!(out, "Packages: +{}", progress.added);
        let _ = writeln!(out, "{}", "+".repeat(progress.added.min(80)));
        let _ = writeln!(out);

        for (header, deps) in &sections {
            let _ = writeln!(out, "{header}:");
            for (name, spec) in deps {
                let _ = writeln!(out, "{}", format_summary_dependency_line(name, spec));
            }
            let _ = writeln!(out);
        }
    }

    let duration_display = if elapsed_ms < 1000 {
        format!("{elapsed_ms}ms")
    } else {
        let seconds = elapsed_ms as f64 / 1000.0;
        format!("{seconds:.1}s")
    };
    let _ =
        writeln!(out, "Done in {duration_display} using pacquet v{}", env!("CARGO_PKG_VERSION"));
}

pub fn format_prefixed_summary_stats(prefix: &str, added: usize, removed: usize) -> String {
    let stats = match (added, removed) {
        (0, 0) => String::new(),
        (added, 0) => format!("+{added} {}", "+".repeat(added.min(12))),
        (0, removed) => format!("-{removed} {}", "-".repeat(removed.min(12))),
        (added, removed) => {
            let total = added + removed;
            let plus_count = ((9 * added) / total).max(1);
            let minus_count = 9usize.saturating_sub(plus_count).max(1);
            let added_count = format!("+{added}");
            let removed_count = format!("-{removed}");
            if added >= removed {
                format!(
                    "{added_count} {:>5} {}{}",
                    removed_count,
                    "+".repeat(plus_count),
                    "-".repeat(minus_count)
                )
            } else {
                format!(
                    "{:>5} {removed_count} {}{}",
                    added_count,
                    "+".repeat(plus_count),
                    "-".repeat(minus_count)
                )
            }
        }
    };
    format!("{:<41} | {stats}", progress_reporter::format_prefix_label(prefix))
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

fn direct_workspace_links_match_current_config(
    config: &Npmrc,
    manifest: &PackageManifest,
    project_snapshot: &pacquet_lockfile::ProjectSnapshot,
    workspace_packages: &WorkspacePackages,
    dependency_groups: &[DependencyGroup],
) -> bool {
    let project_dir = manifest.path().parent().unwrap_or_else(|| Path::new("."));
    manifest.dependencies(dependency_groups.iter().copied()).all(|(name, specifier)| {
        let Some(workspace_package) = expected_workspace_dependency_for_install(
            config,
            workspace_packages,
            name,
            specifier,
            0,
        ) else {
            return true;
        };

        let Some((_, resolved)) = project_snapshot
            .dependencies_by_groups(dependency_groups.iter().copied())
            .find(|(pkg_name, _)| pkg_name.to_string() == name)
        else {
            return false;
        };

        let expected_link =
            expected_workspace_link_reference(project_dir, &workspace_package.root_dir);
        matches!(
            &resolved.version,
            ResolvedDependencyVersion::Link(link) if link == &expected_link
        )
    })
}

pub fn expected_workspace_dependency_for_install<'a>(
    config: &Npmrc,
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
    depth: usize,
) -> Option<&'a crate::WorkspacePackageInfo> {
    if specifier.starts_with("workspace:") {
        return crate::workspace_packages::require_workspace_dependency(
            workspace_packages,
            dependency_name,
            specifier,
        )
        .ok();
    }

    if !config.link_workspace_packages.links_at_depth(depth) {
        return None;
    }

    let (target_name, target_specifier) =
        parse_npm_alias_workspace_target(specifier).unwrap_or((dependency_name, specifier));
    crate::workspace_packages::resolve_workspace_dependency_by_plain_spec(
        workspace_packages,
        target_name,
        target_specifier,
    )
}

fn parse_npm_alias_workspace_target(version_range: &str) -> Option<(&str, &str)> {
    let alias = version_range.strip_prefix("npm:")?;
    let separator = alias.rfind('@');
    match separator {
        Some(index) if index > 0 => Some((&alias[..index], &alias[index + 1..])),
        _ => Some((alias, "latest")),
    }
}

fn expected_workspace_link_reference(project_dir: &Path, workspace_root_dir: &Path) -> String {
    format!("link:{}", to_relative_path(project_dir, workspace_root_dir).replace('\\', "/"))
}

fn to_relative_path(from: &Path, to: &Path) -> String {
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();

    let common_len = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(left, right)| left == right)
        .count();

    if common_len == 0 {
        return to.to_string_lossy().into_owned();
    }

    let mut relative_parts = Vec::<String>::new();
    for _ in common_len..from_components.len() {
        relative_parts.push("..".to_string());
    }
    for component in to_components.iter().skip(common_len) {
        relative_parts.push(component.as_os_str().to_string_lossy().into_owned());
    }

    if relative_parts.is_empty() { ".".to_string() } else { relative_parts.join("/") }
}

fn persisted_install_state_is_reusable(
    lockfile: &Lockfile,
    config: &Npmrc,
    importer_id: &str,
    dependency_groups: &[DependencyGroup],
) -> bool {
    let modules_manifest = read_modules_manifest(&config.modules_dir);
    let current_lockfile =
        Lockfile::load_from_path(&config.virtual_store_dir.join("lock.yaml")).ok().flatten();

    match (modules_manifest, current_lockfile) {
        (None, None) => false,
        (Some(modules_manifest), Some(current_lockfile)) => {
            if !modules_manifest_is_compatible(&modules_manifest, config, dependency_groups) {
                return false;
            }

            let expected_current_lockfile = current_lockfile_for_installers(
                lockfile,
                &std::collections::HashSet::from([importer_id.to_string()]),
                dependency_groups,
                &modules_manifest
                    .skipped()
                    .iter()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>(),
            );

            let matches_current_lockfile = current_lockfile_matches_expected(
                &current_lockfile,
                &expected_current_lockfile,
                importer_id,
            );
            if !matches_current_lockfile {
                return false;
            }

            let Some(project_snapshot) =
                snapshot_for_importer(&expected_current_lockfile.project_snapshot, importer_id)
            else {
                return false;
            };

            importer_dependencies_ready(
                config,
                project_snapshot,
                expected_current_lockfile.packages.as_ref(),
                dependency_groups.iter().copied(),
            ) && direct_root_dependencies_ready(
                config,
                project_snapshot,
                expected_current_lockfile.packages.as_ref(),
                dependency_groups,
            )
        }
        _ => false,
    }
}

fn direct_root_dependencies_ready(
    config: &Npmrc,
    project_snapshot: &pacquet_lockfile::ProjectSnapshot,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    dependency_groups: &[DependencyGroup],
) -> bool {
    project_snapshot.dependencies_by_groups(dependency_groups.iter().copied()).all(
        |(alias, spec)| {
            let dependency_path = config.modules_dir.join(alias.to_string());
            if dependency_path.exists() {
                return true;
            }

            direct_dependency_virtual_store_location(alias, &spec.version, packages).is_none()
        },
    )
}

fn recreate_modules_dir_if_incompatible(
    config: &Npmrc,
    dependency_groups: &[DependencyGroup],
) -> std::io::Result<bool> {
    let Some(modules_manifest) = read_modules_manifest(&config.modules_dir) else {
        return Ok(false);
    };
    if modules_manifest_is_compatible(&modules_manifest, config, dependency_groups) {
        return Ok(false);
    }

    let Ok(entries) = fs::read_dir(&config.modules_dir) else {
        return Ok(false);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if name.starts_with('.')
            && name != ".bin"
            && name != ".modules.yaml"
            && path != config.virtual_store_dir
        {
            continue;
        }

        if file_type.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(true)
}

fn modules_manifest_is_compatible(
    modules_manifest: &crate::ModulesManifest,
    config: &Npmrc,
    dependency_groups: &[DependencyGroup],
) -> bool {
    if modules_manifest.node_linker() != Some(node_linker_name(config)) {
        return false;
    }
    let expected_store_dir =
        fs::canonicalize(PathBuf::from(config.store_dir.display().to_string()).join("v10"))
            .unwrap_or_else(|_| PathBuf::from(config.store_dir.display().to_string()).join("v10"))
            .display()
            .to_string();
    if modules_manifest.store_dir() != Some(expected_store_dir.as_str()) {
        return false;
    }
    if modules_manifest.resolved_virtual_store_dir(&config.modules_dir) != config.virtual_store_dir
    {
        return false;
    }
    if modules_manifest.included() != Some(&included_dependencies(dependency_groups)) {
        return false;
    }
    if modules_manifest.hoist_pattern().unwrap_or(&[]) != config.hoist_pattern {
        return false;
    }
    if modules_manifest.public_hoist_pattern().unwrap_or(&[]) != config.public_hoist_pattern {
        return false;
    }
    modules_manifest.virtual_store_dir_max_length() == crate::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH
}

fn current_lockfile_matches_expected(
    current_lockfile: &Lockfile,
    expected_current_lockfile: &Lockfile,
    importer_id: &str,
) -> bool {
    current_lockfile.lockfile_version == expected_current_lockfile.lockfile_version
        && current_lockfile.settings == expected_current_lockfile.settings
        && current_lockfile.never_built_dependencies
            == expected_current_lockfile.never_built_dependencies
        && current_lockfile.ignored_optional_dependencies
            == expected_current_lockfile.ignored_optional_dependencies
        && current_lockfile.overrides == expected_current_lockfile.overrides
        && current_lockfile.package_extensions_checksum
            == expected_current_lockfile.package_extensions_checksum
        && current_lockfile.patched_dependencies == expected_current_lockfile.patched_dependencies
        && current_lockfile.pnpmfile_checksum == expected_current_lockfile.pnpmfile_checksum
        && current_lockfile.catalogs == expected_current_lockfile.catalogs
        && current_lockfile.time == expected_current_lockfile.time
        && current_lockfile.extra_fields == expected_current_lockfile.extra_fields
        && current_lockfile_contains_expected_packages(current_lockfile, expected_current_lockfile)
        && snapshot_for_importer(&current_lockfile.project_snapshot, importer_id)
            == snapshot_for_importer(&expected_current_lockfile.project_snapshot, importer_id)
}

fn current_lockfile_contains_expected_packages(
    current_lockfile: &Lockfile,
    expected_current_lockfile: &Lockfile,
) -> bool {
    match (&current_lockfile.packages, &expected_current_lockfile.packages) {
        (_, None) => true,
        (Some(current), Some(expected)) => {
            expected.iter().all(|(path, snapshot)| current.get(path) == Some(snapshot))
        }
        _ => false,
    }
}

fn snapshot_for_importer<'a>(
    snapshot: &'a RootProjectSnapshot,
    importer_id: &str,
) -> Option<&'a pacquet_lockfile::ProjectSnapshot> {
    match snapshot {
        RootProjectSnapshot::Single(snapshot) => (importer_id == ".").then_some(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(importer_id),
    }
}

fn node_linker_name(config: &Npmrc) -> &'static str {
    match config.node_linker {
        pacquet_npmrc::NodeLinker::Hoisted => "hoisted",
        pacquet_npmrc::NodeLinker::Isolated => "isolated",
        pacquet_npmrc::NodeLinker::Pnp => "pnp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::ProjectSnapshot;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::{DependencyGroup, PackageManifest};
    use pacquet_registry_mock::AutoMockInstance;
    use pacquet_testing_utils::fs::{
        get_all_folders, get_filenames_in_folder, is_symlink_or_junction,
    };
    use std::{collections::HashMap, fs, path::Path};
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
                    preferred_versions: None,
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
                    pnpmfile: None,
                    ignore_pnpmfile: false,
                    reporter_prefix: None,
                    reporter: progress_reporter::InstallReporter::Default,
                    print_summary: true,
                    manage_progress_reporter: true,
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
                    preferred_versions: None,
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
                    pnpmfile: None,
                    ignore_pnpmfile: false,
                    reporter_prefix: None,
                    reporter: progress_reporter::InstallReporter::Default,
                    print_summary: true,
                    manage_progress_reporter: true,
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
    fn recreate_modules_dir_if_incompatible_removes_pnpm_managed_entries() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create virtual store package");
        fs::create_dir_all(modules_dir.join("dep")).expect("create root package");
        fs::write(modules_dir.join(".keep"), "keep").expect("write keep file");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        config.node_linker = pacquet_npmrc::NodeLinker::Hoisted;
        assert!(
            recreate_modules_dir_if_incompatible(&config, &[DependencyGroup::Prod])
                .expect("recreate modules dir")
        );

        assert!(modules_dir.join(".keep").exists());
        assert!(!modules_dir.join(".modules.yaml").exists());
        assert!(!modules_dir.join("dep").exists());
        assert!(!virtual_store_dir.exists());
    }

    #[test]
    fn recreate_modules_dir_if_incompatible_keeps_compatible_layout() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create virtual store package");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(
            !recreate_modules_dir_if_incompatible(&config, &[DependencyGroup::Prod])
                .expect("check modules dir compatibility")
        );
        assert!(modules_dir.join(".modules.yaml").exists());
        assert!(virtual_store_dir.exists());
    }

    #[test]
    fn recreate_modules_dir_if_incompatible_when_hoist_pattern_changes() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create virtual store package");
        fs::create_dir_all(modules_dir.join("dep")).expect("create root package");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        config.hoist_pattern = vec!["*eslint*".to_string()];
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        config.hoist_pattern = vec!["*babel*".to_string()];
        assert!(
            recreate_modules_dir_if_incompatible(&config, &[DependencyGroup::Prod])
                .expect("recreate modules dir")
        );
        assert!(!modules_dir.join("dep").exists());
        assert!(!virtual_store_dir.exists());
    }

    #[test]
    fn recreate_modules_dir_if_incompatible_when_registry_config_changes() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create virtual store package");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        config.registry = "https://default.example/".to_string();
        config.set_raw_setting("@foo:registry", "https://foo.example");
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        config.set_raw_setting("@foo:registry", "https://bar.example");
        assert!(
            !recreate_modules_dir_if_incompatible(&config, &[DependencyGroup::Prod])
                .expect("recreate modules dir")
        );
        assert!(virtual_store_dir.exists());
    }

    #[test]
    fn recreate_modules_dir_if_incompatible_when_virtual_store_dir_max_length_changes() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create virtual store package");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");
        let manifest_path = modules_dir.join(".modules.yaml");
        let content = fs::read_to_string(&manifest_path).expect("read modules manifest");
        fs::write(
            &manifest_path,
            content
                .replace("\"virtualStoreDirMaxLength\": 60", "\"virtualStoreDirMaxLength\": 64")
                .replace("\"virtualStoreDirMaxLength\": 120", "\"virtualStoreDirMaxLength\": 64"),
        )
        .expect("rewrite modules manifest");

        assert!(
            recreate_modules_dir_if_incompatible(&config, &[DependencyGroup::Prod])
                .expect("recreate modules dir")
        );
        assert!(!virtual_store_dir.exists());
    }

    #[test]
    fn persisted_install_state_is_reusable_when_modules_manifest_and_current_lockfile_match() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create package in virtual store");
        fs::create_dir_all(modules_dir.join("dep")).expect("create direct dependency");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));

        let wanted_lockfile_path = dir.path().join("pnpm-lock.yaml");
        lockfile_with_dependency("dep", DependencyGroup::Prod, false)
            .save_to_path(&wanted_lockfile_path)
            .expect("write wanted lockfile");
        let lockfile = Lockfile::load_from_path(&wanted_lockfile_path)
            .expect("load wanted lockfile")
            .expect("wanted lockfile should exist");
        let current_lockfile = current_lockfile_for_installers(
            &lockfile,
            &std::collections::HashSet::from([".".to_string()]),
            &[DependencyGroup::Prod],
            &std::collections::HashSet::new(),
        );
        current_lockfile
            .save_to_path(&virtual_store_dir.join("lock.yaml"))
            .expect("write current lockfile");
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(persisted_install_state_is_reusable(
            &lockfile,
            &config,
            ".",
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn filter_installable_workspace_targets_unions_importer_packages() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let root_manifest_path = workspace_root.join("package.json");
        let app_manifest_path = workspace_root.join("packages/app/package.json");
        fs::create_dir_all(app_manifest_path.parent().expect("app parent")).expect("create app");

        let root_manifest =
            PackageManifest::create_if_needed(root_manifest_path).expect("create root manifest");
        let app_manifest =
            PackageManifest::create_if_needed(app_manifest_path).expect("create app manifest");

        let targets = vec![
            WorkspaceFrozenInstallTarget {
                importer_id: ".".to_string(),
                config: Npmrc::new().leak(),
                manifest: root_manifest,
                project_snapshot: serde_yaml::from_str(
                    "dependencies:\n  is-positive:\n    specifier: 1.0.0\n    version: 1.0.0\n",
                )
                .expect("root project snapshot"),
            },
            WorkspaceFrozenInstallTarget {
                importer_id: "packages/app".to_string(),
                config: Npmrc::new().leak(),
                manifest: app_manifest,
                project_snapshot: serde_yaml::from_str(
                    "dependencies:\n  is-negative:\n    specifier: 1.0.0\n    version: 1.0.0\n",
                )
                .expect("app project snapshot"),
            },
        ];

        let packages = HashMap::from([
            (
                pacquet_lockfile::DependencyPath::registry(
                    None,
                    "is-positive@1.0.0".parse().expect("positive dep path"),
                ),
                PackageSnapshot {
                    resolution: pacquet_lockfile::LockfileResolution::Registry(
                        pacquet_lockfile::RegistryResolution {
                            integrity: "sha512-Bw==".parse().expect("integrity"),
                        },
                    ),
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
                },
            ),
            (
                pacquet_lockfile::DependencyPath::registry(
                    None,
                    "is-negative@1.0.0".parse().expect("negative dep path"),
                ),
                PackageSnapshot {
                    resolution: pacquet_lockfile::LockfileResolution::Registry(
                        pacquet_lockfile::RegistryResolution {
                            integrity: "sha512-Bw==".parse().expect("integrity"),
                        },
                    ),
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
                },
            ),
        ]);

        let (filtered_targets, filtered_packages, skipped) = filter_installable_workspace_targets(
            targets,
            Some(&packages),
            &[DependencyGroup::Prod],
        );

        assert_eq!(
            filtered_targets.iter().map(|target| target.importer_id.as_str()).collect::<Vec<_>>(),
            vec![".", "packages/app"]
        );
        assert!(skipped.is_empty());
        let filtered_packages = filtered_packages.expect("filtered packages");
        assert_eq!(filtered_packages.len(), 2);
    }

    #[test]
    fn direct_workspace_links_match_current_config_requires_relink_for_registry_and_alias_specs() {
        use pacquet_lockfile::{ResolvedDependencyMap, ResolvedDependencySpec};

        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_manifest_path = workspace_root.join("project/package.json");
        fs::create_dir_all(project_manifest_path.parent().expect("project parent"))
            .expect("create project parent");

        let mut manifest =
            PackageManifest::create_if_needed(project_manifest_path).expect("create manifest");
        manifest
            .add_dependency("is-positive", "2.0.0", DependencyGroup::Prod)
            .expect("add positive dependency");
        manifest
            .add_dependency("negative", "npm:is-negative@1.0.0", DependencyGroup::Prod)
            .expect("add negative alias dependency");
        manifest.save().expect("save manifest");

        let workspace_packages = HashMap::from([
            (
                "is-positive".to_string(),
                crate::WorkspacePackageInfo {
                    root_dir: workspace_root.join("is-positive"),
                    version: "2.0.0".to_string(),
                },
            ),
            (
                "is-negative".to_string(),
                crate::WorkspacePackageInfo {
                    root_dir: workspace_root.join("is-negative"),
                    version: "1.0.0".to_string(),
                },
            ),
        ]);

        let mut config = Npmrc::new();
        config.link_workspace_packages = pacquet_npmrc::LinkWorkspacePackages::Direct;

        let registry_snapshot = pacquet_lockfile::ProjectSnapshot {
            dependencies: Some(ResolvedDependencyMap::from([
                (
                    "is-positive".parse().expect("positive alias"),
                    ResolvedDependencySpec {
                        specifier: "2.0.0".to_string(),
                        version: ResolvedDependencyVersion::PkgVerPeer(
                            "2.0.0".parse().expect("positive version"),
                        ),
                    },
                ),
                (
                    "negative".parse().expect("negative alias"),
                    ResolvedDependencySpec {
                        specifier: "npm:is-negative@1.0.0".to_string(),
                        version: ResolvedDependencyVersion::PkgNameVerPeer(
                            "is-negative@1.0.0".parse().expect("negative version"),
                        ),
                    },
                ),
            ])),
            ..Default::default()
        };

        assert!(!direct_workspace_links_match_current_config(
            &config,
            &manifest,
            &registry_snapshot,
            &workspace_packages,
            &[DependencyGroup::Prod],
        ));

        let linked_snapshot = pacquet_lockfile::ProjectSnapshot {
            dependencies: Some(ResolvedDependencyMap::from([
                (
                    "is-positive".parse().expect("positive alias"),
                    ResolvedDependencySpec {
                        specifier: "2.0.0".to_string(),
                        version: ResolvedDependencyVersion::Link("link:../is-positive".to_string()),
                    },
                ),
                (
                    "negative".parse().expect("negative alias"),
                    ResolvedDependencySpec {
                        specifier: "npm:is-negative@1.0.0".to_string(),
                        version: ResolvedDependencyVersion::Link("link:../is-negative".to_string()),
                    },
                ),
            ])),
            ..Default::default()
        };

        assert!(direct_workspace_links_match_current_config(
            &config,
            &manifest,
            &linked_snapshot,
            &workspace_packages,
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn persisted_install_state_is_reusable_when_current_lockfile_contains_extra_importers() {
        use pacquet_lockfile::ResolvedDependencyMap;

        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("packages/app/node_modules");
        let virtual_store_dir = dir.path().join("node_modules/.pnpm");
        fs::create_dir_all(virtual_store_dir.join("dep@1.0.0/node_modules/dep"))
            .expect("create package in virtual store");
        fs::create_dir_all(modules_dir.join("dep")).expect("create direct dependency");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));

        let mut importers = std::collections::HashMap::new();
        importers.insert(
            "packages/app".to_string(),
            pacquet_lockfile::ProjectSnapshot {
                dependencies: Some(ResolvedDependencyMap::from([(
                    "dep".parse().expect("alias"),
                    pacquet_lockfile::ResolvedDependencySpec {
                        specifier: "^1.0.0".to_string(),
                        version: ResolvedDependencyVersion::PkgVerPeer(
                            "1.0.0".parse().expect("version"),
                        ),
                    },
                )])),
                ..Default::default()
            },
        );
        importers
            .insert("packages/other".to_string(), pacquet_lockfile::ProjectSnapshot::default());
        let lockfile = Lockfile {
            lockfile_version: "9.0".parse().expect("lockfile version"),
            settings: None,
            project_snapshot: RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers,
            }),
            packages: Some(HashMap::from([(
                pacquet_lockfile::DependencyPath::registry(
                    None,
                    "dep@1.0.0".parse().expect("dep path"),
                ),
                PackageSnapshot {
                    resolution: pacquet_lockfile::LockfileResolution::Registry(
                        pacquet_lockfile::RegistryResolution {
                            integrity: "sha512-Bw==".parse().expect("integrity"),
                        },
                    ),
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
                },
            )])),
            never_built_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            ignored_optional_dependencies: None,
            extra_fields: Default::default(),
        };
        let wanted_lockfile_path = dir.path().join("pnpm-lock.yaml");
        lockfile.save_to_path(&wanted_lockfile_path).expect("write wanted lockfile");
        let lockfile =
            Lockfile::load_from_path(&wanted_lockfile_path).expect("load wanted lockfile");
        let lockfile = lockfile.expect("wanted lockfile should exist");
        let mut current_lockfile = current_lockfile_for_installers(
            &lockfile,
            &std::collections::HashSet::from(["packages/app".to_string()]),
            &[DependencyGroup::Prod],
            &std::collections::HashSet::new(),
        );
        if let RootProjectSnapshot::Multi(snapshot) = &mut current_lockfile.project_snapshot {
            snapshot
                .importers
                .insert("packages/other".to_string(), pacquet_lockfile::ProjectSnapshot::default());
        }
        current_lockfile
            .save_to_path(&virtual_store_dir.join("lock.yaml"))
            .expect("write current lockfile");
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(persisted_install_state_is_reusable(
            &lockfile,
            &config,
            "packages/app",
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn persisted_install_state_is_not_reusable_when_virtual_store_package_is_missing() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(&virtual_store_dir).expect("create virtual store dir");
        fs::create_dir_all(modules_dir.join("dep")).expect("create direct dependency");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));

        let lockfile = lockfile_with_dependency("dep", DependencyGroup::Prod, false);
        let current_lockfile = current_lockfile_for_installers(
            &lockfile,
            &std::collections::HashSet::from([".".to_string()]),
            &[DependencyGroup::Prod],
            &std::collections::HashSet::new(),
        );
        current_lockfile
            .save_to_path(&virtual_store_dir.join("lock.yaml"))
            .expect("write current lockfile");
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(!persisted_install_state_is_reusable(
            &lockfile,
            &config,
            ".",
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn persisted_install_state_is_not_reusable_when_current_lockfile_differs() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(&virtual_store_dir).expect("create virtual store dir");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));

        let lockfile = lockfile_with_dependency("dep", DependencyGroup::Prod, false);
        Lockfile { packages: Some(HashMap::new()), ..lockfile.clone() }
            .save_to_path(&virtual_store_dir.join("lock.yaml"))
            .expect("write mismatched current lockfile");
        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(!persisted_install_state_is_reusable(
            &lockfile,
            &config,
            ".",
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn persisted_install_state_is_not_reusable_when_current_lockfile_is_missing() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(&virtual_store_dir).expect("create virtual store dir");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir;
        config.virtual_store_dir = virtual_store_dir;
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));

        let lockfile = lockfile_with_dependency("dep", DependencyGroup::Prod, false);

        assert!(!persisted_install_state_is_reusable(
            &lockfile,
            &config,
            ".",
            &[DependencyGroup::Prod],
        ));
    }

    #[test]
    fn preserve_current_packages_for_unselected_importers_keeps_current_workspace_subset_packages()
    {
        use pacquet_lockfile::ResolvedDependencyMap;

        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        fs::create_dir_all(&virtual_store_dir).expect("create virtual store dir");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir.clone();
        config.store_dir = pacquet_store_dir::StoreDir::new(dir.path().join("store"));
        write_modules_manifest(
            &modules_dir,
            &config,
            &[DependencyGroup::Prod],
            &["skipped-from-current".to_string()],
            None,
            None,
        )
        .expect("write modules manifest");

        let app_dep_path =
            DependencyPath::registry(None, "dep-a@1.0.0".parse().expect("app dep path"));
        let lib_dep_path =
            DependencyPath::registry(None, "dep-b@1.0.0".parse().expect("lib dep path"));

        let registry_snapshot = |integrity: &str| PackageSnapshot {
            resolution: pacquet_lockfile::LockfileResolution::Registry(
                pacquet_lockfile::RegistryResolution {
                    integrity: integrity.parse().expect("integrity"),
                },
            ),
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
        };

        let current_lockfile = Lockfile {
            lockfile_version: "9.0".parse().expect("lockfile version"),
            settings: None,
            project_snapshot: RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers: HashMap::from([
                    (
                        "packages/app".to_string(),
                        pacquet_lockfile::ProjectSnapshot {
                            dependencies: Some(ResolvedDependencyMap::from([(
                                "dep-a".parse().expect("dep-a alias"),
                                pacquet_lockfile::ResolvedDependencySpec {
                                    specifier: "1.0.0".to_string(),
                                    version: ResolvedDependencyVersion::PkgVerPeer(
                                        "1.0.0".parse().expect("dep-a version"),
                                    ),
                                },
                            )])),
                            ..Default::default()
                        },
                    ),
                    (
                        "packages/lib".to_string(),
                        pacquet_lockfile::ProjectSnapshot {
                            dependencies: Some(ResolvedDependencyMap::from([(
                                "dep-b".parse().expect("dep-b alias"),
                                pacquet_lockfile::ResolvedDependencySpec {
                                    specifier: "1.0.0".to_string(),
                                    version: ResolvedDependencyVersion::PkgVerPeer(
                                        "1.0.0".parse().expect("dep-b version"),
                                    ),
                                },
                            )])),
                            ..Default::default()
                        },
                    ),
                ]),
            }),
            packages: Some(HashMap::from([
                (app_dep_path.clone(), registry_snapshot("sha512-Bw==")),
                (lib_dep_path.clone(), registry_snapshot("sha512-CA==")),
            ])),
            never_built_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            ignored_optional_dependencies: None,
            extra_fields: Default::default(),
        };
        current_lockfile
            .save_to_path(&virtual_store_dir.join("lock.yaml"))
            .expect("write current lockfile");

        let (packages, skipped) = preserve_current_packages_for_unselected_importers(
            &config,
            Some(HashMap::from([(app_dep_path.clone(), registry_snapshot("sha512-Bw=="))])),
            Vec::new(),
            &HashSet::from(["packages/app".to_string()]),
            &[DependencyGroup::Prod],
        );

        let packages = packages.expect("merged packages");
        assert!(packages.contains_key(&app_dep_path));
        assert!(packages.contains_key(&lib_dep_path));
        assert_eq!(skipped, vec!["skipped-from-current".to_string()]);
    }

    #[test]
    fn current_lockfile_for_installers_contains_only_selected_importer_packages() {
        use pacquet_lockfile::ResolvedDependencyMap;

        let dep_a_path =
            DependencyPath::registry(None, "dep-a@1.0.0".parse().expect("dep-a dependency path"));
        let dep_b_path =
            DependencyPath::registry(None, "dep-b@1.0.0".parse().expect("dep-b dependency path"));

        let registry_snapshot = |integrity: &str| PackageSnapshot {
            resolution: pacquet_lockfile::LockfileResolution::Registry(
                pacquet_lockfile::RegistryResolution {
                    integrity: integrity.parse().expect("integrity"),
                },
            ),
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
        };

        let lockfile = Lockfile {
            lockfile_version: "9.0".parse().expect("lockfile version"),
            settings: None,
            project_snapshot: RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers: HashMap::from([
                    (
                        "packages/project-1".to_string(),
                        pacquet_lockfile::ProjectSnapshot {
                            dependencies: Some(ResolvedDependencyMap::from([(
                                "dep-a".parse().expect("dep-a alias"),
                                pacquet_lockfile::ResolvedDependencySpec {
                                    specifier: "1.0.0".to_string(),
                                    version: ResolvedDependencyVersion::PkgVerPeer(
                                        "1.0.0".parse().expect("dep-a version"),
                                    ),
                                },
                            )])),
                            ..Default::default()
                        },
                    ),
                    (
                        "packages/project-2".to_string(),
                        pacquet_lockfile::ProjectSnapshot {
                            dependencies: Some(ResolvedDependencyMap::from([(
                                "dep-b".parse().expect("dep-b alias"),
                                pacquet_lockfile::ResolvedDependencySpec {
                                    specifier: "1.0.0".to_string(),
                                    version: ResolvedDependencyVersion::PkgVerPeer(
                                        "1.0.0".parse().expect("dep-b version"),
                                    ),
                                },
                            )])),
                            ..Default::default()
                        },
                    ),
                ]),
            }),
            packages: Some(HashMap::from([
                (dep_a_path.clone(), registry_snapshot("sha512-Bw==")),
                (dep_b_path.clone(), registry_snapshot("sha512-CA==")),
            ])),
            never_built_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            ignored_optional_dependencies: None,
            extra_fields: Default::default(),
        };

        let filtered = current_lockfile_for_installers(
            &lockfile,
            &HashSet::from(["packages/project-2".to_string()]),
            &[DependencyGroup::Prod],
            &HashSet::new(),
        );

        let RootProjectSnapshot::Multi(filtered_snapshot) = &filtered.project_snapshot else {
            panic!("expected multi-importer current lockfile");
        };
        assert_eq!(
            filtered_snapshot.importers.keys().collect::<Vec<_>>(),
            vec![&"packages/project-2".to_string()]
        );

        let filtered_packages = filtered.packages.expect("filtered packages");
        assert_eq!(filtered_packages.len(), 1);
        assert!(filtered_packages.contains_key(&dep_b_path));
        assert!(!filtered_packages.contains_key(&dep_a_path));
    }

    #[test]
    fn current_lockfile_for_installers_preserves_relative_workspace_link_importers() {
        let lockfile = Lockfile {
            lockfile_version: "9.0".parse().expect("lockfile version"),
            settings: None,
            project_snapshot: RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers: HashMap::from([
                    (
                        "bin".to_string(),
                        pacquet_lockfile::ProjectSnapshot {
                            dependencies: Some(HashMap::from([(
                                "ep_etherpad-lite".parse().expect("alias"),
                                pacquet_lockfile::ResolvedDependencySpec {
                                    specifier: "workspace:../src".to_string(),
                                    version: ResolvedDependencyVersion::Link(
                                        "link:../src".to_string(),
                                    ),
                                },
                            )])),
                            specifiers: Some(HashMap::from([(
                                "ep_etherpad-lite".parse().expect("specifier alias"),
                                "workspace:../src".to_string(),
                            )])),
                            ..ProjectSnapshot::default()
                        },
                    ),
                    ("src".to_string(), ProjectSnapshot::default()),
                ]),
            }),
            packages: Some(HashMap::new()),
            never_built_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            ignored_optional_dependencies: None,
            extra_fields: Default::default(),
        };

        let filtered = current_lockfile_for_installers(
            &lockfile,
            &HashSet::from(["bin".to_string(), "src".to_string()]),
            &[DependencyGroup::Prod],
            &HashSet::new(),
        );

        let RootProjectSnapshot::Multi(filtered_snapshot) = filtered.project_snapshot else {
            panic!("expected multi-importer current lockfile");
        };
        let bin = filtered_snapshot.importers.get("bin").expect("bin importer");
        let alias: PkgName = "ep_etherpad-lite".parse().expect("pkg name");
        let dep = bin
            .dependencies
            .as_ref()
            .and_then(|deps| deps.get(&alias))
            .expect("workspace link dependency");
        assert_eq!(dep.specifier, "workspace:../src");
        assert_eq!(dep.version, ResolvedDependencyVersion::Link("link:../src".to_string()));
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

    #[test]
    fn prefixed_summary_stats_truncates_long_prefix_like_pnpm() {
        let line =
            format_prefixed_summary_stats("loooooooooooooooooooooooooooooooooong-pkg-4", 0, 1);
        assert!(line.starts_with("..."));
        assert!(line.contains("ong-pkg-4"));
        assert!(line.contains("|"));
    }

    #[test]
    fn prefixed_summary_stats_caps_single_stat_symbol_bar() {
        let line = format_prefixed_summary_stats("pkg-1", 190, 0);
        assert_eq!(line, "pkg-1                                     | +190 ++++++++++++");
    }

    #[test]
    fn prefixed_summary_stats_balances_added_and_removed_symbols() {
        let line = format_prefixed_summary_stats("pkg-1", 100, 1);
        assert_eq!(line, "pkg-1                                     | +100    -1 ++++++++-");
    }

    #[test]
    fn prefixed_summary_stats_right_aligns_small_add_count_when_removed_dominates() {
        let line = format_prefixed_summary_stats("pkg-1", 1, 100);
        assert_eq!(line, "pkg-1                                     |    +1 -100 +--------");
    }

    #[test]
    fn current_lockfile_for_installers_omits_skipped_packages() {
        let dependency_path = DependencyPath::registry(
            None,
            PkgNameVerPeer::new("opt".parse().expect("name"), "1.0.0".parse().expect("version")),
        );
        let lockfile = lockfile_with_dependency("opt", DependencyGroup::Optional, true);

        let filtered = current_lockfile_for_installers(
            &lockfile,
            &std::collections::HashSet::from([".".to_string()]),
            &[DependencyGroup::Optional],
            &std::collections::HashSet::from([dependency_path.to_string()]),
        );

        assert!(matches!(filtered.project_snapshot, RootProjectSnapshot::Multi(_)));
        assert!(filtered.packages.expect("packages").is_empty());
    }

    fn load_manifest(dir: &Path, value: serde_json::Value) -> PackageManifest {
        let manifest_path = dir.join("package.json");
        fs::write(&manifest_path, value.to_string()).expect("write package.json");
        PackageManifest::from_path(manifest_path).expect("load manifest")
    }

    fn lockfile_with_dependency(
        name: &str,
        dependency_group: DependencyGroup,
        optional: bool,
    ) -> Lockfile {
        let pkg_name: PkgName = name.parse().expect("name");
        let dependency_path = DependencyPath::registry(
            None,
            PkgNameVerPeer::new(pkg_name.clone(), "1.0.0".parse().expect("version")),
        );
        let resolved_spec = pacquet_lockfile::ResolvedDependencySpec {
            specifier: "^1.0.0".to_string(),
            version: ResolvedDependencyVersion::PkgVerPeer("1.0.0".parse().expect("version")),
        };
        let importer_snapshot = match dependency_group {
            DependencyGroup::Prod => ProjectSnapshot {
                dependencies: Some(HashMap::from([(pkg_name, resolved_spec)])),
                ..ProjectSnapshot::default()
            },
            DependencyGroup::Dev => ProjectSnapshot {
                dev_dependencies: Some(HashMap::from([(pkg_name, resolved_spec)])),
                ..ProjectSnapshot::default()
            },
            DependencyGroup::Optional => ProjectSnapshot {
                optional_dependencies: Some(HashMap::from([(pkg_name, resolved_spec)])),
                ..ProjectSnapshot::default()
            },
            DependencyGroup::Peer => ProjectSnapshot::default(),
        };
        Lockfile {
            lockfile_version: pacquet_lockfile::ComVer::new(9, 0),
            settings: None,
            never_built_dependencies: None,
            ignored_optional_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            extra_fields: HashMap::new(),
            project_snapshot: RootProjectSnapshot::Multi(pacquet_lockfile::MultiProjectSnapshot {
                importers: HashMap::from([(".".to_string(), importer_snapshot)]),
            }),
            packages: Some(HashMap::from([(
                dependency_path,
                PackageSnapshot {
                    resolution: pacquet_lockfile::LockfileResolution::Registry(
                        pacquet_lockfile::RegistryResolution {
                            integrity: "sha512-Bw==".parse().expect("integrity"),
                        },
                    ),
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
                    optional: Some(optional),
                },
            )])),
        }
    }
}
