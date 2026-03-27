use crate::{
    InstallPackageFromRegistry, ResolvedPackages, WorkspacePackages,
    collect_runtime_lockfile_config, fetch_package_from_registry_and_cache,
    fetch_package_with_metadata_cache, is_git_spec, is_tarball_spec, link_package,
    link_target_with_publish_config_directory, package_dependency_map,
    read_cached_package_from_config, require_workspace_dependency,
    resolve_package_version_from_git_spec, resolve_package_version_from_tarball_spec,
    resolve_workspace_dependency, resolve_workspace_dependency_by_plain_spec,
    resolve_workspace_dependency_by_relative_path,
};
use async_recursion::async_recursion;
use dashmap::DashMap;
use futures_util::stream::{self, StreamExt};
use miette::Context;
use pacquet_lockfile::{
    ComVer, DependencyPath, DirectoryResolution, Lockfile, LockfileResolution,
    MultiProjectSnapshot, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer,
    PkgVerPeer, ProjectSnapshot, RegistryResolution, ResolvedDependencyMap, ResolvedDependencySpec,
    ResolvedDependencyVersion, RootProjectSnapshot, TarballResolution,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::{PackageTag, PackageVersion};
use pacquet_tarball::MemCache;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

pub type PreferredVersions = HashMap<String, HashSet<String>>;

/// Install dependencies from package.json and update `pnpm-lock.yaml`.
#[must_use]
pub struct InstallWithLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub existing_lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub lockfile_importer_id: &'a str,
    pub workspace_packages: &'a WorkspacePackages,
    pub preferred_versions: Option<&'a PreferredVersions>,
    pub dependency_groups: DependencyGroupList,
    pub lockfile_only: bool,
    pub force: bool,
    pub prefer_offline: bool,
    pub offline: bool,
    pub pnpmfile: Option<&'a Path>,
    pub ignore_pnpmfile: bool,
}

#[derive(Clone)]
struct ResolvedPackage {
    version: ResolvedDependencyVersion,
    peer_suffixes: BTreeMap<String, ResolvedDependencyVersion>,
}

impl ResolvedPackage {
    fn new(version: ResolvedDependencyVersion) -> Self {
        Self { version, peer_suffixes: BTreeMap::new() }
    }
}

