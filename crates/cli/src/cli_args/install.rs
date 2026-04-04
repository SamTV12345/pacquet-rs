use crate::{
    State,
    state::{collect_workspace_state_projects, read_workspace_package_patterns},
};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_executor::{ExecuteLifecycleScript, execute_lifecycle_script};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::{LinkWorkspacePackages, NodeLinker, Npmrc};
use pacquet_package_manager::{
    Install, InstallFrozenWorkspace, InstallReporter, PreferredVersions,
    WorkspaceFrozenInstallTarget, WorkspacePackages,
    current_lockfile_for_installers_preserving_unselected_importers, finish_progress_reporter,
    link_bins_for_manifest, start_progress_reporter, warn_progress_reporter,
    write_modules_manifest,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_store_dir::StoreDir;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Default, Args)]
pub struct InstallDependencyOptions {
    /// pacquet will not install any package listed in devDependencies and will remove those insofar
    /// they were already installed, if the NODE_ENV environment variable is set to production.
    /// Use this flag to instruct pacquet to ignore NODE_ENV and take its production status from this
    /// flag instead.
    #[arg(short = 'P', long)]
    pub(crate) prod: bool,
    /// Only devDependencies are installed and dependencies are removed insofar they were
    /// already installed, regardless of the NODE_ENV.
    #[arg(short = 'D', long)]
    pub(crate) dev: bool,
    /// optionalDependencies are not installed.
    #[arg(long)]
    pub(crate) no_optional: bool,
}

impl InstallDependencyOptions {
    /// Convert the dependency options to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    pub(crate) fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &InstallDependencyOptions { prod, dev, no_optional } = self;
        let has_both = prod == dev;
        let has_prod = has_both || prod;
        let has_dev = has_both || dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Prod))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: InstallDependencyOptions,

    /// Don't generate a lockfile and fail if the lockfile is outdated.
    #[clap(long)]
    pub frozen_lockfile: bool,

    /// Prefer lockfile-only resolution when a compatible lockfile is present.
    #[clap(long, conflicts_with = "no_prefer_frozen_lockfile")]
    pub prefer_frozen_lockfile: bool,

    /// Disable lockfile-only preference and always resolve dependencies online/offline as needed.
    #[clap(long, conflicts_with = "prefer_frozen_lockfile")]
    pub no_prefer_frozen_lockfile: bool,

    /// Fix broken lockfile entries and proceed even when frozen lockfile checks would fail.
    #[clap(long)]
    pub fix_lockfile: bool,

    /// Skip lifecycle scripts during installation.
    #[clap(long)]
    pub ignore_scripts: bool,

    /// Resolve dependencies and write pnpm-lock.yaml without installing into node_modules.
    #[clap(long)]
    pub lockfile_only: bool,

    /// Force reinstall dependencies and bypass local store/virtual-store reuse shortcuts.
    #[clap(long)]
    pub force: bool,

    /// Resolve dependencies only and write lockfile changes without installing.
    #[clap(long)]
    pub resolution_only: bool,

    /// Disable pnpm hooks defined in .pnpmfile.cjs.
    #[clap(long)]
    pub ignore_pnpmfile: bool,

    /// Use hooks from the specified pnpmfile instead of <lockfileDir>/.pnpmfile.cjs.
    #[clap(long)]
    pub pnpmfile: Option<PathBuf>,

    /// Reporter name (accepted for compatibility).
    #[clap(long)]
    pub reporter: Option<String>,

    /// Starts a store server in the background (currently accepted for compatibility).
    #[clap(long)]
    pub use_store_server: bool,

    /// Hoist all dependencies to the root of the virtual store.
    #[clap(long)]
    pub shamefully_hoist: bool,

    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    pub filter: Vec<String>,
    /// Install recursively in every workspace project (including workspace root).
    #[clap(short = 'r', long)]
    pub recursive: bool,

    /// Skip staleness checks for cached metadata and prefer local metadata when possible.
    #[clap(long)]
    pub prefer_offline: bool,

    /// Disallow network requests and use only locally available lockfile/store data.
    #[clap(long)]
    pub offline: bool,
}

