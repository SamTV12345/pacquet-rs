use crate::State;
use crate::cli_args::install::{InstallArgs, InstallDependencyOptions};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{
    DependencyPath, DirectoryResolution, Lockfile, LockfileResolution, PackageSnapshot,
    PackageSnapshotDependency, ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencySpec,
    ResolvedDependencyVersion, RootProjectSnapshot, TarballResolution,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml::Value as YamlValue;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Target directory.
    target_dir: PathBuf,

    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    dependency_options: InstallDependencyOptions,

    /// Overwrite a non-empty deploy directory.
    #[arg(long)]
    force: bool,

    /// Select a workspace package by name or importer path.
    #[arg(long = "filter")]
    filter: Vec<String>,
}

#[derive(Debug, Clone)]
struct SelectedProject {
    importer_id: String,
    manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ImporterInfo {
    dir: PathBuf,
    canonical_dir: PathBuf,
    name: String,
    manifest_json: JsonValue,
    snapshot: ProjectSnapshot,
}

struct DeployFiles {
    manifest_json: JsonValue,
    lockfile: Lockfile,
}

impl DeployArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        let selected_project = select_source_project(&state, &self.filter)?;
        let source_dir = selected_project
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let current_dir =
            std::env::current_dir().into_diagnostic().wrap_err("get current directory")?;
        let target_dir = if self.target_dir.is_absolute() {
            self.target_dir
        } else {
            current_dir.join(self.target_dir)
        };

        let source_dir = fs::canonicalize(&source_dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("canonicalize {}", source_dir.display()))?;
        if target_dir.starts_with(&source_dir) {
            miette::bail!("Deploy target must not be inside the source package directory");
        }

        prepare_target_dir(&target_dir, self.force)?;
        copy_project(&source_dir, &target_dir)?;

        if state.lockfile.is_some() {
            let deploy_files = create_deploy_files(&state, &selected_project, &target_dir)?;
            write_deploy_files(&target_dir, deploy_files)?;
            return run_deploy_install(&state, &target_dir, self.dependency_options, true).await;
        }

        ensure_legacy_deploy_supported(&selected_project.manifest_path)?;
        run_deploy_install(&state, &target_dir, self.dependency_options, false).await
    }
}

async fn run_deploy_install(
    state: &State,
    target_dir: &Path,
    dependency_options: InstallDependencyOptions,
    frozen_lockfile: bool,
) -> miette::Result<()> {
    let mut target_config = state.config.clone();
    target_config.modules_dir = target_dir.join("node_modules");
    target_config.virtual_store_dir = target_config.modules_dir.join(".pnpm");
    let target_config = Box::leak(Box::new(target_config));
    let target_state = crate::State::init(target_dir.join("package.json"), target_config)
        .wrap_err("initialize deploy target state")?;

    InstallArgs {
        dependency_options,
        frozen_lockfile,
        prefer_frozen_lockfile: false,
        no_prefer_frozen_lockfile: true,
        fix_lockfile: false,
        ignore_scripts: false,
        lockfile_only: false,
        force: false,
        resolution_only: false,
        ignore_pnpmfile: false,
        pnpmfile: None,
        reporter: None,
        use_store_server: false,
        shamefully_hoist: false,
        filter: Vec::new(),
        recursive: false,
        prefer_offline: false,
        offline: false,
    }
    .run(target_state)
    .await
}