impl<'a, DependencyGroupList> InstallWithLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> miette::Result<Vec<String>> {
        let InstallWithLockfile {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            existing_lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            preferred_versions,
            dependency_groups,
            lockfile_only,
            force,
            prefer_offline,
            offline,
            pnpmfile,
            ignore_pnpmfile,
        } = self;

        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();
        let package_snapshots = DashMap::<DependencyPath, PackageSnapshot>::new();
        let mut resolved_direct_dependencies = HashMap::<(String, String), ResolvedPackage>::new();
        let workspace_root_peer_overrides = workspace_root_peer_overrides(manifest.path());
        let workspace_root_overrides = workspace_root_overrides(manifest.path());

        let hooked_manifest = crate::apply_read_package_hook_to_manifest(
            lockfile_dir,
            pnpmfile,
            ignore_pnpmfile,
            manifest.value(),
        )
        .expect("apply .pnpmfile.cjs readPackage hook to project manifest");

        let direct_dependencies = unique_direct_dependencies(&hooked_manifest, &dependency_groups);
        let direct_dependency_concurrency = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get().clamp(4, 32))
            .unwrap_or(16);

        let resolved_direct_dependency_entries = stream::iter(direct_dependencies)
            .map(|(group, name, version_range)| {
                let package_snapshots = &package_snapshots;
                let workspace_root_peer_overrides = &workspace_root_peer_overrides;
                let workspace_root_overrides = &workspace_root_overrides;
                async move {
                    let key = (name.clone(), version_range.clone());
                    let version_range = apply_workspace_root_override(
                        workspace_root_overrides,
                        &name,
                        &version_range,
                    );

                    let resolved_package =
                        if let Some((resolved_package, local_dep_path, should_symlink)) =
                            resolve_local_dependency(manifest.path(), &version_range)
                        {
                            if should_symlink {
                                if !lockfile_only {
                                    let dependency_path = config.modules_dir.join(&name);
                                    link_package(true, &local_dep_path, &dependency_path)
                                        .expect("install local dependency");
                                }
                                resolved_package
                            } else {
                                let normalized_ref =
                                    normalized_local_file_reference(lockfile_dir, &local_dep_path)
                                        .replace('\\', "/");
                                Self::snapshot_local_directory_package(
                                    resolved_packages,
                                    http_client,
                                    config,
                                    package_snapshots,
                                    workspace_root_overrides,
                                    workspace_packages,
                                    workspace_root_peer_overrides,
                                    manifest.path(),
                                    lockfile_dir,
                                    pnpmfile,
                                    ignore_pnpmfile,
                                    preferred_versions,
                                    &name,
                                    &local_dep_path,
                                    &normalized_ref,
                                    prefer_offline,
                                    offline,
                                )
                                .await
                                .wrap_err_with(|| format!("snapshot local dependency `{name}`"))?
                            }
                        } else if let Some(workspace_package) =
                            resolve_workspace_dependency_for_install(
                                config,
                                workspace_packages,
                                &name,
                                &version_range,
                                0,
                            )?
                        {
                            if should_inject_workspace_dependency(
                                manifest,
                                &name,
                                &version_range,
                                config,
                            ) {
                                let normalized_ref = normalized_local_file_reference(
                                    lockfile_dir,
                                    &workspace_package.root_dir,
                                )
                                .replace('\\', "/");
                                Self::snapshot_local_directory_package(
                                    resolved_packages,
                                    http_client,
                                    config,
                                    package_snapshots,
                                    workspace_root_overrides,
                                    workspace_packages,
                                    workspace_root_peer_overrides,
                                    manifest.path(),
                                    lockfile_dir,
                                    pnpmfile,
                                    ignore_pnpmfile,
                                    preferred_versions,
                                    &name,
                                    &workspace_package.root_dir,
                                    &normalized_ref,
                                    prefer_offline,
                                    offline,
                                )
                                .await
                                .wrap_err_with(|| {
                                    format!("snapshot injected workspace dependency `{name}`")
                                })?
                            } else {
                                if !lockfile_only {
                                    let symlink_path = config.modules_dir.join(&name);
                                    let symlink_target = link_target_with_publish_config_directory(
                                        &workspace_package.root_dir,
                                    );
                                    link_package(config.symlink, &symlink_target, &symlink_path)
                                        .map_err(|error| {
                                            miette::miette!(
                                                "symlink workspace package `{name}`: {error}"
                                            )
                                        })?;
                                }

                                let project_dir =
                                    manifest.path().parent().unwrap_or_else(|| Path::new("."));
                                let relative =
                                    to_relative_path(project_dir, &workspace_package.root_dir);
                                ResolvedPackage::new(ResolvedDependencyVersion::Link(format!(
                                    "link:{}",
                                    relative.replace('\\', "/")
                                )))
                            }
                        } else if lockfile_only {
                            Self::resolve_and_snapshot_package(
                                resolved_packages,
                                http_client,
                                config,
                                lockfile_dir,
                                pnpmfile,
                                ignore_pnpmfile,
                                package_snapshots,
                                workspace_packages,
                                workspace_root_overrides,
                                workspace_root_peer_overrides,
                                preferred_versions,
                                &name,
                                &version_range,
                                matches!(group, DependencyGroup::Optional),
                                prefer_offline,
                                offline,
                            )
                            .await
                            .wrap_err_with(|| {
                                format!("resolve dependency `{name}` for lockfile snapshot")
                            })?
                        } else {
                            Self::install_and_snapshot_package(
                                tarball_mem_cache,
                                resolved_packages,
                                http_client,
                                config,
                                lockfile_dir,
                                pnpmfile,
                                ignore_pnpmfile,
                                package_snapshots,
                                workspace_packages,
                                workspace_root_overrides,
                                workspace_root_peer_overrides,
                                &config.modules_dir,
                                &name,
                                &version_range,
                                matches!(group, DependencyGroup::Optional),
                                offline,
                                prefer_offline,
                                force,
                            )
                            .await
                            .wrap_err_with(|| {
                                format!("install dependency `{name}` and update lockfile")
                            })?
                        };

                    Ok::<_, miette::Report>((key, resolved_package))
                }
            })
            .buffer_unordered(direct_dependency_concurrency)
            .collect::<Vec<miette::Result<_>>>()
            .await;

        for entry in resolved_direct_dependency_entries {
            let (key, resolved_package) = entry?;
            resolved_direct_dependencies.insert(key, resolved_package);
        }

        if config.dedupe_injected_deps {
            dedupe_injected_direct_dependencies(
                config,
                manifest,
                existing_lockfile,
                lockfile_dir,
                workspace_packages,
                &package_snapshots,
                &mut resolved_direct_dependencies,
            );
        }

        let project_snapshot = Self::build_project_snapshot(
            manifest,
            dependency_groups.iter().copied(),
            &resolved_direct_dependencies,
            config.exclude_links_from_lockfile,
        );

        let mut packages = existing_lockfile
            .and_then(|lockfile| lockfile.packages.as_ref())
            .cloned()
            .unwrap_or_default();
        packages.extend(package_snapshots.into_iter());
        let project_snapshot =
            dedupe_project_snapshot(&project_snapshot, &packages, config.dedupe_peer_dependents);

        let project_snapshot =
            merge_project_snapshot(existing_lockfile, lockfile_importer_id, project_snapshot);
        let project_snapshot =
            ensure_workspace_importers(project_snapshot, lockfile_dir, workspace_packages);
        prune_unreferenced_packages(&project_snapshot, &mut packages);
        let runtime_lockfile_config = collect_runtime_lockfile_config(
            config,
            manifest,
            lockfile_dir,
            pnpmfile,
            ignore_pnpmfile,
        );

        let lockfile = Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: Some(runtime_lockfile_config.settings),
            never_built_dependencies: None,
            ignored_optional_dependencies: existing_lockfile
                .and_then(|lockfile| lockfile.ignored_optional_dependencies.clone()),
            overrides: runtime_lockfile_config.overrides,
            package_extensions_checksum: runtime_lockfile_config.package_extensions_checksum,
            patched_dependencies: crate::manifest_patched_dependencies_for_lockfile(lockfile_dir)?,
            pnpmfile_checksum: runtime_lockfile_config.pnpmfile_checksum,
            catalogs: existing_lockfile.and_then(|lockfile| lockfile.catalogs.clone()),
            time: existing_lockfile.and_then(|lockfile| lockfile.time.clone()),
            extra_fields: existing_lockfile
                .map(|lockfile| lockfile.extra_fields.clone())
                .unwrap_or_default(),
            project_snapshot,
            packages: (!packages.is_empty()).then_some(packages),
        };
        let lockfile = crate::apply_after_all_resolved_hook(
            lockfile_dir,
            pnpmfile,
            ignore_pnpmfile,
            &lockfile,
        )
        .expect("apply .pnpmfile.cjs afterAllResolved hook to lockfile");

        let project_snapshot_for_importer = match &lockfile.project_snapshot {
            RootProjectSnapshot::Single(snapshot) => snapshot.clone(),
            RootProjectSnapshot::Multi(snapshot) => {
                snapshot.importers.get(lockfile_importer_id).cloned().unwrap_or_default()
            }
        };

        lockfile.save_to_dir(lockfile_dir).expect("save lockfile");

        if !lockfile_only {
            let relinked_packages = ResolvedPackages::new();
            let skipped = crate::InstallFrozenLockfile {
                http_client,
                resolved_packages: &relinked_packages,
                config,
                project_snapshot: &project_snapshot_for_importer,
                packages: lockfile.packages.as_ref(),
                lockfile_dir,
                dependency_groups,
                offline,
                force,
                pnpmfile,
                ignore_pnpmfile,
            }
            .run()
            .await;
            return Ok(skipped);
        }

        Ok(Vec::new())
    }

    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn resolve_and_snapshot_package(
        resolved_packages: &ResolvedPackages,
        http_client: &ThrottledClient,
        config: &'static Npmrc,
        lockfile_dir: &Path,
        pnpmfile: Option<&Path>,
        ignore_pnpmfile: bool,
        package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
        workspace_packages: &WorkspacePackages,
        workspace_root_overrides: &HashMap<String, String>,
        workspace_root_peer_overrides: &HashMap<String, String>,
        preferred_versions: Option<&PreferredVersions>,
        name: &str,
        version_range: &str,
        optional: bool,
        prefer_offline: bool,
        offline: bool,
    ) -> miette::Result<ResolvedPackage> {
        let package_version = resolve_package_version(
            ResolvePackageVersionContext {
                config,
                http_client,
                lockfile_dir,
                pnpmfile,
                ignore_pnpmfile,
                prefer_offline,
                offline,
                preferred_versions,
            },
            name,
            version_range,
        )
        .await?;

        let ver_peer = Self::to_pkg_ver_peer(&package_version);
        let resolved_version = if package_version.name == name {
            ResolvedDependencyVersion::PkgVerPeer(ver_peer.clone())
        } else {
            ResolvedDependencyVersion::PkgNameVerPeer(PkgNameVerPeer::new(
                Self::parse_pkg_name(&package_version.name),
                ver_peer.clone(),
            ))
        };
        let dependency_path = Self::to_dependency_path(&package_version);
        let virtual_store_name = package_version.to_virtual_store_name();
        let skipped_optional = optional
            && crate::installability::should_skip_optional_package_version(&package_version);

        if resolved_packages.insert(virtual_store_name.clone()) {
            let mut snapshot_dependencies = HashMap::new();
            let mut snapshot_optional_dependencies = HashMap::new();
            let dependency_entries = if skipped_optional {
                Vec::new()
            } else {
                package_dependency_entries(&package_version, config.auto_install_peers)
            };
            let dependency_concurrency = std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().clamp(4, 32))
                .unwrap_or(16);
            let peer_dependencies = package_version.peer_dependencies.clone();

            let resolved_dependencies = stream::iter(dependency_entries)
                .map(|(dependency_optional, dependency_name, dependency_version_range)| {
                    let peer_dependencies = peer_dependencies.clone();
                    async move {
                        let dependency_version_range = apply_workspace_root_peer_override(
                            config,
                            workspace_root_peer_overrides,
                            peer_dependencies.as_ref(),
                            &dependency_name,
                            &dependency_version_range,
                        );
                        let dependency_version_range = apply_workspace_root_override(
                            workspace_root_overrides,
                            &dependency_name,
                            &dependency_version_range,
                        );
                        if let Some(workspace_package) = resolve_workspace_dependency_for_install(
                            config,
                            workspace_packages,
                            &dependency_name,
                            &dependency_version_range,
                            1,
                        )? {
                            let relative =
                                to_relative_path(lockfile_dir, &workspace_package.root_dir);
                            return Ok::<_, miette::Report>((
                                dependency_optional,
                                dependency_name,
                                ResolvedPackage::new(ResolvedDependencyVersion::Link(format!(
                                    "link:{}",
                                    relative.replace('\\', "/")
                                ))),
                            ));
                        }
                        let resolved_dependency = Self::resolve_and_snapshot_package(
                            resolved_packages,
                            http_client,
                            config,
                            lockfile_dir,
                            pnpmfile,
                            ignore_pnpmfile,
                            package_snapshots,
                            workspace_packages,
                            workspace_root_overrides,
                            workspace_root_peer_overrides,
                            preferred_versions,
                            &dependency_name,
                            &dependency_version_range,
                            dependency_optional,
                            prefer_offline,
                            offline,
                        )
                        .await?;
                        Ok::<_, miette::Report>((
                            dependency_optional,
                            dependency_name,
                            resolved_dependency,
                        ))
                    }
                })
                .buffer_unordered(dependency_concurrency)
                .collect::<Vec<miette::Result<_>>>()
                .await;

            for resolved_dependency in resolved_dependencies {
                let (dependency_optional, dependency_name, resolved_dependency) =
                    resolved_dependency?;
                insert_snapshot_dependency(
                    &mut snapshot_dependencies,
                    &mut snapshot_optional_dependencies,
                    &dependency_name,
                    resolved_dependency.version,
                    dependency_optional,
                );
            }

            let package_snapshot = Self::to_package_snapshot(
                config,
                &package_version,
                snapshot_dependencies,
                snapshot_optional_dependencies,
                optional,
            );
            package_snapshots.insert(dependency_path, package_snapshot);
        } else {
            downgrade_existing_snapshot_optional_flag(
                package_snapshots,
                &dependency_path,
                optional,
            );
        }

        Ok(ResolvedPackage::new(resolved_version))
    }

    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn install_and_snapshot_package(
        tarball_mem_cache: &MemCache,
        resolved_packages: &ResolvedPackages,
        http_client: &ThrottledClient,
        config: &'static Npmrc,
        lockfile_dir: &Path,
        pnpmfile: Option<&Path>,
        ignore_pnpmfile: bool,
        package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
        workspace_packages: &WorkspacePackages,
        workspace_root_overrides: &HashMap<String, String>,
        workspace_root_peer_overrides: &HashMap<String, String>,
        node_modules_dir: &Path,
        name: &str,
        version_range: &str,
        optional: bool,
        offline: bool,
        prefer_offline: bool,
        force: bool,
    ) -> miette::Result<ResolvedPackage> {
        let package_version = InstallPackageFromRegistry {
            tarball_mem_cache,
            http_client,
            config,
            lockfile_dir,
            pnpmfile,
            ignore_pnpmfile,
            node_modules_dir,
            name,
            version_range,
            optional,
            prefer_offline,
            offline,
            force,
        }
        .run()
        .await
        .map_err(|error| miette::miette!("install package from registry: {error}"))?;

        let ver_peer = Self::to_pkg_ver_peer(&package_version);
        let resolved_version = if package_version.name == name {
            ResolvedDependencyVersion::PkgVerPeer(ver_peer.clone())
        } else {
            ResolvedDependencyVersion::PkgNameVerPeer(PkgNameVerPeer::new(
                Self::parse_pkg_name(&package_version.name),
                ver_peer.clone(),
            ))
        };
        let dependency_path = Self::to_dependency_path(&package_version);
        let virtual_store_name = package_version.to_virtual_store_name();
        let skipped_optional = optional
            && crate::installability::should_skip_optional_package_version(&package_version);

        if resolved_packages.insert(virtual_store_name.clone()) {
            let virtual_node_modules_dir =
                config.virtual_store_dir.join(virtual_store_name).join("node_modules");

            let mut snapshot_dependencies = HashMap::new();
            let mut snapshot_optional_dependencies = HashMap::new();
            let dependency_entries = if skipped_optional {
                Vec::new()
            } else {
                package_dependency_entries(&package_version, config.auto_install_peers)
            };
            let dependency_concurrency = std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().clamp(4, 32))
                .unwrap_or(16);
            let peer_dependencies = package_version.peer_dependencies.clone();

            let resolved_dependencies = stream::iter(dependency_entries)
                .map(|(dependency_optional, dependency_name, dependency_version_range)| {
                    let peer_dependencies = peer_dependencies.clone();
                    let virtual_node_modules_dir = virtual_node_modules_dir.clone();
                    async move {
                        let dependency_version_range = apply_workspace_root_peer_override(
                            config,
                            workspace_root_peer_overrides,
                            peer_dependencies.as_ref(),
                            &dependency_name,
                            &dependency_version_range,
                        );
                        let dependency_version_range = apply_workspace_root_override(
                            workspace_root_overrides,
                            &dependency_name,
                            &dependency_version_range,
                        );
                        if let Some(workspace_package) = resolve_workspace_dependency_for_install(
                            config,
                            workspace_packages,
                            &dependency_name,
                            &dependency_version_range,
                            1,
                        )? {
                            let relative =
                                to_relative_path(lockfile_dir, &workspace_package.root_dir);
                            return Ok::<_, miette::Report>((
                                dependency_optional,
                                dependency_name,
                                ResolvedPackage::new(ResolvedDependencyVersion::Link(format!(
                                    "link:{}",
                                    relative.replace('\\', "/")
                                ))),
                            ));
                        }
                        let resolved_dependency = Self::install_and_snapshot_package(
                            tarball_mem_cache,
                            resolved_packages,
                            http_client,
                            config,
                            lockfile_dir,
                            pnpmfile,
                            ignore_pnpmfile,
                            package_snapshots,
                            workspace_packages,
                            workspace_root_overrides,
                            workspace_root_peer_overrides,
                            &virtual_node_modules_dir,
                            &dependency_name,
                            &dependency_version_range,
                            dependency_optional,
                            offline,
                            prefer_offline,
                            force,
                        )
                        .await?;
                        Ok::<_, miette::Report>((
                            dependency_optional,
                            dependency_name,
                            resolved_dependency,
                        ))
                    }
                })
                .buffer_unordered(dependency_concurrency)
                .collect::<Vec<miette::Result<_>>>()
                .await;

            for resolved_dependency in resolved_dependencies {
                let (dependency_optional, dependency_name, resolved_dependency) =
                    resolved_dependency?;
                insert_snapshot_dependency(
                    &mut snapshot_dependencies,
                    &mut snapshot_optional_dependencies,
                    &dependency_name,
                    resolved_dependency.version,
                    dependency_optional,
                );
            }

            let package_snapshot = Self::to_package_snapshot(
                config,
                &package_version,
                snapshot_dependencies,
                snapshot_optional_dependencies,
                optional,
            );
            package_snapshots.insert(dependency_path, package_snapshot);
        } else {
            downgrade_existing_snapshot_optional_flag(
                package_snapshots,
                &dependency_path,
                optional,
            );
        }

        Ok(ResolvedPackage::new(resolved_version))
    }

    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn snapshot_local_directory_package(
        resolved_packages: &ResolvedPackages,
        http_client: &ThrottledClient,
        config: &'static Npmrc,
        package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
        workspace_root_overrides: &HashMap<String, String>,
        workspace_packages: &WorkspacePackages,
        workspace_root_peer_overrides: &HashMap<String, String>,
        current_manifest_path: &Path,
        lockfile_dir: &Path,
        pnpmfile: Option<&Path>,
        ignore_pnpmfile: bool,
        preferred_versions: Option<&PreferredVersions>,
        dependency_name: &str,
        local_dep_path: &Path,
        normalized_ref: &str,
        prefer_offline: bool,
        offline: bool,
    ) -> miette::Result<ResolvedPackage> {
        let local_manifest = PackageManifest::from_path(local_dep_path.join("package.json")).ok();
        let local_manifest_value = local_manifest.as_ref().and_then(|manifest| {
            crate::apply_read_package_hook_to_manifest(
                lockfile_dir,
                pnpmfile,
                ignore_pnpmfile,
                manifest.value(),
            )
            .ok()
        });
        let package_name = local_manifest_value
            .as_ref()
            .and_then(|manifest| manifest.get("name"))
            .and_then(|value| value.as_str())
            .unwrap_or(dependency_name);
        let local_dependency_entries = local_manifest_value
            .as_ref()
            .map(|manifest| {
                crate::dependencies_from_manifest_value(
                    manifest,
                    [DependencyGroup::Prod, DependencyGroup::Optional],
                )
            })
            .unwrap_or_default();
        let peer_dependencies = local_manifest_value
            .as_ref()
            .and_then(|manifest| json_string_map(manifest.get("peerDependencies")));
        let peer_dependency_entries = peer_dependencies
            .as_ref()
            .map(|dependencies| {
                dependencies
                    .iter()
                    .map(|(name, range)| (name.clone(), range.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut resolved_local_peers = BTreeMap::<String, ResolvedDependencyVersion>::new();
        let mut snapshot_dependencies = HashMap::new();
        let dependency_concurrency = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get().clamp(4, 32))
            .unwrap_or(16);

        if let Some(peer_dependencies) = peer_dependencies.as_ref() {
            let resolved_peers = stream::iter(peer_dependency_entries)
                .map(|(peer_name, peer_range)| async move {
                    let resolved_range = apply_workspace_root_peer_override(
                        config,
                        workspace_root_peer_overrides,
                        Some(peer_dependencies),
                        &peer_name,
                        &peer_range,
                    );
                    let resolved_range = apply_workspace_root_override(
                        workspace_root_overrides,
                        &peer_name,
                        &resolved_range,
                    );
                    let resolved_peer = Self::resolve_local_peer_dependency(
                        resolved_packages,
                        http_client,
                        config,
                        package_snapshots,
                        workspace_root_overrides,
                        workspace_packages,
                        workspace_root_peer_overrides,
                        current_manifest_path,
                        lockfile_dir,
                        pnpmfile,
                        ignore_pnpmfile,
                        preferred_versions,
                        &peer_name,
                        &resolved_range,
                        prefer_offline,
                        offline,
                    )
                    .await?;

                    Ok::<_, miette::Report>(
                        resolved_peer.map(|resolved_peer| (peer_name, resolved_peer)),
                    )
                })
                .buffer_unordered(dependency_concurrency)
                .collect::<Vec<miette::Result<Option<(String, ResolvedPackage)>>>>()
                .await;

            for resolved_peer in resolved_peers {
                let Some((peer_name, resolved_peer)) = resolved_peer? else {
                    continue;
                };

                resolved_local_peers.insert(peer_name.clone(), resolved_peer.version.clone());
                merge_resolved_peer_suffixes(
                    &mut resolved_local_peers,
                    &resolved_peer.peer_suffixes,
                );
                snapshot_dependencies.insert(
                    Self::parse_pkg_name(&peer_name),
                    resolved_version_to_snapshot_dependency(&peer_name, &resolved_peer.version),
                );
            }
        }

        if let Some(local_manifest) = &local_manifest {
            let local_manifest_path = local_manifest.path().to_path_buf();
            let resolved_children = stream::iter(local_dependency_entries.iter().cloned())
                .map(|(child_name, child_version_range)| {
                    let local_manifest_path = local_manifest_path.clone();
                    async move {
                        if let Some((resolved_local, local_child_path, should_symlink)) =
                            resolve_local_dependency(&local_manifest_path, &child_version_range)
                        {
                            if should_symlink {
                                return Ok::<_, miette::Report>((child_name, resolved_local, true));
                            }

                            let normalized_child_ref =
                                normalized_local_file_reference(lockfile_dir, &local_child_path)
                                    .replace('\\', "/");
                            let resolved_local = Self::snapshot_local_directory_package(
                                resolved_packages,
                                http_client,
                                config,
                                package_snapshots,
                                workspace_root_overrides,
                                workspace_packages,
                                workspace_root_peer_overrides,
                                current_manifest_path,
                                lockfile_dir,
                                pnpmfile,
                                ignore_pnpmfile,
                                preferred_versions,
                                &child_name,
                                &local_child_path,
                                &normalized_child_ref,
                                prefer_offline,
                                offline,
                            )
                            .await?;
                            return Ok((child_name, resolved_local, true));
                        }

                        if let Some(workspace_package) = resolve_workspace_dependency_for_install(
                            config,
                            workspace_packages,
                            &child_name,
                            &child_version_range,
                            1,
                        )? {
                            let normalized_child_ref = normalized_local_file_reference(
                                lockfile_dir,
                                &workspace_package.root_dir,
                            )
                            .replace('\\', "/");
                            let resolved_local = Self::snapshot_local_directory_package(
                                resolved_packages,
                                http_client,
                                config,
                                package_snapshots,
                                workspace_root_overrides,
                                workspace_packages,
                                workspace_root_peer_overrides,
                                current_manifest_path,
                                lockfile_dir,
                                pnpmfile,
                                ignore_pnpmfile,
                                preferred_versions,
                                &child_name,
                                &workspace_package.root_dir,
                                &normalized_child_ref,
                                prefer_offline,
                                offline,
                            )
                            .await?;
                            return Ok((child_name, resolved_local, true));
                        }

                        let resolved_range = apply_workspace_root_peer_override(
                            config,
                            workspace_root_peer_overrides,
                            None,
                            &child_name,
                            &child_version_range,
                        );
                        let resolved_range = apply_workspace_root_override(
                            workspace_root_overrides,
                            &child_name,
                            &resolved_range,
                        );
                        let resolved_dependency = Self::resolve_and_snapshot_package(
                            resolved_packages,
                            http_client,
                            config,
                            lockfile_dir,
                            pnpmfile,
                            ignore_pnpmfile,
                            package_snapshots,
                            workspace_packages,
                            workspace_root_overrides,
                            workspace_root_peer_overrides,
                            preferred_versions,
                            &child_name,
                            &resolved_range,
                            false,
                            prefer_offline,
                            offline,
                        )
                        .await?;
                        Ok((child_name, resolved_dependency, false))
                    }
                })
                .buffer_unordered(dependency_concurrency)
                .collect::<Vec<miette::Result<(String, ResolvedPackage, bool)>>>()
                .await;

            for resolved_child in resolved_children {
                let (child_name, resolved_dependency, is_link) = resolved_child?;
                if is_link {
                    merge_resolved_peer_suffixes(
                        &mut resolved_local_peers,
                        &resolved_dependency.peer_suffixes,
                    );
                    snapshot_dependencies.insert(
                        Self::parse_pkg_name(&child_name),
                        PackageSnapshotDependency::Link(resolved_dependency.version.to_string()),
                    );
                } else {
                    snapshot_dependencies.insert(
                        Self::parse_pkg_name(&child_name),
                        resolved_version_to_snapshot_dependency(
                            &child_name,
                            &resolved_dependency.version,
                        ),
                    );
                }
            }
        }

        let normalized_ref_with_peers =
            append_peer_suffix_to_local_reference(normalized_ref, &resolved_local_peers);
        let dependency_path = DependencyPath::local_file(
            Self::parse_pkg_name(package_name),
            normalized_ref_with_peers.clone(),
        );
        package_snapshots.insert(
            dependency_path,
            PackageSnapshot {
                resolution: LockfileResolution::Directory(DirectoryResolution {
                    directory: normalized_ref
                        .strip_prefix("file:")
                        .unwrap_or(normalized_ref)
                        .to_string(),
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
                peer_dependencies,
                peer_dependencies_meta: None,
                dependencies: (!snapshot_dependencies.is_empty()).then_some(snapshot_dependencies),
                optional_dependencies: None,
                transitive_peer_dependencies: None,
                dev: None,
                optional: None,
            },
        );

        Ok(ResolvedPackage {
            version: ResolvedDependencyVersion::Link(normalized_ref_with_peers.to_string()),
            peer_suffixes: resolved_local_peers,
        })
    }

    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn resolve_local_peer_dependency(
        resolved_packages: &ResolvedPackages,
        http_client: &ThrottledClient,
        config: &'static Npmrc,
        package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
        workspace_root_overrides: &HashMap<String, String>,
        workspace_packages: &WorkspacePackages,
        workspace_root_peer_overrides: &HashMap<String, String>,
        current_manifest_path: &Path,
        lockfile_dir: &Path,
        pnpmfile: Option<&Path>,
        ignore_pnpmfile: bool,
        preferred_versions: Option<&PreferredVersions>,
        name: &str,
        version_range: &str,
        prefer_offline: bool,
        offline: bool,
    ) -> miette::Result<Option<ResolvedPackage>> {
        let available_specs = read_dependency_specs(current_manifest_path);
        let requested_range =
            available_specs.get(name).map(String::as_str).unwrap_or(version_range);
        let requested_range =
            apply_workspace_root_override(workspace_root_overrides, name, requested_range);

        if available_specs.contains_key(name)
            && let Some(workspace_package) = resolve_workspace_dependency_for_install(
                config,
                workspace_packages,
                name,
                &requested_range,
                1,
            )?
        {
            let relative = to_relative_path(lockfile_dir, &workspace_package.root_dir);
            return Ok(Some(ResolvedPackage::new(ResolvedDependencyVersion::Link(format!(
                "link:{}",
                relative.replace('\\', "/")
            )))));
        }

        if let Some((_, local_dep_path, should_symlink)) =
            resolve_local_dependency(current_manifest_path, &requested_range)
        {
            let protocol = if should_symlink { "link:" } else { "file:" };
            let normalized_ref = format!(
                "{protocol}{}",
                to_relative_path(
                    current_manifest_path.parent().unwrap_or_else(|| Path::new(".")),
                    &local_dep_path
                )
                .replace('\\', "/")
            );
            if should_symlink {
                return Ok(Some(ResolvedPackage::new(ResolvedDependencyVersion::Link(
                    normalized_ref,
                ))));
            }

            return Ok(Some(
                Self::snapshot_local_directory_package(
                    resolved_packages,
                    http_client,
                    config,
                    package_snapshots,
                    workspace_root_overrides,
                    workspace_packages,
                    workspace_root_peer_overrides,
                    current_manifest_path,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                    preferred_versions,
                    name,
                    &local_dep_path,
                    &normalized_ref,
                    prefer_offline,
                    offline,
                )
                .await?,
            ));
        }

        if let Some(workspace_package) = resolve_workspace_dependency_for_install(
            config,
            workspace_packages,
            name,
            &requested_range,
            1,
        )? {
            let relative = to_relative_path(lockfile_dir, &workspace_package.root_dir);
            return Ok(Some(ResolvedPackage::new(ResolvedDependencyVersion::Link(format!(
                "link:{}",
                relative.replace('\\', "/")
            )))));
        }

        if available_specs.contains_key(name) || config.auto_install_peers {
            return Ok(Some(
                Self::resolve_and_snapshot_package(
                    resolved_packages,
                    http_client,
                    config,
                    lockfile_dir,
                    pnpmfile,
                    ignore_pnpmfile,
                    package_snapshots,
                    workspace_packages,
                    workspace_root_overrides,
                    workspace_root_peer_overrides,
                    preferred_versions,
                    name,
                    &requested_range,
                    false,
                    prefer_offline,
                    offline,
                )
                .await?,
            ));
        }

        Ok(None)
    }

    fn build_project_snapshot(
        manifest: &PackageManifest,
        dependency_groups: impl IntoIterator<Item = DependencyGroup>,
        resolved_direct_dependencies: &HashMap<(String, String), ResolvedPackage>,
        exclude_links_from_lockfile: bool,
    ) -> ProjectSnapshot {
        let dependency_groups = dependency_groups.into_iter().collect::<Vec<_>>();

        let mut specifiers = HashMap::new();
        let mut dependencies = None;
        let mut optional_dependencies = None;
        let mut dev_dependencies = None;

        for group in dependency_groups {
            let mut map = ResolvedDependencyMap::new();
            for (name, specifier) in manifest.dependencies([group]) {
                let key = (name.to_string(), specifier.to_string());
                if let Some(resolved_dependency) = resolved_direct_dependencies.get(&key) {
                    if !should_include_dependency_in_lockfile(
                        specifier,
                        &resolved_dependency.version,
                        exclude_links_from_lockfile,
                    ) {
                        continue;
                    }
                    map.insert(
                        Self::parse_pkg_name(name),
                        ResolvedDependencySpec {
                            specifier: specifier.to_string(),
                            version: resolved_dependency.version.clone(),
                        },
                    );
                    specifiers.insert(name.to_string(), specifier.to_string());
                }
            }

            if map.is_empty() {
                continue;
            }

            match group {
                DependencyGroup::Prod => dependencies = Some(map),
                DependencyGroup::Optional => optional_dependencies = Some(map),
                DependencyGroup::Dev => dev_dependencies = Some(map),
                DependencyGroup::Peer => {}
            }
        }

        ProjectSnapshot {
            specifiers: (!specifiers.is_empty()).then_some(specifiers),
            dependencies,
            optional_dependencies,
            dev_dependencies,
            dependencies_meta: project_dependencies_meta(manifest),
            publish_directory: manifest
                .value()
                .get("publishConfig")
                .and_then(|publish_config| publish_config.get("directory"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
        }
    }

    fn to_dependency_path(package_version: &PackageVersion) -> DependencyPath {
        let package_specifier = PkgNameVerPeer::new(
            Self::parse_pkg_name(package_version.name.as_str()),
            Self::to_pkg_ver_peer(package_version),
        );
        DependencyPath::registry(None, package_specifier)
    }

    fn to_pkg_ver_peer(package_version: &PackageVersion) -> PkgVerPeer {
        package_version.version.to_string().parse().expect("package version is always valid semver")
    }

    fn parse_pkg_name(package_name: &str) -> PkgName {
        package_name.parse().expect("package name from npm registry is valid")
    }

    fn to_package_snapshot(
        config: &'static Npmrc,
        package_version: &PackageVersion,
        dependencies: HashMap<PkgName, PackageSnapshotDependency>,
        optional_dependencies: HashMap<String, String>,
        optional: bool,
    ) -> PackageSnapshot {
        let integrity =
            package_version.dist.integrity.clone().expect("registry package has integrity field");
        let package_id = format!("{}@{}", package_version.name, package_version.version);
        let requires_build = config
            .store_dir
            .read_index_file(&integrity, &package_id)
            .and_then(|index| index.requires_build)
            .unwrap_or(false);

        let expected_registry_tarball = expected_registry_tarball(config, package_version);
        let should_use_tarball_resolution = config.lockfile_include_tarball_url
            || package_version.as_tarball_url() != expected_registry_tarball;

        let resolution = if should_use_tarball_resolution {
            LockfileResolution::Tarball(TarballResolution {
                tarball: package_version.as_tarball_url().to_string(),
                integrity: Some(integrity),
            })
        } else {
            LockfileResolution::Registry(RegistryResolution { integrity })
        };

        PackageSnapshot {
            resolution,
            id: None,
            name: None,
            version: None,
            engines: package_version.engines.clone(),
            cpu: package_version.cpu.clone(),
            os: package_version.os.clone(),
            libc: package_version.libc.clone(),
            deprecated: package_version.deprecated.clone(),
            has_bin: package_version.has_bin().then_some(true),
            prepare: None,
            requires_build: requires_build.then_some(true),
            bundled_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            dependencies: (!dependencies.is_empty()).then_some(dependencies),
            optional_dependencies: (!optional_dependencies.is_empty())
                .then_some(optional_dependencies),
            transitive_peer_dependencies: None,
            dev: None,
            optional: optional.then_some(true),
        }
    }
}

fn unique_direct_dependencies(
    manifest_value: &serde_json::Value,
    dependency_groups: &[DependencyGroup],
) -> Vec<(DependencyGroup, String, String)> {
    let mut seen = HashSet::<(String, String)>::new();
    let mut dependencies = Vec::new();
    for (group, name, version_range) in crate::dependencies_from_manifest_value_grouped(
        manifest_value,
        dependency_groups.iter().copied(),
    ) {
        let key = (name.to_string(), version_range.to_string());
        if seen.insert(key.clone()) {
            dependencies.push((group, key.0, key.1));
        }
    }
    dependencies
}

fn package_dependency_entries(
    package_version: &PackageVersion,
    auto_install_peers: bool,
) -> Vec<(bool, String, String)> {
    let mut dependencies = package_version
        .regular_dependencies()
        .map(|(name, version_range)| (false, name.to_string(), version_range.to_string()))
        .collect::<Vec<_>>();
    dependencies.extend(
        package_version
            .optional_dependencies_iter()
            .map(|(name, version_range)| (true, name.to_string(), version_range.to_string())),
    );
    if auto_install_peers {
        dependencies.extend(
            package_version
                .peer_dependencies_iter()
                .map(|(name, version_range)| (false, name.to_string(), version_range.to_string())),
        );
    }
    dependencies
}

fn insert_snapshot_dependency(
    dependencies: &mut HashMap<PkgName, PackageSnapshotDependency>,
    optional_dependencies: &mut HashMap<String, String>,
    dependency_name: &str,
    resolved_version: ResolvedDependencyVersion,
    optional: bool,
) {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            if optional {
                optional_dependencies.insert(dependency_name.to_string(), ver_peer.to_string());
            } else {
                dependencies.insert(
                    dependency_name.parse().expect("registry package name"),
                    PackageSnapshotDependency::PkgVerPeer(ver_peer),
                );
            }
        }
        ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
            if optional {
                optional_dependencies
                    .insert(dependency_name.to_string(), name_ver_peer.to_string());
            } else {
                dependencies.insert(
                    dependency_name.parse().expect("registry package name"),
                    PackageSnapshotDependency::PkgNameVerPeer(name_ver_peer),
                );
            }
        }
        ResolvedDependencyVersion::Link(link) => {
            if optional {
                optional_dependencies.insert(dependency_name.to_string(), link);
            } else {
                dependencies.insert(
                    dependency_name.parse().expect("registry package name"),
                    PackageSnapshotDependency::Link(link),
                );
            }
        }
    }
}

fn expected_registry_tarball(config: &Npmrc, package_version: &PackageVersion) -> String {
    let registry = config.registry_for_package_name(package_version.name.as_str());
    let registry = registry.trim_end_matches('/');
    let bare_name =
        package_version.name.rsplit('/').next().unwrap_or(package_version.name.as_str());
    format!("{registry}/{}/-/{bare_name}-{}.tgz", package_version.name, package_version.version)
}

fn downgrade_existing_snapshot_optional_flag(
    package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
    dependency_path: &DependencyPath,
    optional: bool,
) {
    if optional {
        return;
    }
    if let Some(mut package_snapshot) = package_snapshots.get_mut(dependency_path) {
        package_snapshot.optional = None;
    }
}

fn should_include_dependency_in_lockfile(
    specifier: &str,
    resolved_version: &ResolvedDependencyVersion,
    exclude_links_from_lockfile: bool,
) -> bool {
    if !exclude_links_from_lockfile {
        return true;
    }
    if specifier.starts_with("workspace:") {
        return true;
    }
    !matches!(
        resolved_version,
        ResolvedDependencyVersion::Link(version) if version.starts_with("link:")
    )
}

fn dedupe_injected_direct_dependencies(
    config: &Npmrc,
    manifest: &PackageManifest,
    existing_lockfile: Option<&Lockfile>,
    lockfile_dir: &Path,
    workspace_packages: &WorkspacePackages,
    package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
    resolved_direct_dependencies: &mut HashMap<(String, String), ResolvedPackage>,
) {
    let ctx = DedupeInjectedContext {
        config,
        manifest,
        existing_lockfile,
        lockfile_dir,
        workspace_packages,
        package_snapshots,
    };
    dedupe_injected_dependency_map(&ctx, resolved_direct_dependencies, DependencyGroup::Prod);
    dedupe_injected_dependency_map(&ctx, resolved_direct_dependencies, DependencyGroup::Optional);
    dedupe_injected_dependency_map(&ctx, resolved_direct_dependencies, DependencyGroup::Dev);
}

struct DedupeInjectedContext<'a> {
    config: &'a Npmrc,
    manifest: &'a PackageManifest,
    existing_lockfile: Option<&'a Lockfile>,
    lockfile_dir: &'a Path,
    workspace_packages: &'a WorkspacePackages,
    package_snapshots: &'a DashMap<DependencyPath, PackageSnapshot>,
}

fn dedupe_injected_dependency_map(
    ctx: &DedupeInjectedContext<'_>,
    resolved_direct_dependencies: &mut HashMap<(String, String), ResolvedPackage>,
    group: DependencyGroup,
) {
    let project_dir = ctx.manifest.path().parent().unwrap_or_else(|| Path::new("."));

    for (dependency_name, specifier) in ctx.manifest.dependencies([group]) {
        if !ctx.config.dedupe_injected_deps
            || !should_inject_workspace_dependency(
                ctx.manifest,
                dependency_name,
                specifier,
                ctx.config,
            )
        {
            continue;
        }

        let Some(workspace_package) = try_resolve_workspace_dependency(
            ctx.config,
            ctx.workspace_packages,
            dependency_name,
            specifier,
            0,
        ) else {
            continue;
        };

        let key = (dependency_name.to_string(), specifier.to_string());
        let Some(resolved_package) = resolved_direct_dependencies.get_mut(&key) else {
            continue;
        };
        let ResolvedDependencyVersion::Link(link) = &resolved_package.version else {
            continue;
        };
        if !link.starts_with("file:") {
            continue;
        }

        let Some(candidate_snapshot) = resolve_local_directory_snapshot_by_link(
            ctx.package_snapshots,
            link,
            ctx.lockfile_dir,
            &workspace_package.root_dir,
        ) else {
            continue;
        };

        let importer_id = to_lockfile_importer_id(ctx.lockfile_dir, &workspace_package.root_dir);
        let Some(target_project_snapshot) =
            project_snapshot_by_importer(ctx.existing_lockfile, &importer_id)
        else {
            continue;
        };

        if !local_directory_snapshot_is_subset_of_project(
            ctx,
            &candidate_snapshot,
            target_project_snapshot,
        ) {
            continue;
        }

        resolved_package.version = ResolvedDependencyVersion::Link(format!(
            "link:{}",
            to_relative_path(project_dir, &workspace_package.root_dir).replace('\\', "/")
        ));
        resolved_package.peer_suffixes.clear();
    }
}

fn resolve_local_directory_snapshot_by_link(
    package_snapshots: &DashMap<DependencyPath, PackageSnapshot>,
    link: &str,
    lockfile_dir: &Path,
    workspace_root_dir: &Path,
) -> Option<PackageSnapshot> {
    let expected_directory = normalized_local_file_reference(lockfile_dir, workspace_root_dir)
        .trim_start_matches("file:")
        .to_string();
    package_snapshots.iter().find_map(|entry| {
        let dependency_path = entry.key();
        let snapshot = entry.value();
        (dependency_path.local_file_reference() == Some(link)
            && matches!(
                &snapshot.resolution,
                LockfileResolution::Directory(resolution) if resolution.directory == expected_directory
            ))
        .then(|| snapshot.clone())
    })
}

fn project_snapshot_by_importer<'a>(
    existing_lockfile: Option<&'a Lockfile>,
    importer_id: &str,
) -> Option<&'a ProjectSnapshot> {
    match existing_lockfile.map(|lockfile| &lockfile.project_snapshot)? {
        RootProjectSnapshot::Single(snapshot) => (importer_id == ".").then_some(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(importer_id),
    }
}

fn local_directory_snapshot_is_subset_of_project(
    ctx: &DedupeInjectedContext<'_>,
    snapshot: &PackageSnapshot,
    project_snapshot: &ProjectSnapshot,
) -> bool {
    let Some(snapshot_dependencies) = snapshot.dependencies.as_ref() else {
        return true;
    };
    let project_dependencies = project_snapshot_dependency_map(project_snapshot);
    snapshot_dependencies.iter().all(|(alias, dependency)| {
        project_dependencies.get(alias).is_some_and(|project_dependency| {
            project_dependency == dependency
                || local_snapshot_dependency_matches_project_dependency(
                    ctx,
                    alias,
                    dependency,
                    project_dependency,
                )
        })
    })
}

fn local_snapshot_dependency_matches_project_dependency(
    ctx: &DedupeInjectedContext<'_>,
    alias: &PkgName,
    dependency: &PackageSnapshotDependency,
    project_dependency: &PackageSnapshotDependency,
) -> bool {
    let (
        PackageSnapshotDependency::Link(candidate_link),
        PackageSnapshotDependency::Link(project_link),
    ) = (dependency, project_dependency)
    else {
        return false;
    };
    if !candidate_link.starts_with("file:") || !project_link.starts_with("link:") {
        return false;
    }

    let alias_name = alias.to_string();
    let Some(workspace_package) = ctx.workspace_packages.get(&alias_name) else {
        return false;
    };
    let Some(candidate_snapshot) = resolve_local_directory_snapshot_by_link(
        ctx.package_snapshots,
        candidate_link,
        ctx.lockfile_dir,
        &workspace_package.root_dir,
    ) else {
        return false;
    };
    let importer_id = to_lockfile_importer_id(ctx.lockfile_dir, &workspace_package.root_dir);
    let Some(target_project_snapshot) =
        project_snapshot_by_importer(ctx.existing_lockfile, &importer_id)
    else {
        return false;
    };

    local_directory_snapshot_is_subset_of_project(ctx, &candidate_snapshot, target_project_snapshot)
}

fn project_snapshot_dependency_map(
    project_snapshot: &ProjectSnapshot,
) -> HashMap<PkgName, PackageSnapshotDependency> {
    let mut dependencies = HashMap::<PkgName, PackageSnapshotDependency>::new();

    for map in [
        project_snapshot.dependencies.as_ref(),
        project_snapshot.optional_dependencies.as_ref(),
        project_snapshot.dev_dependencies.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for (alias, spec) in map {
            dependencies.insert(
                alias.clone(),
                resolved_version_to_snapshot_dependency(&alias.to_string(), &spec.version),
            );
        }
    }

    dependencies
}

fn prune_unreferenced_packages(
    project_snapshot: &RootProjectSnapshot,
    packages: &mut HashMap<DependencyPath, PackageSnapshot>,
) {
    if packages.is_empty() {
        return;
    }

    let mut referenced = HashSet::<DependencyPath>::new();
    let mut queue = Vec::<DependencyPath>::new();

    match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => {
            collect_project_dependency_paths(snapshot, packages, &mut queue);
        }
        RootProjectSnapshot::Multi(snapshot) => {
            for importer in snapshot.importers.values() {
                collect_project_dependency_paths(importer, packages, &mut queue);
            }
        }
    }

    while let Some(candidate_path) = queue.pop() {
        if !referenced.insert(candidate_path.clone()) {
            continue;
        }

        let Some(package_snapshot) = packages.get(&candidate_path) else {
            continue;
        };
        collect_package_snapshot_dependency_paths(package_snapshot, packages, &mut queue);
    }

    packages.retain(|dependency_path, _| referenced.contains(dependency_path));
}