impl InstallArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        self.run_with_preferred_versions(state, None).await
    }

    pub(crate) async fn run_with_preferred_versions(
        self,
        state: State,
        preferred_versions: Option<PreferredVersions>,
    ) -> miette::Result<()> {
        let State {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            resolved_packages,
        } = &state;
        let InstallArgs {
            dependency_options,
            frozen_lockfile,
            prefer_frozen_lockfile,
            no_prefer_frozen_lockfile,
            fix_lockfile,
            ignore_scripts,
            lockfile_only,
            force,
            resolution_only,
            ignore_pnpmfile,
            pnpmfile,
            reporter,
            use_store_server: _use_store_server,
            shamefully_hoist,
            filter,
            recursive,
            prefer_offline,
            offline,
        } = self;
        let lockfile_only = lockfile_only || resolution_only;
        let frozen_lockfile = frozen_lockfile && !fix_lockfile;
        let reporter = parse_install_reporter(reporter.as_deref())?;
        let prefer_frozen_lockfile_override = if prefer_frozen_lockfile {
            Some(true)
        } else if no_prefer_frozen_lockfile {
            Some(false)
        } else {
            None
        };
        let dependency_groups = dependency_options.dependency_groups().collect::<Vec<_>>();

        let mut install_targets = BTreeMap::<String, PathBuf>::new();
        if recursive && !workspace_packages.is_empty() {
            let workspace_root_manifest = lockfile_dir.join("package.json");
            if workspace_root_manifest.is_file() {
                install_targets.insert(".".to_string(), workspace_root_manifest);
            }
            for info in workspace_packages.values() {
                let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                install_targets.insert(importer_id, info.root_dir.join("package.json"));
            }
        } else {
            install_targets.insert(lockfile_importer_id.clone(), manifest.path().to_path_buf());

            let is_workspace_root = lockfile_importer_id == "."
                && manifest.path().parent().is_some_and(|parent| parent == lockfile_dir.as_path());
            if is_workspace_root {
                for info in workspace_packages.values() {
                    let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                    install_targets
                        .entry(importer_id)
                        .or_insert_with(|| info.root_dir.join("package.json"));
                }
            }
        }
        install_targets = apply_workspace_filters(
            install_targets,
            manifest,
            workspace_packages,
            lockfile_dir,
            &filter,
        )?;

        let multiple_targets = install_targets.len() > 1;
        let selected_importers = install_targets.keys().cloned().collect::<HashSet<_>>();
        let install_target_entries = install_targets.into_iter().collect::<Vec<_>>();
        let mut skipped_dep_paths = HashSet::<String>::new();
        let current_lockfile = lockfile.clone();
        let config_project_dir =
            manifest.path().parent().map(Path::to_path_buf).unwrap_or_else(|| lockfile_dir.clone());
        let current_lockfile_virtual_store_dir =
            effective_virtual_store_dir(config, lockfile_dir, &config_project_dir);

        if lockfile_only
            && reporter != InstallReporter::Silent
            && current_lockfile_virtual_store_dir.join("lock.yaml").exists()
        {
            warn_progress_reporter(
                "`node_modules` is present. Lockfile only installation will make it out-of-date",
            );
        }
        if should_parallelize_workspace_frozen_install(
            multiple_targets,
            frozen_lockfile,
            lockfile_only,
            force,
        ) {
            let lockfile = current_lockfile.as_ref().ok_or_else(|| {
                miette::miette!("parallel frozen workspace install requires pnpm-lock.yaml")
            })?;
            let root_config: &'static Npmrc = config_for_project(
                config,
                lockfile_dir,
                lockfile_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
            )
            .leak();
            let mut targets = Vec::new();
            let mut total_direct_dependencies = 0usize;
            for (importer_id, manifest_path) in &install_target_entries {
                let target_manifest = PackageManifest::from_path(manifest_path.clone())
                    .wrap_err_with(|| {
                        format!("load workspace manifest: {}", manifest_path.display())
                    })?;
                total_direct_dependencies +=
                    target_manifest.dependencies(dependency_groups.iter().copied()).count();
                let project_dir = target_manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.to_path_buf());
                let target_config: &'static Npmrc = config_for_project(
                    config,
                    lockfile_dir,
                    &project_dir,
                    shamefully_hoist,
                    prefer_frozen_lockfile_override,
                )
                .leak();
                let project_snapshot = match &lockfile.project_snapshot {
                    pacquet_lockfile::RootProjectSnapshot::Single(snapshot) => {
                        if importer_id != "." {
                            miette::bail!("Cannot find importer `{importer_id}` in pnpm-lock.yaml");
                        }
                        snapshot.clone()
                    }
                    pacquet_lockfile::RootProjectSnapshot::Multi(snapshot) => snapshot
                        .importers
                        .get(importer_id.as_str())
                        .ok_or_else(|| {
                            miette::miette!(
                                "Cannot find importer `{importer_id}` in pnpm-lock.yaml"
                            )
                        })?
                        .clone(),
                };
                targets.push(WorkspaceFrozenInstallTarget {
                    importer_id: importer_id.clone(),
                    config: target_config,
                    manifest: target_manifest,
                    project_snapshot,
                });
            }

            start_progress_reporter(total_direct_dependencies, true, reporter, None);
            let result = InstallFrozenWorkspace {
                http_client,
                resolved_packages,
                shared_config: root_config,
                lockfile,
                targets,
                packages: lockfile.packages.as_ref(),
                lockfile_dir,
                dependency_groups: dependency_groups.clone(),
                offline,
                force,
                pnpmfile: pnpmfile.as_deref(),
                ignore_pnpmfile,
            }
            .run()
            .await;
            let _ = finish_progress_reporter(result.is_ok());
            skipped_dep_paths.extend(result?);
        } else {
            // Workspace batch resolution: resolve ALL importers in a single
            // pass (matching pnpm's atomic workspace resolution). The primary
            // importer is the first entry; additional importers are passed as
            // `additional_importers` so the dependency graph is resolved once
            // with global dedup across the entire workspace.
            let (primary_id, primary_manifest_path) =
                install_target_entries.first().expect("at least one install target");
            let primary_workspace_manifest = if primary_manifest_path == manifest.path() {
                None
            } else {
                Some(PackageManifest::from_path(primary_manifest_path.clone()).wrap_err_with(
                    || format!("load workspace manifest: {}", primary_manifest_path.display()),
                )?)
            };
            let primary_manifest = primary_workspace_manifest.as_ref().unwrap_or(manifest);
            let primary_dir = primary_manifest
                .path()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| lockfile_dir.to_path_buf());
            let primary_config: &'static Npmrc = config_for_project(
                config,
                lockfile_dir,
                &primary_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
            )
            .leak();

            // Build additional importers list (all except primary)
            let mut additional_importers = Vec::new();
            for (importer_id, manifest_path) in install_target_entries.iter().skip(1) {
                let imp_manifest = PackageManifest::from_path(manifest_path.clone())
                    .wrap_err_with(|| {
                        format!("load workspace manifest: {}", manifest_path.display())
                    })?;
                let imp_dir = imp_manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.to_path_buf());
                let imp_config: &'static Npmrc = config_for_project(
                    config,
                    lockfile_dir,
                    &imp_dir,
                    shamefully_hoist,
                    prefer_frozen_lockfile_override,
                )
                .leak();
                additional_importers.push(pacquet_package_manager::AdditionalImporter {
                    importer_id: importer_id.clone(),
                    manifest: imp_manifest,
                    config: imp_config,
                    _phantom: std::marker::PhantomData,
                });
            }

            let skipped = Install {
                tarball_mem_cache,
                http_client,
                config: primary_config,
                manifest: primary_manifest,
                lockfile: current_lockfile.as_ref(),
                lockfile_dir,
                lockfile_importer_id: primary_id,
                workspace_packages,
                preferred_versions: preferred_versions.as_ref(),
                dependency_groups: dependency_groups.iter().copied(),
                frozen_lockfile,
                lockfile_only,
                force,
                prefer_offline,
                offline,
                pnpmfile: pnpmfile.as_deref(),
                ignore_pnpmfile,
                // Batch workspace resolution resolves ALL importers in one
                // pass, so the summary should be a single aggregate (matching
                // pnpm's output).  `reporter_prefix` is only used for
                // per-importer recursive output; set it to None here so that
                // `print_pnpm_style_summary` uses the aggregate format with
                // `Packages: +N` / `Done in ...`.
                reporter_prefix: None,
                reporter,
                print_summary: true,
                manage_progress_reporter: true,
                resolved_packages,
                additional_importers,
            }
            .run()
            .await?;
            skipped_dep_paths.extend(skipped);
        }

        if !ignore_scripts && !lockfile_only {
            run_install_lifecycle_scripts_for_targets(
                &install_target_entries,
                config,
                lockfile_dir,
                workspace_packages,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
                &dependency_groups,
            )?;
        }

        if config.lockfile
            && !lockfile_only
            && let Some(lockfile) = current_lockfile.as_ref()
        {
            let skipped_dep_paths_vec = skipped_dep_paths.iter().cloned().collect::<Vec<_>>();
            let current_lockfile = current_lockfile_for_installers_preserving_unselected_importers(
                lockfile,
                config,
                &selected_importers,
                &dependency_groups,
                &skipped_dep_paths,
            );
            current_lockfile
                .save_to_path(&current_lockfile_virtual_store_dir.join("lock.yaml"))
                .wrap_err("write node_modules/.pnpm/lock.yaml")?;
            rewrite_root_modules_manifest_from_current_lockfile(
                config,
                lockfile_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
                &dependency_groups,
                &skipped_dep_paths_vec,
                &current_lockfile,
            )?;
        }

        if !ignore_scripts && !lockfile_only && multiple_targets {
            relink_workspace_bins_after_lifecycle(
                &install_target_entries,
                config,
                lockfile_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
                &dependency_groups,
            )?;
        }

        if !lockfile_only {
            let workspace_state_config = config_for_project(
                config,
                lockfile_dir,
                lockfile_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
            );
            write_workspace_state(
                lockfile_dir,
                &workspace_state_config,
                current_lockfile.as_ref(),
                &dependency_groups,
                !filter.is_empty(),
                ignore_pnpmfile,
                pnpmfile.as_deref(),
            )?;
        }

        Ok(())
    }
}