fn create_deploy_files(
    state: &State,
    selected_project: &SelectedProject,
    target_dir: &Path,
) -> miette::Result<DeployFiles> {
    let lockfile = state.lockfile.as_ref().ok_or_else(|| {
        miette::miette!("`pacquet deploy` requires pnpm-lock.yaml for workspace deployment")
    })?;
    let importers = collect_importers(lockfile, &state.lockfile_dir)?;
    let selected_importer = importers.get(&selected_project.importer_id).ok_or_else(|| {
        miette::miette!("Cannot find importer `{}` in pnpm-lock.yaml", selected_project.importer_id)
    })?;

    let mut packages = HashMap::<DependencyPath, PackageSnapshot>::new();
    if let Some(existing_packages) = lockfile.packages.as_ref() {
        for (dependency_path, snapshot) in existing_packages {
            packages.insert(
                rewrite_dependency_path_key(
                    dependency_path,
                    &state.lockfile_dir,
                    target_dir,
                    &selected_importer.canonical_dir,
                ),
                rewrite_package_snapshot(
                    snapshot,
                    &state.lockfile_dir,
                    target_dir,
                    &selected_importer.canonical_dir,
                ),
            );
        }
    }
    for (importer_id, importer) in &importers {
        if importer_id == &selected_project.importer_id {
            continue;
        }
        let package_name = importer.name.parse().map_err(|error| {
            miette::miette!("parse workspace package name `{}`: {error}", importer.name)
        })?;
        let reference = file_reference_to_target(target_dir, &importer.dir);
        packages.entry(DependencyPath::local_file(package_name, reference.clone())).or_insert_with(
            || {
                project_snapshot_to_package_snapshot(
                    &importer.snapshot,
                    &importer.dir,
                    &state.lockfile_dir,
                    target_dir,
                    &selected_importer.canonical_dir,
                )
            },
        );
    }

    let target_project_snapshot = rewrite_project_snapshot(
        &selected_importer.snapshot,
        &selected_importer.dir,
        &state.lockfile_dir,
        target_dir,
        &selected_importer.canonical_dir,
    );
    let patched_dependencies = rewrite_patched_dependencies(
        lockfile.patched_dependencies.as_ref(),
        &state.lockfile_dir,
        target_dir,
    );

    let mut deploy_lockfile = lockfile.clone();
    deploy_lockfile.catalogs = None;
    deploy_lockfile.overrides = None;
    deploy_lockfile.package_extensions_checksum = None;
    deploy_lockfile.pnpmfile_checksum = None;
    deploy_lockfile.time = None;
    deploy_lockfile.project_snapshot = RootProjectSnapshot::Single(target_project_snapshot.clone());
    deploy_lockfile.packages = (!packages.is_empty()).then_some(packages);
    deploy_lockfile.patched_dependencies = patched_dependencies.clone();

    let manifest_json = build_deploy_manifest(
        selected_importer.manifest_json.clone(),
        &target_project_snapshot,
        patched_dependencies.as_ref(),
    )?;

    Ok(DeployFiles { manifest_json, lockfile: deploy_lockfile })
}

fn collect_importers(
    lockfile: &Lockfile,
    lockfile_dir: &Path,
) -> miette::Result<HashMap<String, ImporterInfo>> {
    let importers = match &lockfile.project_snapshot {
        RootProjectSnapshot::Single(snapshot) => {
            HashMap::from([(".".to_string(), snapshot.clone())])
        }
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.clone(),
    };

    let mut result = HashMap::<String, ImporterInfo>::new();
    for (importer_id, snapshot) in importers {
        let manifest_path = if importer_id == "." {
            lockfile_dir.join("package.json")
        } else {
            lockfile_dir.join(&importer_id).join("package.json")
        };
        let manifest = PackageManifest::from_path(manifest_path.clone()).wrap_err_with(|| {
            format!("load package.json for workspace importer `{importer_id}`")
        })?;
        let dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        let canonical_dir = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        let name = manifest
            .value()
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                miette::miette!("workspace importer `{importer_id}` is missing package.json name")
            })?
            .to_string();
        result.insert(
            importer_id,
            ImporterInfo {
                dir,
                canonical_dir,
                name,
                manifest_json: manifest.value().clone(),
                snapshot,
            },
        );
    }
    Ok(result)
}