fn collect_project_dependency_paths(
    project_snapshot: &ProjectSnapshot,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    queue: &mut Vec<DependencyPath>,
) {
    for map in [
        project_snapshot.dependencies.as_ref(),
        project_snapshot.optional_dependencies.as_ref(),
        project_snapshot.dev_dependencies.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for (alias, spec) in map {
            if let Some(path) =
                dependency_path_from_resolved_version(alias, &spec.version, packages)
            {
                queue.push(path);
            }
        }
    }
}

fn collect_package_snapshot_dependency_paths(
    package_snapshot: &PackageSnapshot,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    queue: &mut Vec<DependencyPath>,
) {
    for (alias, dependency) in package_dependency_map(package_snapshot) {
        if let Some(path) = dependency_path_from_snapshot_dependency(&alias, &dependency, packages)
        {
            queue.push(path);
        }
    }
}

fn dependency_path_from_resolved_version(
    alias: &PkgName,
    resolved_version: &ResolvedDependencyVersion,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> Option<DependencyPath> {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => Some(DependencyPath::registry(
            None,
            PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        )),
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            Some(DependencyPath::registry(None, specifier.clone()))
        }
        ResolvedDependencyVersion::Link(link) => {
            if !link.starts_with("file:") {
                return None;
            }
            packages
                .keys()
                .find(|dependency_path| {
                    dependency_path.local_file_reference() == Some(link.as_str())
                })
                .cloned()
        }
    }
}