fn should_parallelize_workspace_frozen_install(
    multiple_targets: bool,
    frozen_lockfile: bool,
    lockfile_only: bool,
    force: bool,
) -> bool {
    multiple_targets && frozen_lockfile && !lockfile_only && !force
}

pub(crate) fn parse_install_reporter(value: Option<&str>) -> miette::Result<InstallReporter> {
    match value.unwrap_or("default") {
        "default" => Ok(InstallReporter::Default),
        "append-only" | "appendOnly" => Ok(InstallReporter::AppendOnly),
        "silent" => Ok(InstallReporter::Silent),
        other => miette::bail!(
            "Unsupported reporter `{other}`. Supported values: default, append-only, silent"
        ),
    }
}

fn apply_workspace_filters(
    install_targets: BTreeMap<String, PathBuf>,
    root_manifest: &PackageManifest,
    workspace_packages: &WorkspacePackages,
    lockfile_dir: &Path,
    filters: &[String],
) -> miette::Result<BTreeMap<String, PathBuf>> {
    if filters.is_empty() {
        return Ok(install_targets);
    }

    let root_name = root_manifest
        .value()
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);

    let selected = install_targets
        .into_iter()
        .filter(|(importer_id, _)| {
            filters.iter().any(|selector| {
                let normalized_selector = selector.trim_start_matches("./").replace('\\', "/");
                if importer_id == &normalized_selector || importer_id == selector {
                    return true;
                }

                if importer_id == "." {
                    return root_name
                        .as_deref()
                        .is_some_and(|name| name == selector || name == normalized_selector);
                }

                workspace_packages.iter().any(|(name, info)| {
                    to_lockfile_importer_id(lockfile_dir, &info.root_dir) == *importer_id
                        && (name == selector || name == &normalized_selector)
                })
            })
        })
        .collect::<BTreeMap<_, _>>();

    if selected.is_empty() {
        miette::bail!("No workspace projects matched --filter selectors: {}", filters.join(", "));
    }

    Ok(selected)
}