fn rewrite_project_snapshot(
    snapshot: &ProjectSnapshot,
    project_dir: &Path,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> ProjectSnapshot {
    let dependencies = rewrite_resolved_dependency_map(
        snapshot.dependencies.as_ref(),
        project_dir,
        lockfile_dir,
        target_dir,
        selected_project_dir,
    );
    let optional_dependencies = rewrite_resolved_dependency_map(
        snapshot.optional_dependencies.as_ref(),
        project_dir,
        lockfile_dir,
        target_dir,
        selected_project_dir,
    );
    let dev_dependencies = rewrite_resolved_dependency_map(
        snapshot.dev_dependencies.as_ref(),
        project_dir,
        lockfile_dir,
        target_dir,
        selected_project_dir,
    );
    let specifiers =
        collect_project_specifiers(&dependencies, &optional_dependencies, &dev_dependencies);

    ProjectSnapshot {
        specifiers: (!specifiers.is_empty()).then_some(specifiers),
        dependencies,
        optional_dependencies,
        dev_dependencies,
        dependencies_meta: snapshot.dependencies_meta.clone(),
        publish_directory: snapshot.publish_directory.clone(),
    }
}

fn rewrite_resolved_dependency_map(
    map: Option<&ResolvedDependencyMap>,
    project_dir: &Path,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> Option<ResolvedDependencyMap> {
    let map = map?;
    let rewritten = map
        .iter()
        .map(|(name, spec)| {
            let rewritten_spec = match &spec.version {
                ResolvedDependencyVersion::Link(reference) => {
                    let base_dir = local_reference_base_dir(reference, project_dir, lockfile_dir);
                    let rewritten_reference = rewrite_local_reference(
                        base_dir,
                        reference,
                        target_dir,
                        selected_project_dir,
                        true,
                    );
                    ResolvedDependencySpec {
                        specifier: rewritten_reference.clone(),
                        version: ResolvedDependencyVersion::Link(rewritten_reference),
                    }
                }
                _ => spec.clone(),
            };
            (name.clone(), rewritten_spec)
        })
        .collect::<ResolvedDependencyMap>();
    (!rewritten.is_empty()).then_some(rewritten)
}

fn collect_project_specifiers(
    dependencies: &Option<ResolvedDependencyMap>,
    optional_dependencies: &Option<ResolvedDependencyMap>,
    dev_dependencies: &Option<ResolvedDependencyMap>,
) -> HashMap<String, String> {
    dependencies
        .iter()
        .chain(optional_dependencies.iter())
        .chain(dev_dependencies.iter())
        .flat_map(|map| map.iter())
        .map(|(name, spec)| (name.to_string(), spec.specifier.clone()))
        .collect()
}

fn rewrite_dependency_path_key(
    dependency_path: &DependencyPath,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> DependencyPath {
    if let Some(reference) = dependency_path.local_file_reference() {
        return DependencyPath::local_file(
            dependency_path.package_name().clone(),
            rewrite_local_reference(
                lockfile_dir,
                reference,
                target_dir,
                selected_project_dir,
                true,
            ),
        );
    }
    dependency_path.clone()
}

fn rewrite_package_snapshot(
    snapshot: &PackageSnapshot,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> PackageSnapshot {
    let resolution = match &snapshot.resolution {
        LockfileResolution::Tarball(resolution) if resolution.tarball.starts_with("file:") => {
            LockfileResolution::Tarball(TarballResolution {
                tarball: rewrite_local_reference(
                    lockfile_dir,
                    &resolution.tarball,
                    target_dir,
                    selected_project_dir,
                    false,
                ),
                integrity: resolution.integrity.clone(),
            })
        }
        LockfileResolution::Directory(resolution) => {
            let source_dir = resolve_local_target(lockfile_dir, &resolution.directory);
            LockfileResolution::Directory(DirectoryResolution {
                directory: to_relative_path(target_dir, &source_dir),
            })
        }
        _ => snapshot.resolution.clone(),
    };

    let dependencies = snapshot.dependencies.as_ref().map(|dependencies| {
        dependencies
            .iter()
            .map(|(name, dependency)| {
                (
                    name.clone(),
                    rewrite_package_snapshot_dependency(
                        dependency,
                        lockfile_dir,
                        target_dir,
                        selected_project_dir,
                    ),
                )
            })
            .collect::<HashMap<_, _>>()
    });
    let optional_dependencies = snapshot.optional_dependencies.as_ref().map(|dependencies| {
        dependencies
            .iter()
            .map(|(name, value)| {
                let value = if value.starts_with("file:") || value.starts_with("link:") {
                    rewrite_local_reference(
                        lockfile_dir,
                        value,
                        target_dir,
                        selected_project_dir,
                        true,
                    )
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect::<HashMap<_, _>>()
    });

    PackageSnapshot {
        resolution,
        id: snapshot.id.clone(),
        name: snapshot.name.clone(),
        version: snapshot.version.clone(),
        engines: snapshot.engines.clone(),
        cpu: snapshot.cpu.clone(),
        os: snapshot.os.clone(),
        libc: snapshot.libc.clone(),
        deprecated: snapshot.deprecated.clone(),
        has_bin: snapshot.has_bin,
        prepare: snapshot.prepare,
        requires_build: snapshot.requires_build,
        bundled_dependencies: snapshot.bundled_dependencies.clone(),
        peer_dependencies: snapshot.peer_dependencies.clone(),
        peer_dependencies_meta: snapshot.peer_dependencies_meta.clone(),
        dependencies,
        optional_dependencies,
        transitive_peer_dependencies: snapshot.transitive_peer_dependencies.clone(),
        dev: snapshot.dev,
        optional: snapshot.optional,
    }
}

fn rewrite_package_snapshot_dependency(
    dependency: &PackageSnapshotDependency,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> PackageSnapshotDependency {
    match dependency {
        PackageSnapshotDependency::DependencyPath(path) => {
            PackageSnapshotDependency::DependencyPath(rewrite_dependency_path_key(
                path,
                lockfile_dir,
                target_dir,
                selected_project_dir,
            ))
        }
        PackageSnapshotDependency::Link(reference) => {
            PackageSnapshotDependency::Link(rewrite_local_reference(
                lockfile_dir,
                reference,
                target_dir,
                selected_project_dir,
                true,
            ))
        }
        _ => dependency.clone(),
    }
}

fn project_snapshot_to_package_snapshot(
    snapshot: &ProjectSnapshot,
    project_dir: &Path,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> PackageSnapshot {
    let dependencies = snapshot.dependencies.as_ref().map(|dependencies| {
        dependencies
            .iter()
            .map(|(name, spec)| {
                (
                    name.clone(),
                    resolved_dependency_to_snapshot_dependency(
                        &spec.version,
                        project_dir,
                        lockfile_dir,
                        target_dir,
                        selected_project_dir,
                    ),
                )
            })
            .collect::<HashMap<_, _>>()
    });
    let optional_dependencies = snapshot.optional_dependencies.as_ref().map(|dependencies| {
        dependencies
            .iter()
            .map(|(name, spec)| {
                let value = match &spec.version {
                    ResolvedDependencyVersion::Link(reference) => rewrite_local_reference(
                        local_reference_base_dir(reference, project_dir, lockfile_dir),
                        reference,
                        target_dir,
                        selected_project_dir,
                        true,
                    ),
                    _ => spec.version.to_string(),
                };
                (name.to_string(), value)
            })
            .collect::<HashMap<_, _>>()
    });

    PackageSnapshot {
        resolution: LockfileResolution::Directory(DirectoryResolution {
            directory: to_relative_path(target_dir, project_dir),
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
        optional_dependencies,
        transitive_peer_dependencies: None,
        dev: None,
        optional: None,
    }
}

fn resolved_dependency_to_snapshot_dependency(
    version: &ResolvedDependencyVersion,
    project_dir: &Path,
    lockfile_dir: &Path,
    target_dir: &Path,
    selected_project_dir: &Path,
) -> PackageSnapshotDependency {
    match version {
        ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
            PackageSnapshotDependency::PkgVerPeer(ver_peer.clone())
        }
        ResolvedDependencyVersion::PkgNameVerPeer(specifier) => {
            PackageSnapshotDependency::PkgNameVerPeer(specifier.clone())
        }
        ResolvedDependencyVersion::Link(reference) => {
            PackageSnapshotDependency::Link(rewrite_local_reference(
                local_reference_base_dir(reference, project_dir, lockfile_dir),
                reference,
                target_dir,
                selected_project_dir,
                true,
            ))
        }
    }
}

fn rewrite_patched_dependencies(
    patched_dependencies: Option<&HashMap<String, YamlValue>>,
    lockfile_dir: &Path,
    target_dir: &Path,
) -> Option<HashMap<String, YamlValue>> {
    let patched_dependencies = patched_dependencies?;
    let rewritten = patched_dependencies
        .iter()
        .filter_map(|(key, value)| {
            let path = match value {
                YamlValue::String(path) => path.clone(),
                YamlValue::Mapping(mapping) => mapping
                    .get(YamlValue::String("path".to_string()))
                    .and_then(YamlValue::as_str)
                    .map(ToString::to_string)?,
                _ => return None,
            };
            let absolute = resolve_local_target(lockfile_dir, &path);
            Some((key.clone(), YamlValue::String(to_relative_path(target_dir, &absolute))))
        })
        .collect::<HashMap<_, _>>();
    (!rewritten.is_empty()).then_some(rewritten)
}

fn build_deploy_manifest(
    mut manifest_json: JsonValue,
    snapshot: &ProjectSnapshot,
    patched_dependencies: Option<&HashMap<String, YamlValue>>,
) -> miette::Result<JsonValue> {
    set_dependency_field(&mut manifest_json, "dependencies", snapshot.dependencies.as_ref())?;
    set_dependency_field(
        &mut manifest_json,
        "optionalDependencies",
        snapshot.optional_dependencies.as_ref(),
    )?;
    set_dependency_field(
        &mut manifest_json,
        "devDependencies",
        snapshot.dev_dependencies.as_ref(),
    )?;

    let root = manifest_json
        .as_object_mut()
        .ok_or_else(|| miette::miette!("deploy manifest must be a JSON object"))?;
    let pnpm_value =
        root.entry("pnpm".to_string()).or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let pnpm = pnpm_value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;
    pnpm.remove("overrides");
    pnpm.remove("packageExtensions");

    match patched_dependencies_to_json(patched_dependencies) {
        Some(value) => {
            pnpm.insert("patchedDependencies".to_string(), JsonValue::Object(value));
        }
        None => {
            pnpm.remove("patchedDependencies");
        }
    }
    if pnpm.is_empty() {
        root.remove("pnpm");
    }

    Ok(manifest_json)
}

fn patched_dependencies_to_json(
    patched_dependencies: Option<&HashMap<String, YamlValue>>,
) -> Option<JsonMap<String, JsonValue>> {
    let patched_dependencies = patched_dependencies?;
    let mapped = patched_dependencies
        .iter()
        .filter_map(|(key, value)| {
            value.as_str().map(|value| (key.clone(), JsonValue::String(value.to_string())))
        })
        .collect::<JsonMap<_, _>>();
    (!mapped.is_empty()).then_some(mapped)
}

fn set_dependency_field(
    manifest_json: &mut JsonValue,
    field: &str,
    dependencies: Option<&ResolvedDependencyMap>,
) -> miette::Result<()> {
    let root = manifest_json
        .as_object_mut()
        .ok_or_else(|| miette::miette!("deploy manifest must be a JSON object"))?;
    let Some(dependencies) = dependencies else {
        root.remove(field);
        return Ok(());
    };
    let mapped = dependencies
        .iter()
        .map(|(name, spec)| (name.to_string(), JsonValue::String(spec.specifier.clone())))
        .collect::<JsonMap<_, _>>();
    if mapped.is_empty() {
        root.remove(field);
    } else {
        root.insert(field.to_string(), JsonValue::Object(mapped));
    }
    Ok(())
}

fn write_deploy_files(target_dir: &Path, deploy_files: DeployFiles) -> miette::Result<()> {
    let manifest_text = serde_json::to_string_pretty(&deploy_files.manifest_json)
        .into_diagnostic()
        .wrap_err("serialize deploy package.json")?;
    fs::write(target_dir.join("package.json"), format!("{manifest_text}\n"))
        .into_diagnostic()
        .wrap_err("write deploy package.json")?;
    deploy_files.lockfile.save_to_dir(target_dir).wrap_err("write deploy pnpm-lock.yaml")?;
    Ok(())
}

fn ensure_legacy_deploy_supported(manifest_path: &Path) -> miette::Result<()> {
    let manifest = PackageManifest::from_path(manifest_path.to_path_buf())
        .wrap_err("load package.json for deploy")?;
    if manifest
        .dependencies([DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional])
        .any(|(_, spec)| spec.starts_with("workspace:"))
    {
        miette::bail!(
            "`pacquet deploy` requires pnpm-lock.yaml when the selected project has workspace: dependencies"
        );
    }
    Ok(())
}

fn select_source_project(state: &State, filters: &[String]) -> miette::Result<SelectedProject> {
    if !state.lockfile_dir.join("pnpm-workspace.yaml").is_file() {
        miette::bail!("`pacquet deploy` is only possible from inside a workspace");
    }

    if filters.is_empty() {
        if state.lockfile_importer_id != "." {
            return Ok(SelectedProject {
                importer_id: state.lockfile_importer_id.clone(),
                manifest_path: state.manifest.path().to_path_buf(),
            });
        }
        miette::bail!(
            "No workspace project was selected for deployment. Run the command from a workspace package or pass exactly one --filter selector."
        );
    }
    let mut matches = Vec::<SelectedProject>::new();
    for selector in filters {
        if let Some(project) = match_selector(state, selector)? {
            matches.push(project);
        }
    }
    matches.sort_by(|left, right| left.importer_id.cmp(&right.importer_id));
    matches.dedup_by(|left, right| left.importer_id == right.importer_id);
    if matches.is_empty() {
        miette::bail!("No workspace project matched --filter selector(s): {}", filters.join(", "));
    }
    if matches.len() > 1 {
        miette::bail!("Cannot deploy more than 1 project");
    }
    Ok(matches.remove(0))
}

fn match_selector(state: &State, selector: &str) -> miette::Result<Option<SelectedProject>> {
    let selector = selector.trim_start_matches("./").replace('\\', "/");
    if selector.is_empty() {
        return Ok(None);
    }
    if selector == "." || selector == state.lockfile_importer_id {
        return Ok(Some(SelectedProject {
            importer_id: state.lockfile_importer_id.clone(),
            manifest_path: state.manifest.path().to_path_buf(),
        }));
    }

    let root_name = state
        .manifest
        .value()
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    if state.lockfile_importer_id == "."
        && root_name.as_deref().is_some_and(|name| name == selector)
    {
        return Ok(Some(SelectedProject {
            importer_id: ".".to_string(),
            manifest_path: state.manifest.path().to_path_buf(),
        }));
    }

    for (name, info) in &state.workspace_packages {
        let importer_id = to_importer_id(&state.lockfile_dir, &info.root_dir);
        if selector == *name || selector == importer_id {
            return Ok(Some(SelectedProject {
                importer_id,
                manifest_path: info.root_dir.join("package.json"),
            }));
        }
    }
    Ok(None)
}

fn prepare_target_dir(target_dir: &Path, force: bool) -> miette::Result<()> {
    if target_dir.exists() {
        let is_empty = target_dir
            .read_dir()
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", target_dir.display()))?
            .next()
            .is_none();
        if !is_empty {
            if !force {
                miette::bail!("Deploy path {} is not empty", target_dir.display());
            }
            fs::remove_dir_all(target_dir)
                .into_diagnostic()
                .wrap_err_with(|| format!("remove {}", target_dir.display()))?;
        }
    }
    fs::create_dir_all(target_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", target_dir.display()))
}

fn copy_project(source_dir: &Path, target_dir: &Path) -> miette::Result<()> {
    fn walk(source_dir: &Path, target_dir: &Path, dir: &Path) -> miette::Result<()> {
        for entry in fs::read_dir(dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", dir.display()))?
        {
            let entry = entry.into_diagnostic().wrap_err("read deploy entry")?;
            let path = entry.path();
            let file_type =
                entry.file_type().into_diagnostic().wrap_err("read deploy file type")?;
            let file_name = entry.file_name().to_string_lossy().into_owned();

            if matches!(file_name.as_str(), "node_modules" | ".git" | "target") {
                continue;
            }

            let relative = path.strip_prefix(source_dir).unwrap_or(&path);
            let destination = target_dir.join(relative);
            if file_type.is_dir() {
                fs::create_dir_all(&destination)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("create {}", destination.display()))?;
                walk(source_dir, target_dir, &path)?;
            } else if file_type.is_file() {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)
                        .into_diagnostic()
                        .wrap_err_with(|| format!("create {}", parent.display()))?;
                }
                fs::copy(&path, &destination).into_diagnostic().wrap_err_with(|| {
                    format!("copy {} to {}", path.display(), destination.display())
                })?;
            }
        }
        Ok(())
    }

    walk(source_dir, target_dir, source_dir)
}

fn local_reference_base_dir<'a>(
    reference: &str,
    project_dir: &'a Path,
    lockfile_dir: &'a Path,
) -> &'a Path {
    if reference.starts_with("link:") { project_dir } else { lockfile_dir }
}

fn rewrite_local_reference(
    base_dir: &Path,
    reference: &str,
    target_dir: &Path,
    selected_project_dir: &Path,
    use_file_protocol: bool,
) -> String {
    let Some((protocol, path_part, suffix)) = split_local_reference(reference) else {
        return reference.to_string();
    };
    let resolved_path = resolve_local_target(base_dir, path_part);
    let canonical_resolved = fs::canonicalize(&resolved_path).unwrap_or(resolved_path.clone());
    if canonical_resolved == *selected_project_dir {
        return format!("link:.{suffix}");
    }
    let protocol = if use_file_protocol { "file:" } else { protocol };
    format!("{protocol}{}{suffix}", to_relative_path(target_dir, &canonical_resolved))
}

fn split_local_reference(reference: &str) -> Option<(&str, &str, &str)> {
    let (protocol, remainder) = if let Some(value) = reference.strip_prefix("link:") {
        ("link:", value)
    } else if let Some(value) = reference.strip_prefix("file:") {
        ("file:", value)
    } else {
        return None;
    };
    let suffix_index = remainder.find('(').unwrap_or(remainder.len());
    Some((protocol, &remainder[..suffix_index], &remainder[suffix_index..]))
}

fn resolve_local_target(base_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { base_dir.join(path) }
}

fn file_reference_to_target(target_dir: &Path, source_dir: &Path) -> String {
    format!("file:{}", to_relative_path(target_dir, source_dir))
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
        return to.to_string_lossy().replace('\\', "/");
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

fn to_importer_id(workspace_root: &Path, project_dir: &Path) -> String {
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