fn dependency_path_from_snapshot_dependency(
    alias: &PkgName,
    dependency: &PackageSnapshotDependency,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
) -> Option<DependencyPath> {
    match dependency {
        PackageSnapshotDependency::PkgVerPeer(ver_peer) => Some(DependencyPath::registry(
            None,
            PkgNameVerPeer::new(alias.clone(), ver_peer.clone()),
        )),
        PackageSnapshotDependency::PkgNameVerPeer(specifier) => {
            Some(DependencyPath::registry(None, specifier.clone()))
        }
        PackageSnapshotDependency::DependencyPath(path) => Some(path.clone()),
        PackageSnapshotDependency::Link(link) => {
            if !link.starts_with("file:") {
                return None;
            }
            packages
                .keys()
                .find(|dependency_path| {
                    dependency_path.local_file_reference() == Some(link.as_str())
                })
                .cloned()
        }
    }
}

fn merge_project_snapshot(
    existing_lockfile: Option<&Lockfile>,
    lockfile_importer_id: &str,
    project_snapshot: ProjectSnapshot,
) -> RootProjectSnapshot {
    match existing_lockfile.map(|lockfile| &lockfile.project_snapshot) {
        Some(RootProjectSnapshot::Multi(snapshot)) => {
            let mut importers = snapshot.importers.clone();
            importers.insert(lockfile_importer_id.to_string(), project_snapshot);
            RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
        }
        Some(RootProjectSnapshot::Single(existing_snapshot)) => {
            if lockfile_importer_id == "." {
                RootProjectSnapshot::Single(project_snapshot)
            } else {
                let mut importers = HashMap::new();
                importers.insert(".".to_string(), existing_snapshot.clone());
                importers.insert(lockfile_importer_id.to_string(), project_snapshot);
                RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
            }
        }
        None => {
            if lockfile_importer_id == "." {
                RootProjectSnapshot::Single(project_snapshot)
            } else {
                let mut importers = HashMap::new();
                importers.insert(lockfile_importer_id.to_string(), project_snapshot);
                RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
            }
        }
    }
}