pub(crate) fn run_install_lifecycle_scripts(
    manifest_path: PathBuf,
    config: &Npmrc,
) -> miette::Result<()> {
    let manifest = PackageManifest::from_path(manifest_path.clone())
        .wrap_err("reload package.json for lifecycle scripts")?;
    let package_dir =
        manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let init_cwd = std::env::current_dir().into_diagnostic().wrap_err("get current directory")?;
    const LIFECYCLE_SCRIPTS: [&str; 7] = [
        "pnpm:devPreinstall",
        "preinstall",
        "install",
        "postinstall",
        "preprepare",
        "prepare",
        "postprepare",
    ];

    for script_name in LIFECYCLE_SCRIPTS {
        let Some(script) = manifest.script(script_name, true)? else {
            continue;
        };
        execute_lifecycle_script(ExecuteLifecycleScript {
            pkg_root: &package_dir,
            package_json_path: &manifest_path,
            script_name,
            script,
            args: &[],
            extra_env: &[],
            script_shell: config.script_shell.as_deref(),
            shell_emulator: config.shell_emulator,
            init_cwd: &init_cwd,
        })
        .wrap_err_with(|| format!("executing install lifecycle script `{script_name}`"))?;
    }

    Ok(())
}

fn effective_virtual_store_dir(config: &Npmrc, lockfile_dir: &Path, project_dir: &Path) -> PathBuf {
    let default_project_virtual_store_dir = config.modules_dir.join(".pnpm");
    if project_dir != lockfile_dir && config.virtual_store_dir == default_project_virtual_store_dir
    {
        lockfile_dir.join("node_modules/.pnpm")
    } else if config.virtual_store_dir.is_absolute() {
        config.virtual_store_dir.clone()
    } else {
        lockfile_dir.join(&config.virtual_store_dir)
    }
}