fn ensure_workspace_importers(
    project_snapshot: RootProjectSnapshot,
    lockfile_dir: &Path,
    workspace_packages: &WorkspacePackages,
) -> RootProjectSnapshot {
    if workspace_packages.is_empty() {
        return project_snapshot;
    }

    let mut importers = match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => HashMap::from([(".".to_string(), snapshot)]),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers,
    };

    for info in workspace_packages.values() {
        let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
        importers.entry(importer_id).or_insert_with(ProjectSnapshot::default);
    }

    RootProjectSnapshot::Multi(MultiProjectSnapshot { importers })
}

fn to_lockfile_importer_id(lockfile_dir: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(lockfile_dir) else {
        return ".".to_string();
    };
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
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

fn normalized_local_file_reference(lockfile_dir: &Path, local_dep_path: &Path) -> String {
    format!(
        "file:{}",
        if local_dep_path.is_absolute() {
            to_relative_path(lockfile_dir, local_dep_path)
        } else {
            local_dep_path.to_string_lossy().replace('\\', "/")
        }
    )
}

fn resolve_local_dependency(
    manifest_path: &Path,
    version_range: &str,
) -> Option<(ResolvedPackage, PathBuf, bool)> {
    let (protocol, target, should_symlink) =
        if let Some(target) = version_range.strip_prefix("link:") {
            ("link:", target, true)
        } else if let Some(target) = version_range.strip_prefix("file:") {
            ("file:", target, false)
        } else {
            return None;
        };
    let project_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let local_dep_path = normalize_local_dependency_path(project_dir, target);
    let normalized = format!("{protocol}{}", target.replace('\\', "/"));
    Some((
        ResolvedPackage::new(ResolvedDependencyVersion::Link(normalized)),
        local_dep_path,
        should_symlink,
    ))
}

fn should_inject_workspace_dependency(
    manifest: &PackageManifest,
    dependency_name: &str,
    specifier: &str,
    config: &Npmrc,
) -> bool {
    if !specifier.starts_with("workspace:") {
        return false;
    }
    config.inject_workspace_packages
        || dependency_meta_injected_json(manifest.value().get("dependenciesMeta"), dependency_name)
}

fn try_resolve_workspace_dependency<'a>(
    config: &Npmrc,
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
    depth: usize,
) -> Option<&'a crate::WorkspacePackageInfo> {
    if specifier.starts_with("workspace:") {
        return resolve_workspace_dependency(workspace_packages, dependency_name, specifier);
    }
    if !config.link_workspace_packages.links_at_depth(depth) {
        return None;
    }
    let (target_name, target_specifier) =
        parse_npm_alias(specifier).unwrap_or((dependency_name, specifier));
    resolve_workspace_dependency_by_plain_spec(workspace_packages, target_name, target_specifier)
}