fn rewrite_root_modules_manifest_from_current_lockfile(
    config: &Npmrc,
    lockfile_dir: &Path,
    shamefully_hoist: bool,
    prefer_frozen_lockfile_override: Option<bool>,
    dependency_groups: &[DependencyGroup],
    skipped_dep_paths: &[String],
    current_lockfile: &Lockfile,
) -> miette::Result<()> {
    let root_manifest_path = lockfile_dir.join("package.json");
    if !root_manifest_path.is_file() {
        return Ok(());
    }
    let root_manifest = PackageManifest::from_path(root_manifest_path.clone())
        .wrap_err_with(|| format!("reload root manifest: {}", root_manifest_path.display()))?;
    let root_config: &'static Npmrc = config_for_project(
        config,
        lockfile_dir,
        lockfile_dir,
        shamefully_hoist,
        prefer_frozen_lockfile_override,
    )
    .leak();
    let direct_dependency_names = root_manifest
        .dependencies(dependency_groups.iter().copied())
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    write_modules_manifest(
        &root_config.modules_dir,
        root_config,
        dependency_groups,
        skipped_dep_paths,
        current_lockfile.packages.as_ref(),
        Some(&direct_dependency_names),
    )
    .into_diagnostic()
    .wrap_err("rewrite root node_modules/.modules.yaml from current lockfile")?;
    Ok(())
}

fn config_for_project(
    config: &Npmrc,
    lockfile_dir: &Path,
    project_dir: &Path,
    shamefully_hoist: bool,
    prefer_frozen_lockfile_override: Option<bool>,
) -> Npmrc {
    let mut next = config.clone();
    next.store_dir = StoreDir::new(config.store_dir.display().to_string());
    next.modules_dir = project_dir.join("node_modules");
    next.virtual_store_dir = effective_virtual_store_dir(config, lockfile_dir, project_dir);
    next.node_linker = match config.node_linker {
        NodeLinker::Isolated => NodeLinker::Isolated,
        NodeLinker::Hoisted => NodeLinker::Hoisted,
        NodeLinker::Pnp => NodeLinker::Pnp,
    };
    if shamefully_hoist {
        next.shamefully_hoist = true;
        next.hoist = true;
    }
    if let Some(value) = prefer_frozen_lockfile_override {
        next.prefer_frozen_lockfile = value;
    }
    next.apply_derived_settings();
    next
}

fn write_workspace_state(
    workspace_dir: &Path,
    config: &Npmrc,
    current_lockfile: Option<&Lockfile>,
    dependency_groups: &[DependencyGroup],
    filtered_install: bool,
    ignore_pnpmfile: bool,
    pnpmfile: Option<&Path>,
) -> miette::Result<()> {
    // pnpm only populates `projects` when running inside a workspace
    // (i.e. when pnpm-workspace.yaml exists). For single-project installs
    // `allProjects` is `[]` so `projects` is `{}`.
    let is_workspace = workspace_dir.join("pnpm-workspace.yaml").is_file();
    let mut projects_json = Map::new();
    if is_workspace {
        let projects = collect_workspace_state_projects(workspace_dir);
        for project in projects {
            let mut project_entry = Map::new();
            if let Some(name) = project.name {
                project_entry.insert("name".to_string(), Value::String(name));
            }
            if let Some(version) = project.version {
                project_entry.insert("version".to_string(), Value::String(version));
            }
            projects_json
                .insert(project.root_dir.display().to_string(), Value::Object(project_entry));
        }
    }

    let pnpmfiles = if ignore_pnpmfile {
        Vec::new()
    } else if let Some(pnpmfile) = pnpmfile {
        vec![pnpmfile.display().to_string()]
    } else {
        let default_pnpmfile = workspace_dir.join(".pnpmfile.cjs");
        if default_pnpmfile.is_file() {
            vec![default_pnpmfile.display().to_string()]
        } else {
            Vec::new()
        }
    };

    // pnpm uses Ramda `pick` which only includes keys that actually exist
    // on the settings object. For single-project installs, keys like
    // `catalogs` and `workspacePackagePatterns` are undefined and therefore
    // omitted from the output. We replicate this by conditionally inserting.
    let mut settings = Map::new();
    settings.insert("autoInstallPeers".into(), json!(config.auto_install_peers));

    // catalogs: only include if non-empty
    let catalogs = current_lockfile
        .and_then(|lockfile| lockfile.catalogs.clone())
        .and_then(|catalogs| serde_json::to_value(catalogs).ok())
        .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()));
    if let Some(catalogs) = catalogs {
        settings.insert("catalogs".into(), catalogs);
    }

    settings.insert("dedupeDirectDeps".into(), json!(false));
    settings.insert("dedupeInjectedDeps".into(), json!(config.dedupe_injected_deps));
    settings.insert("dedupePeerDependents".into(), json!(config.dedupe_peer_dependents));
    settings.insert("dedupePeers".into(), json!(false));
    settings.insert("dev".into(), json!(dependency_groups.contains(&DependencyGroup::Dev)));
    settings.insert("excludeLinksFromLockfile".into(), json!(config.exclude_links_from_lockfile));
    settings.insert("hoistPattern".into(), json!(config.hoist_pattern));
    settings.insert("hoistWorkspacePackages".into(), json!(true));
    settings.insert("injectWorkspacePackages".into(), json!(config.inject_workspace_packages));
    settings.insert(
        "linkWorkspacePackages".into(),
        match config.link_workspace_packages {
            LinkWorkspacePackages::False => Value::Bool(false),
            LinkWorkspacePackages::Direct => Value::Bool(true),
            LinkWorkspacePackages::Deep => Value::String("deep".to_string()),
        },
    );
    settings.insert(
        "nodeLinker".into(),
        json!(match config.node_linker {
            NodeLinker::Isolated => "isolated",
            NodeLinker::Hoisted => "hoisted",
            NodeLinker::Pnp => "pnp",
        }),
    );
    settings
        .insert("optional".into(), json!(dependency_groups.contains(&DependencyGroup::Optional)));
    settings.insert("peersSuffixMaxLength".into(), json!(config.peers_suffix_max_length));
    settings.insert("preferWorkspacePackages".into(), json!(false));
    settings.insert("production".into(), json!(dependency_groups.contains(&DependencyGroup::Prod)));
    settings.insert("publicHoistPattern".into(), json!(config.public_hoist_pattern));

    // workspacePackagePatterns: only include if in a workspace
    if is_workspace {
        let workspace_package_patterns =
            read_workspace_package_patterns(workspace_dir).unwrap_or_default();
        settings.insert("workspacePackagePatterns".into(), json!(workspace_package_patterns));
    }

    let state = json!({
        "lastValidatedTimestamp": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_millis() as u64,
        "projects": Value::Object(projects_json),
        "pnpmfiles": pnpmfiles,
        "settings": Value::Object(settings),
        "filteredInstall": filtered_install,
    });

    let workspace_state_path = workspace_dir.join("node_modules/.pnpm-workspace-state-v1.json");
    let workspace_state_parent =
        workspace_state_path.parent().expect("workspace state path should have parent");
    fs::create_dir_all(workspace_state_parent)
        .into_diagnostic()
        .wrap_err("create node_modules for workspace state")?;
    let content = serde_json::to_string_pretty(&state)
        .into_diagnostic()
        .wrap_err("serialize workspace state")?
        + "\n";
    fs::write(&workspace_state_path, content)
        .into_diagnostic()
        .wrap_err("write node_modules/.pnpm-workspace-state-v1.json")?;
    Ok(())
}