fn resolve_workspace_dependency_for_install<'a>(
    config: &Npmrc,
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
    depth: usize,
) -> miette::Result<Option<&'a crate::WorkspacePackageInfo>> {
    if specifier.starts_with("workspace:") {
        let project_dir = config.modules_dir.parent().unwrap_or_else(|| Path::new("."));
        if let Some(workspace_package) = resolve_workspace_dependency_by_relative_path(
            workspace_packages,
            project_dir,
            specifier,
        ) {
            return Ok(Some(workspace_package));
        }
        let workspace_package =
            require_workspace_dependency(workspace_packages, dependency_name, specifier)
                .map_err(|error| miette::miette!("{error}"))?;
        return Ok(Some(workspace_package));
    }
    if !config.link_workspace_packages.links_at_depth(depth) {
        return Ok(None);
    }
    let (target_name, target_specifier) =
        parse_npm_alias(specifier).unwrap_or((dependency_name, specifier));
    Ok(resolve_workspace_dependency_by_plain_spec(
        workspace_packages,
        target_name,
        target_specifier,
    ))
}

fn project_dependencies_meta(manifest: &PackageManifest) -> Option<serde_yaml::Value> {
    let value = manifest.value().get("dependenciesMeta")?;
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return None;
    }
    serde_yaml::to_value(value).ok()
}

fn dependency_meta_injected_json(
    dependencies_meta: Option<&serde_json::Value>,
    dependency_name: &str,
) -> bool {
    dependencies_meta
        .and_then(|value| value.get(dependency_name))
        .and_then(|value| value.get("injected"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn json_string_map(value: Option<&serde_json::Value>) -> Option<HashMap<String, String>> {
    let object = value?.as_object()?;
    let map = object
        .iter()
        .filter_map(|(name, value)| {
            value.as_str().map(|value| (name.to_string(), value.to_string()))
        })
        .collect::<HashMap<_, _>>();
    (!map.is_empty()).then_some(map)
}

fn resolved_version_to_snapshot_dependency(
    dependency_name: &str,
    resolved_version: &ResolvedDependencyVersion,
) -> PackageSnapshotDependency {
    match resolved_version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            PackageSnapshotDependency::PkgVerPeer(ver_peer.clone())
        }
        ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
            PackageSnapshotDependency::PkgNameVerPeer(name_ver_peer.clone())
        }
        ResolvedDependencyVersion::Link(link) => {
            let _ = dependency_name;
            PackageSnapshotDependency::Link(link.clone())
        }
    }
}

fn merge_resolved_peer_suffixes(
    target: &mut BTreeMap<String, ResolvedDependencyVersion>,
    source: &BTreeMap<String, ResolvedDependencyVersion>,
) {
    for (peer_name, peer_version) in source {
        target.entry(peer_name.clone()).or_insert_with(|| peer_version.clone());
    }
}

fn append_peer_suffix_to_local_reference(
    reference: &str,
    resolved_peers: &BTreeMap<String, ResolvedDependencyVersion>,
) -> String {
    if resolved_peers.is_empty() {
        return reference.to_string();
    }

    let mut suffixed = reference.to_string();
    for (peer_name, peer_version) in resolved_peers {
        let peer_repr = match peer_version {
            ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
                format!("{peer_name}@{ver_peer}")
            }
            ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
                format!("{}@{}", name_ver_peer.name, name_ver_peer.suffix)
            }
            ResolvedDependencyVersion::Link(link) => {
                format!("{peer_name}@{}", link.strip_prefix("link:").unwrap_or(link))
            }
        };
        suffixed.push('(');
        suffixed.push_str(&peer_repr);
        suffixed.push(')');
    }
    suffixed
}

fn normalize_local_dependency_path(project_dir: &Path, target: &str) -> PathBuf {
    let candidate = Path::new(target);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    project_dir.join(candidate)
}

fn parse_npm_alias(version_range: &str) -> Option<(&str, &str)> {
    let alias = version_range.strip_prefix("npm:")?;
    let separator = alias.rfind('@');
    match separator {
        Some(index) if index > 0 => Some((&alias[..index], &alias[index + 1..])),
        _ => Some((alias, "latest")),
    }
}

struct ResolvePackageVersionContext<'a> {
    config: &'static Npmrc,
    http_client: &'a ThrottledClient,
    lockfile_dir: &'a Path,
    pnpmfile: Option<&'a Path>,
    ignore_pnpmfile: bool,
    prefer_offline: bool,
    offline: bool,
    preferred_versions: Option<&'a PreferredVersions>,
}

async fn resolve_package_version(
    ctx: ResolvePackageVersionContext<'_>,
    name: &str,
    version_range: &str,
) -> miette::Result<PackageVersion> {
    if is_tarball_spec(version_range) {
        crate::progress_reporter::resolved();
        return crate::apply_read_package_hook_to_package_version(
            ctx.lockfile_dir,
            ctx.pnpmfile,
            ctx.ignore_pnpmfile,
            &resolve_package_version_from_tarball_spec(ctx.config, ctx.http_client, version_range)
                .await
                .map_err(|error| {
                    miette::miette!("resolve package version from tarball spec: {error}")
                })?,
        )
        .map_err(|error| {
            miette::miette!("apply .pnpmfile.cjs readPackage hook to tarball package: {error}")
        });
    }
    if is_git_spec(version_range) {
        crate::progress_reporter::resolved();
        return crate::apply_read_package_hook_to_package_version(
            ctx.lockfile_dir,
            ctx.pnpmfile,
            ctx.ignore_pnpmfile,
            &resolve_package_version_from_git_spec(ctx.config, ctx.http_client, version_range)
                .await
                .map_err(|error| {
                    miette::miette!("resolve package version from git spec: {error}")
                })?,
        )
        .map_err(|error| {
            miette::miette!("apply .pnpmfile.cjs readPackage hook to git package: {error}")
        });
    }
    let (requested_name, requested_range) =
        parse_npm_alias(version_range).unwrap_or((name, version_range));
    let package = fetch_package_with_metadata_cache(
        ctx.config,
        ctx.http_client,
        requested_name,
        ctx.prefer_offline,
        ctx.offline,
    )
    .await
    .map_err(|error| miette::miette!("fetch package metadata from registry: {error}"))?;
    let preferred_versions_for_package = ctx
        .preferred_versions
        .and_then(|preferred_versions| preferred_versions.get(requested_name));
    let resolve = |package: &pacquet_registry::Package| {
        resolve_package_version_from_metadata(
            package,
            requested_range,
            preferred_versions_for_package,
        )
    };

    let maybe_cached = if ctx.prefer_offline && !ctx.offline {
        read_cached_package_from_config(ctx.config, requested_name)
    } else {
        None
    };
    let mut package_version = resolve(&package);
    if package_version.is_none() && maybe_cached.is_some() {
        let fresh =
            fetch_package_from_registry_and_cache(ctx.config, ctx.http_client, requested_name)
                .await
                .map_err(|error| {
                    miette::miette!("fetch package metadata from registry: {error}")
                })?;
        package_version = resolve(&fresh);
    }

    crate::progress_reporter::resolved();
    crate::apply_read_package_hook_to_package_version(
        ctx.lockfile_dir,
        ctx.pnpmfile,
        ctx.ignore_pnpmfile,
        &package_version.ok_or_else(|| {
            miette::miette!(
                "resolve package version from metadata: no matching version for `{requested_name}@{requested_range}`"
            )
        })?,
    )
    .map_err(|error| {
        miette::miette!("apply .pnpmfile.cjs readPackage hook to package metadata: {error}")
    })
}

fn resolve_package_version_from_metadata(
    package: &pacquet_registry::Package,
    requested_range: &str,
    preferred_versions: Option<&HashSet<String>>,
) -> Option<PackageVersion> {
    if let Ok(version) = requested_range.parse::<node_semver::Version>() {
        return package.versions.get(&version.to_string()).cloned();
    }
    if let Ok(tag) = requested_range.parse::<PackageTag>() {
        return match tag {
            PackageTag::Latest => Some(package.latest().clone()),
            PackageTag::Version(version) => package.versions.get(&version.to_string()).cloned(),
        };
    }
    if let Some(preferred_versions) = preferred_versions
        && let Ok(range) = requested_range.parse::<node_semver::Range>()
    {
        let mut preferred_matches = preferred_versions
            .iter()
            .filter_map(|version| package.versions.get(version))
            .filter(|package_version| package_version.version.satisfies(&range))
            .collect::<Vec<_>>();
        preferred_matches.sort_by(|left, right| left.version.partial_cmp(&right.version).unwrap());
        if let Some(package_version) = preferred_matches.last() {
            return Some((*package_version).clone());
        }
    }
    package.pinned_version(requested_range).cloned()
}

fn apply_workspace_root_peer_override(
    config: &Npmrc,
    workspace_root_peer_overrides: &HashMap<String, String>,
    peer_dependencies: Option<&HashMap<String, String>>,
    name: &str,
    requested_range: &str,
) -> String {
    if !config.resolve_peers_from_workspace_root {
        return requested_range.to_string();
    }

    let is_peer = peer_dependencies.is_some_and(|peers| peers.contains_key(name));
    if !is_peer {
        return requested_range.to_string();
    }

    workspace_root_peer_overrides.get(name).cloned().unwrap_or_else(|| requested_range.to_string())
}

fn apply_workspace_root_override(
    workspace_root_overrides: &HashMap<String, String>,
    name: &str,
    requested_range: &str,
) -> String {
    workspace_root_overrides.get(name).cloned().unwrap_or_else(|| requested_range.to_string())
}

fn workspace_root_peer_overrides(manifest_path: &Path) -> HashMap<String, String> {
    let Some(start_dir) = manifest_path.parent() else {
        return HashMap::new();
    };
    let Some(workspace_root) = find_workspace_root(start_dir) else {
        return HashMap::new();
    };
    read_dependency_specs(&workspace_root.join("package.json"))
}