fn run_install_lifecycle_scripts_for_targets(
    install_target_entries: &[(String, PathBuf)],
    config: &Npmrc,
    lockfile_dir: &Path,
    workspace_packages: &WorkspacePackages,
    shamefully_hoist: bool,
    prefer_frozen_lockfile_override: Option<bool>,
    dependency_groups: &[DependencyGroup],
) -> miette::Result<()> {
    let ordered_manifests = order_install_targets_for_lifecycle_scripts(
        install_target_entries,
        config,
        workspace_packages,
    )?;
    for manifest_path in ordered_manifests {
        let project_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        let target_config: &'static Npmrc = config_for_project(
            config,
            lockfile_dir,
            &project_dir,
            shamefully_hoist,
            prefer_frozen_lockfile_override,
        )
        .leak();
        run_install_lifecycle_scripts(manifest_path, target_config)?;
        if install_target_entries.len() > 1 {
            relink_workspace_bins_after_lifecycle(
                install_target_entries,
                config,
                lockfile_dir,
                shamefully_hoist,
                prefer_frozen_lockfile_override,
                dependency_groups,
            )?;
        }
    }

    Ok(())
}

fn order_install_targets_for_lifecycle_scripts(
    install_target_entries: &[(String, PathBuf)],
    config: &Npmrc,
    workspace_packages: &WorkspacePackages,
) -> miette::Result<Vec<PathBuf>> {
    struct LifecycleTarget {
        manifest_path: PathBuf,
        dependencies: Vec<PathBuf>,
    }

    fn normalize_existing_dir(path: PathBuf) -> PathBuf {
        dunce::canonicalize(&path).unwrap_or(path)
    }

    let mut targets = Vec::<LifecycleTarget>::new();
    let mut target_index_by_dir = std::collections::HashMap::<PathBuf, usize>::new();

    for (_, manifest_path) in install_target_entries {
        let manifest = PackageManifest::from_path(manifest_path.clone())
            .wrap_err_with(|| format!("reload workspace manifest: {}", manifest_path.display()))?;
        let project_dir =
            manifest.path().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let project_dir = normalize_existing_dir(project_dir);
        let mut dependencies = Vec::new();
        for (name, specifier) in manifest.dependencies([
            DependencyGroup::Prod,
            DependencyGroup::Dev,
            DependencyGroup::Optional,
        ]) {
            if let Some(relative) = specifier.strip_prefix("link:") {
                dependencies.push(normalize_existing_dir(project_dir.join(relative)));
                continue;
            }
            if let Some(info) = pacquet_package_manager::expected_workspace_dependency_for_install(
                config,
                workspace_packages,
                name,
                specifier,
                0,
            ) {
                dependencies.push(normalize_existing_dir(info.root_dir.clone()));
            }
        }
        let index = targets.len();
        target_index_by_dir.insert(project_dir.clone(), index);
        targets
            .push(LifecycleTarget { manifest_path: manifest.path().to_path_buf(), dependencies });
    }

    let mut adjacency = vec![Vec::<usize>::new(); targets.len()];
    let mut indegree = vec![0usize; targets.len()];
    for (index, target) in targets.iter().enumerate() {
        let mut seen = HashSet::new();
        for dependency_dir in &target.dependencies {
            let Some(&dependency_index) = target_index_by_dir.get(dependency_dir) else {
                continue;
            };
            if dependency_index == index || !seen.insert(dependency_index) {
                continue;
            }
            adjacency[dependency_index].push(index);
            indegree[index] += 1;
        }
    }

    let mut queue = VecDeque::new();
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            queue.push_back(index);
        }
    }

    let mut ordered_indices = Vec::with_capacity(targets.len());
    while let Some(index) = queue.pop_front() {
        ordered_indices.push(index);
        for &dependent in &adjacency[index] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    if ordered_indices.len() != targets.len() {
        for index in 0..targets.len() {
            if !ordered_indices.contains(&index) {
                ordered_indices.push(index);
            }
        }
    }

    Ok(ordered_indices.into_iter().map(|index| targets[index].manifest_path.clone()).collect())
}

fn relink_workspace_bins_after_lifecycle(
    install_target_entries: &[(String, PathBuf)],
    config: &Npmrc,
    lockfile_dir: &Path,
    shamefully_hoist: bool,
    prefer_frozen_lockfile_override: Option<bool>,
    dependency_groups: &[DependencyGroup],
) -> miette::Result<()> {
    for (_, manifest_path) in install_target_entries {
        let manifest = PackageManifest::from_path(manifest_path.clone())
            .wrap_err_with(|| format!("reload workspace manifest: {}", manifest_path.display()))?;
        let project_dir = manifest
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        let target_config: &'static Npmrc = config_for_project(
            config,
            lockfile_dir,
            &project_dir,
            shamefully_hoist,
            prefer_frozen_lockfile_override,
        )
        .leak();
        link_bins_for_manifest(target_config, &manifest, dependency_groups.iter().copied())
            .wrap_err_with(|| format!("refresh bins for {}", manifest_path.display()))?;
    }

    Ok(())
}

fn to_lockfile_importer_id(workspace_root: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(workspace_root) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_package_manifest::DependencyGroup;
    use pretty_assertions::assert_eq;

    #[test]
    fn dependency_options_to_dependency_groups() {
        use DependencyGroup::{Dev, Optional, Prod};
        let create_list =
            |opts: InstallDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod + dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: false }),
            [Prod, Dev, Optional],
        );

        // --prod -> prod + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: false }),
            [Prod, Optional],
        );

        // --dev -> dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: false }),
            [Dev, Optional],
        );

        // --no-optional -> prod + dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: true }),
            [Prod, Dev],
        );

        // --prod --no-optional -> prod
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: true }),
            [Prod],
        );

        // --dev --no-optional -> dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: true }),
            [Dev],
        );

        // --prod --dev -> prod + dev + optional
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: false }),
            [Prod, Dev, Optional],
        );

        // --prod --dev --no-optional -> prod + dev
        assert_eq!(
            create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: true }),
            [Prod, Dev],
        );
    }

    #[test]
    fn should_parallelize_workspace_frozen_install_only_for_multi_target_frozen_runs() {
        assert!(should_parallelize_workspace_frozen_install(true, true, false, false));
        assert!(!should_parallelize_workspace_frozen_install(false, true, false, false));
        assert!(!should_parallelize_workspace_frozen_install(true, false, false, false));
        assert!(!should_parallelize_workspace_frozen_install(true, true, true, false));
        assert!(!should_parallelize_workspace_frozen_install(true, true, false, true));
    }
}