fn workspace_root_overrides(manifest_path: &Path) -> HashMap<String, String> {
    let Some(start_dir) = manifest_path.parent() else {
        return HashMap::new();
    };
    let Some(workspace_root) = find_workspace_root(start_dir) else {
        return HashMap::new();
    };

    let mut overrides = HashMap::new();
    if let Ok(text) = fs::read_to_string(workspace_root.join("package.json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(package_overrides) = value
            .get("pnpm")
            .and_then(|pnpm| pnpm.get("overrides"))
            .and_then(serde_json::Value::as_object)
    {
        overrides.extend(package_overrides.iter().filter_map(|(name, spec)| {
            spec.as_str().map(|spec| (name.to_string(), spec.to_string()))
        }));
    }

    if let Ok(text) = fs::read_to_string(workspace_root.join("pnpm-workspace.yaml"))
        && let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text)
        && let Some(workspace_overrides) = value
            .as_mapping()
            .and_then(|root| root.get(serde_yaml::Value::String("overrides".to_string())))
            .and_then(serde_yaml::Value::as_mapping)
    {
        overrides.extend(workspace_overrides.iter().filter_map(|(name, spec)| {
            Some((name.as_str()?.to_string(), spec.as_str()?.to_string()))
        }));
    }

    overrides
}

fn find_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join("pnpm-workspace.yaml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn read_dependency_specs(package_json_path: &Path) -> HashMap<String, String> {
    let Ok(text) = fs::read_to_string(package_json_path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };

    ["dependencies", "optionalDependencies", "devDependencies", "peerDependencies"]
        .into_iter()
        .flat_map(|field| {
            value
                .get(field)
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, spec)| {
                    spec.as_str().map(|spec| (name.to_string(), spec.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn dedupe_project_snapshot(
    project_snapshot: &ProjectSnapshot,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    dedupe_peer_dependents: bool,
) -> ProjectSnapshot {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspacePackageInfo;
    use tempfile::tempdir;

    fn empty_lockfile(project_snapshot: RootProjectSnapshot) -> Lockfile {
        Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: None,
            never_built_dependencies: None,
            ignored_optional_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            extra_fields: Default::default(),
            project_snapshot,
            packages: None,
        }
    }

    #[test]
    fn merge_without_existing_lockfile_non_workspace() {
        let received = merge_project_snapshot(None, ".", ProjectSnapshot::default());
        assert!(matches!(received, RootProjectSnapshot::Single(_)));
    }

    #[test]
    fn merge_without_existing_lockfile_workspace_importer() {
        let received = merge_project_snapshot(None, "packages/app", ProjectSnapshot::default());
        let RootProjectSnapshot::Multi(snapshot) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(snapshot.importers.contains_key("packages/app"));
    }

    #[test]
    fn merge_with_existing_multi_lockfile_keeps_other_importers() {
        let mut importers = HashMap::new();
        importers.insert("packages/old".to_string(), ProjectSnapshot::default());
        let existing =
            empty_lockfile(RootProjectSnapshot::Multi(MultiProjectSnapshot { importers }));

        let received =
            merge_project_snapshot(Some(&existing), "packages/new", ProjectSnapshot::default());
        let RootProjectSnapshot::Multi(snapshot) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(snapshot.importers.contains_key("packages/old"));
        assert!(snapshot.importers.contains_key("packages/new"));
    }

    #[test]
    fn relative_path_from_project_to_workspace_package() {
        let from = Path::new("/repo/packages/app");
        let to = Path::new("/repo/packages/lib");
        assert_eq!(to_relative_path(from, to), "../lib".to_string());
    }

    #[test]
    fn ensure_workspace_importers_adds_missing_packages() {
        let mut workspace_packages = WorkspacePackages::new();
        workspace_packages.insert(
            "@repo/app".to_string(),
            WorkspacePackageInfo {
                root_dir: Path::new("/repo/packages/app").to_path_buf(),
                version: "1.0.0".to_string(),
            },
        );
        workspace_packages.insert(
            "@repo/lib".to_string(),
            WorkspacePackageInfo {
                root_dir: Path::new("/repo/packages/lib").to_path_buf(),
                version: "1.0.0".to_string(),
            },
        );

        let mut importers = HashMap::new();
        importers.insert("packages/app".to_string(), ProjectSnapshot::default());
        let snapshot = RootProjectSnapshot::Multi(MultiProjectSnapshot { importers });

        let received =
            ensure_workspace_importers(snapshot, Path::new("/repo"), &workspace_packages);
        let RootProjectSnapshot::Multi(received) = received else {
            panic!("expected multi project snapshot");
        };
        assert!(received.importers.contains_key("packages/app"));
        assert!(received.importers.contains_key("packages/lib"));
    }

    #[test]
    fn workspace_root_peer_overrides_reads_root_package_dependencies() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_dir = workspace_root.join("packages/app");
        fs::create_dir_all(&project_dir).expect("create project dir");
        fs::write(workspace_root.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n")
            .expect("write workspace manifest");
        fs::write(
            workspace_root.join("package.json"),
            serde_json::json!({
                "name": "workspace-root",
                "version": "1.0.0",
                "dependencies": {
                    "react": "^18.3.0"
                }
            })
            .to_string(),
        )
        .expect("write root package.json");
        fs::write(project_dir.join("package.json"), "{\"name\":\"app\",\"version\":\"1.0.0\"}")
            .expect("write app package.json");

        let overrides = workspace_root_peer_overrides(&project_dir.join("package.json"));
        assert_eq!(overrides.get("react").map(String::as_str), Some("^18.3.0"));
    }

    #[test]
    fn apply_workspace_root_peer_override_uses_workspace_spec_for_peer() {
        let mut config = Npmrc::new();
        config.resolve_peers_from_workspace_root = true;
        let root = HashMap::from([("react".to_string(), "^18.3.0".to_string())]);
        let peers = HashMap::from([("react".to_string(), "^18.0.0".to_string())]);

        let resolved =
            apply_workspace_root_peer_override(&config, &root, Some(&peers), "react", "^18.0.0");
        assert_eq!(resolved, "^18.3.0");
    }

    #[test]
    fn apply_workspace_root_peer_override_keeps_non_peer_range() {
        let mut config = Npmrc::new();
        config.resolve_peers_from_workspace_root = true;
        let root = HashMap::from([("react".to_string(), "^18.3.0".to_string())]);
        let peers = HashMap::from([("typescript".to_string(), "^5.0.0".to_string())]);

        let resolved =
            apply_workspace_root_peer_override(&config, &root, Some(&peers), "react", "^18.0.0");
        assert_eq!(resolved, "^18.0.0");
    }

    fn load_manifest_from_json(dir: &Path, value: serde_json::Value) -> PackageManifest {
        let path = dir.join("package.json");
        fs::write(&path, value.to_string()).expect("write package.json");
        PackageManifest::from_path(path).expect("load package manifest")
    }

    #[test]
    fn build_project_snapshot_excludes_external_link_when_config_enabled() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "external-link": "link:../external",
                    "is-number": "^7.0.0"
                }
            }),
        );
        let resolved = HashMap::from([
            (
                ("external-link".to_string(), "link:../external".to_string()),
                ResolvedPackage::new(ResolvedDependencyVersion::Link(
                    "link:../external".to_string(),
                )),
            ),
            (
                ("is-number".to_string(), "^7.0.0".to_string()),
                ResolvedPackage::new(ResolvedDependencyVersion::PkgVerPeer(
                    "7.0.0".parse().expect("version"),
                )),
            ),
        ]);

        let snapshot = InstallWithLockfile::<'_, [DependencyGroup; 1]>::build_project_snapshot(
            &manifest,
            [DependencyGroup::Prod],
            &resolved,
            true,
        );

        let deps = snapshot.dependencies.expect("dependencies map");
        let specifiers = snapshot.specifiers.expect("specifiers map");
        assert!(deps.contains_key(&"is-number".parse().expect("pkg name")));
        assert!(!deps.contains_key(&"external-link".parse().expect("pkg name")));
        assert!(specifiers.contains_key("is-number"));
        assert!(!specifiers.contains_key("external-link"));
    }

    #[test]
    fn build_project_snapshot_keeps_workspace_protocol_link_when_excluding_links() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "workspace-pkg": "workspace:*"
                }
            }),
        );
        let resolved = HashMap::from([(
            ("workspace-pkg".to_string(), "workspace:*".to_string()),
            ResolvedPackage::new(ResolvedDependencyVersion::Link(
                "link:../workspace-pkg".to_string(),
            )),
        )]);

        let snapshot = InstallWithLockfile::<'_, [DependencyGroup; 1]>::build_project_snapshot(
            &manifest,
            [DependencyGroup::Prod],
            &resolved,
            true,
        );

        let deps = snapshot.dependencies.expect("dependencies map");
        let specifiers = snapshot.specifiers.expect("specifiers map");
        assert!(deps.contains_key(&"workspace-pkg".parse().expect("pkg name")));
        assert_eq!(specifiers.get("workspace-pkg"), Some(&"workspace:*".to_string()));
    }

    #[test]
    fn build_project_snapshot_preserves_dependencies_meta() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "workspace-pkg": "workspace:*"
                },
                "dependenciesMeta": {
                    "workspace-pkg": {
                        "injected": true
                    }
                }
            }),
        );
        let resolved = HashMap::from([(
            ("workspace-pkg".to_string(), "workspace:*".to_string()),
            ResolvedPackage::new(ResolvedDependencyVersion::Link(
                "link:../workspace-pkg".to_string(),
            )),
        )]);

        let snapshot = InstallWithLockfile::<'_, [DependencyGroup; 1]>::build_project_snapshot(
            &manifest,
            [DependencyGroup::Prod],
            &resolved,
            false,
        );

        let dependencies_meta = snapshot.dependencies_meta.expect("dependenciesMeta");
        assert_eq!(dependencies_meta["workspace-pkg"]["injected"], serde_yaml::Value::Bool(true));
    }

    #[test]
    fn append_peer_suffix_to_local_reference_appends_sorted_resolved_peers() {
        let resolved_peers = BTreeMap::from([
            (
                "is-even".to_string(),
                ResolvedDependencyVersion::PkgVerPeer("1.0.0".parse().expect("version")),
            ),
            (
                "is-number".to_string(),
                ResolvedDependencyVersion::PkgVerPeer("7.0.0".parse().expect("version")),
            ),
        ]);

        assert_eq!(
            append_peer_suffix_to_local_reference("file:../src", &resolved_peers),
            "file:../src(is-even@1.0.0)(is-number@7.0.0)"
        );
    }

    #[test]
    fn append_peer_suffix_to_local_reference_strips_link_prefix_from_workspace_peer() {
        let resolved_peers = BTreeMap::from([(
            "project-1".to_string(),
            ResolvedDependencyVersion::Link("link:packages/project-1".to_string()),
        )]);

        assert_eq!(
            append_peer_suffix_to_local_reference("file:packages/project-2", &resolved_peers),
            "file:packages/project-2(project-1@packages/project-1)"
        );
    }

    #[test]
    fn dedupe_injected_direct_dependencies_rewrites_workspace_file_variant_to_link() {
        let dir = tempdir().expect("tempdir");
        let workspace_root = dir.path().join("workspace");
        let project_1_dir = workspace_root.join("packages/project-1");
        let project_2_dir = workspace_root.join("packages/project-2");
        fs::create_dir_all(&project_1_dir).expect("create project-1 dir");
        fs::create_dir_all(&project_2_dir).expect("create project-2 dir");

        let manifest = load_manifest_from_json(
            &project_2_dir,
            serde_json::json!({
                "name": "project-2",
                "version": "1.0.0",
                "dependencies": {
                    "project-1": "workspace:*"
                },
                "dependenciesMeta": {
                    "project-1": {
                        "injected": true
                    }
                }
            }),
        );

        let mut config = Npmrc::new();
        config.dedupe_injected_deps = true;

        let existing_lockfile = empty_lockfile(RootProjectSnapshot::Multi(MultiProjectSnapshot {
            importers: HashMap::from([(
                "packages/project-1".to_string(),
                ProjectSnapshot {
                    specifiers: None,
                    dependencies: Some(HashMap::from([(
                        "is-number".parse().expect("pkg name"),
                        ResolvedDependencySpec {
                            specifier: "7.0.0".to_string(),
                            version: ResolvedDependencyVersion::PkgVerPeer(
                                "7.0.0".parse().expect("version"),
                            ),
                        },
                    )])),
                    optional_dependencies: None,
                    dev_dependencies: None,
                    dependencies_meta: None,
                    publish_directory: None,
                },
            )]),
        }));

        let package_snapshots = DashMap::new();
        package_snapshots.insert(
            DependencyPath::local_file(
                "project-1".parse().expect("pkg name"),
                "file:packages/project-1".to_string(),
            ),
            PackageSnapshot {
                resolution: LockfileResolution::Directory(DirectoryResolution {
                    directory: "packages/project-1".to_string(),
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
                dependencies: Some(HashMap::from([(
                    "is-number".parse().expect("pkg name"),
                    PackageSnapshotDependency::PkgVerPeer("7.0.0".parse().expect("version")),
                )])),
                optional_dependencies: None,
                transitive_peer_dependencies: None,
                dev: None,
                optional: None,
            },
        );

        let mut workspace_packages = WorkspacePackages::new();
        workspace_packages.insert(
            "project-1".to_string(),
            WorkspacePackageInfo { root_dir: project_1_dir.clone(), version: "1.0.0".to_string() },
        );

        let mut resolved_direct_dependencies = HashMap::from([(
            ("project-1".to_string(), "workspace:*".to_string()),
            ResolvedPackage::new(ResolvedDependencyVersion::Link(
                "file:packages/project-1".to_string(),
            )),
        )]);

        dedupe_injected_direct_dependencies(
            &config,
            &manifest,
            Some(&existing_lockfile),
            &workspace_root,
            &workspace_packages,
            &package_snapshots,
            &mut resolved_direct_dependencies,
        );

        assert_eq!(
            resolved_direct_dependencies
                .get(&("project-1".to_string(), "workspace:*".to_string()))
                .expect("resolved dependency")
                .version,
            ResolvedDependencyVersion::Link("link:../project-1".to_string())
        );
    }

    #[test]
    fn prune_unreferenced_packages_removes_unused_local_directory_snapshot() {
        let mut packages = HashMap::from([
            (
                DependencyPath::local_file(
                    "project-1".parse().expect("pkg name"),
                    "file:packages/project-1".to_string(),
                ),
                PackageSnapshot {
                    resolution: LockfileResolution::Directory(DirectoryResolution {
                        directory: "packages/project-1".to_string(),
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
                    dependencies: Some(HashMap::from([(
                        "is-number".parse().expect("pkg name"),
                        PackageSnapshotDependency::PkgVerPeer("7.0.0".parse().expect("version")),
                    )])),
                    optional_dependencies: None,
                    transitive_peer_dependencies: None,
                    dev: None,
                    optional: None,
                },
            ),
            (
                DependencyPath::registry(
                    None,
                    PkgNameVerPeer::new(
                        "is-number".parse().expect("pkg name"),
                        "7.0.0".parse().expect("version"),
                    ),
                ),
                dummy_snapshot_with_dependencies(None, None),
            ),
        ]);

        let project_snapshot = RootProjectSnapshot::Multi(MultiProjectSnapshot {
            importers: HashMap::from([
                (
                    "packages/project-1".to_string(),
                    ProjectSnapshot {
                        specifiers: None,
                        dependencies: Some(HashMap::from([(
                            "is-number".parse().expect("pkg name"),
                            ResolvedDependencySpec {
                                specifier: "7.0.0".to_string(),
                                version: ResolvedDependencyVersion::PkgVerPeer(
                                    "7.0.0".parse().expect("version"),
                                ),
                            },
                        )])),
                        optional_dependencies: None,
                        dev_dependencies: None,
                        dependencies_meta: None,
                        publish_directory: None,
                    },
                ),
                (
                    "packages/project-2".to_string(),
                    ProjectSnapshot {
                        specifiers: None,
                        dependencies: Some(HashMap::from([(
                            "project-1".parse().expect("pkg name"),
                            ResolvedDependencySpec {
                                specifier: "workspace:*".to_string(),
                                version: ResolvedDependencyVersion::Link(
                                    "link:../project-1".to_string(),
                                ),
                            },
                        )])),
                        optional_dependencies: None,
                        dev_dependencies: None,
                        dependencies_meta: None,
                        publish_directory: None,
                    },
                ),
            ]),
        });

        prune_unreferenced_packages(&project_snapshot, &mut packages);

        assert_eq!(packages.len(), 1);
        assert!(packages.contains_key(&DependencyPath::registry(
            None,
            PkgNameVerPeer::new(
                "is-number".parse().expect("pkg name"),
                "7.0.0".parse().expect("version"),
            ),
        )));
    }

    #[test]
    fn expected_registry_tarball_uses_scoped_registry_from_config() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join(".npmrc"),
            "registry=https://default.example/\n@foo:registry=https://foo.example/\n",
        )
        .expect("write .npmrc");
        let config = Npmrc::current(|| Ok::<_, ()>(dir.path().to_path_buf()), || None, Npmrc::new)
            .expect("load npmrc");
        let package_version = PackageVersion {
            name: "@foo/pkg".to_string(),
            version: "1.2.3".parse().expect("version"),
            dist: pacquet_registry::PackageDistribution {
                tarball: "https://foo.example/@foo/pkg/-/pkg-1.2.3.tgz".to_string(),
                integrity: None,
                shasum: None,
                file_count: None,
                unpacked_size: None,
            },
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: None,
            bin: None,
            homepage: None,
            repository: None,
        };

        assert_eq!(
            expected_registry_tarball(&config, &package_version),
            "https://foo.example/@foo/pkg/-/pkg-1.2.3.tgz"
        );
    }

    #[test]
    fn to_package_snapshot_preserves_deprecated_message() {
        let config = Box::leak(Box::new(Npmrc::new()));
        let package_version = PackageVersion {
            name: "pkg".to_string(),
            version: "1.0.0".parse().expect("version"),
            dist: pacquet_registry::PackageDistribution {
                tarball: "https://registry.npmjs.org/pkg/-/pkg-1.0.0.tgz".to_string(),
                integrity: Some("sha512-deadbeef".parse().expect("integrity")),
                shasum: None,
                file_count: None,
                unpacked_size: None,
            },
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: Some("use pkg2".to_string()),
            bin: None,
            homepage: None,
            repository: None,
        };

        let snapshot = InstallWithLockfile::<'_, [DependencyGroup; 0]>::to_package_snapshot(
            config,
            &package_version,
            HashMap::new(),
            HashMap::new(),
            false,
        );

        assert_eq!(snapshot.deprecated.as_deref(), Some("use pkg2"));
    }

    #[test]
    fn downgrade_existing_snapshot_optional_flag_clears_optional_for_required_reuse() {
        let dependency_path: DependencyPath = "/foo@1.0.0".parse().expect("dependency path");
        let package_snapshots = DashMap::new();
        package_snapshots.insert(
            dependency_path.clone(),
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
                optional: Some(true),
            },
        );

        downgrade_existing_snapshot_optional_flag(&package_snapshots, &dependency_path, false);

        assert_eq!(package_snapshots.get(&dependency_path).expect("snapshot").optional, None);
    }

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

        let deduped = dedupe_project_snapshot(&snapshot, &packages, true);
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

        let deduped = dedupe_project_snapshot(&snapshot, &packages, false);
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

    #[test]
    fn unique_direct_dependencies_dedupes_same_name_and_spec_across_groups() {
        let manifest = serde_json::json!({
            "dependencies": {
                "foo": "^1.0.0"
            },
            "devDependencies": {
                "foo": "^1.0.0",
                "bar": "^2.0.0"
            }
        });

        let items =
            unique_direct_dependencies(&manifest, &[DependencyGroup::Prod, DependencyGroup::Dev]);

        assert_eq!(
            items,
            vec![
                (DependencyGroup::Prod, "foo".to_string(), "^1.0.0".to_string()),
                (DependencyGroup::Dev, "bar".to_string(), "^2.0.0".to_string()),
            ]
        );
    }

    #[test]
    fn resolve_package_version_from_metadata_prefers_matching_legacy_lockfile_version() {
        let package: pacquet_registry::Package = serde_json::from_value(serde_json::json!({
            "name": "foo",
            "dist-tags": { "latest": "2.0.0" },
            "versions": {
                "1.0.0": {
                    "name": "foo",
                    "version": "1.0.0",
                    "dist": { "tarball": "https://registry.example/foo/-/foo-1.0.0.tgz" }
                },
                "2.0.0": {
                    "name": "foo",
                    "version": "2.0.0",
                    "dist": { "tarball": "https://registry.example/foo/-/foo-2.0.0.tgz" }
                }
            }
        }))
        .expect("parse package metadata");

        let preferred = std::collections::HashSet::from(["1.0.0".to_string()]);
        let resolved = resolve_package_version_from_metadata(&package, "*", Some(&preferred))
            .expect("resolved version");

        assert_eq!(resolved.version.to_string(), "1.0.0");
    }

    #[test]
    fn resolve_package_version_from_metadata_falls_back_when_preferred_version_misses_range() {
        let package: pacquet_registry::Package = serde_json::from_value(serde_json::json!({
            "name": "foo",
            "dist-tags": { "latest": "2.0.0" },
            "versions": {
                "1.0.0": {
                    "name": "foo",
                    "version": "1.0.0",
                    "dist": { "tarball": "https://registry.example/foo/-/foo-1.0.0.tgz" }
                },
                "2.0.0": {
                    "name": "foo",
                    "version": "2.0.0",
                    "dist": { "tarball": "https://registry.example/foo/-/foo-2.0.0.tgz" }
                }
            }
        }))
        .expect("parse package metadata");

        let preferred = std::collections::HashSet::from(["1.0.0".to_string()]);
        let resolved = resolve_package_version_from_metadata(&package, "^2.0.0", Some(&preferred))
            .expect("resolved version");

        assert_eq!(resolved.version.to_string(), "2.0.0");
    }
}
