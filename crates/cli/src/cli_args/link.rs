use crate::State;
use crate::cli_args::bin::global_bin_dir;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::{
    Install, InstallReporter, link_bins_from_package_manifest, link_package,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use serde_json::{Map, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Directories to link, or globally linked package names.
    packages: Vec<String>,

    /// Save the linked dependency to devDependencies.
    #[arg(short = 'D', long)]
    save_dev: bool,

    /// Save the linked dependency to optionalDependencies.
    #[arg(short = 'O', long)]
    save_optional: bool,
}

impl LinkArgs {
    pub async fn run(self, dir: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        if self.packages.is_empty() {
            return register_current_package_globally(&dir, npmrc);
        }

        let target_group = self.target_group()?;
        let project_manifest_path = dir.join("package.json");
        for package in &self.packages {
            let linked_dep_dir = if is_path_like(package) {
                resolve_linked_dir(&dir, package)
            } else {
                global_package_root()?.join("node_modules").join(package)
            };
            if !linked_dep_dir.exists() {
                miette::bail!("linked package path not found: {}", linked_dep_dir.display());
            }

            let linked_manifest = PackageManifest::from_path(linked_dep_dir.join("package.json"))
                .wrap_err_with(|| {
                format!("load linked package manifest: {}", linked_dep_dir.display())
            })?;
            maybe_warn_about_peer_dependencies(&dir, &linked_manifest);
            let package_name = linked_package_name(&linked_manifest, &linked_dep_dir)?;
            let link_spec = link_spec_from(&dir, &linked_dep_dir)?;
            set_link_dependency_in_manifest(
                &project_manifest_path,
                &package_name,
                &link_spec,
                target_group,
            )?;
            write_link_override(&dir, &linked_dep_dir, &package_name, &link_spec)?;
        }

        install_linked_project(project_manifest_path, npmrc).await
    }

    fn target_group(&self) -> miette::Result<DependencyGroup> {
        if self.save_dev && self.save_optional {
            miette::bail!("`pacquet link` accepts only one of `--save-dev` or `--save-optional`");
        }
        if self.save_dev {
            return Ok(DependencyGroup::Dev);
        }
        if self.save_optional {
            return Ok(DependencyGroup::Optional);
        }
        Ok(DependencyGroup::Prod)
    }
}

fn register_current_package_globally(dir: &Path, npmrc: &'static Npmrc) -> miette::Result<()> {
    let manifest = PackageManifest::from_path(dir.join("package.json")).wrap_err_with(|| {
        format!("load package manifest: {}", dir.join("package.json").display())
    })?;
    let package_name = linked_package_name(&manifest, dir)?;
    let global_root = global_package_root()?;
    let global_node_modules = global_root.join("node_modules");
    let global_bin = global_bin_dir()?;
    fs::create_dir_all(&global_node_modules)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", global_node_modules.display()))?;
    fs::create_dir_all(&global_bin)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", global_bin.display()))?;
    link_package(true, dir, &global_node_modules.join(&package_name))
        .map_err(|error| miette::miette!("link global package: {error}"))?;
    link_bins_from_package_manifest(npmrc, &manifest, dir, &global_bin)?;
    Ok(())
}

async fn install_linked_project(
    project_manifest_path: PathBuf,
    npmrc: &'static Npmrc,
) -> miette::Result<()> {
    let state = State::init(project_manifest_path, npmrc).wrap_err("initialize the state")?;
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

    Install {
        tarball_mem_cache,
        resolved_packages,
        http_client,
        config,
        manifest,
        lockfile: lockfile.as_ref(),
        lockfile_dir,
        lockfile_importer_id,
        workspace_packages,
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
        frozen_lockfile: false,
        lockfile_only: false,
        force: false,
        prefer_offline: false,
        offline: false,
        pnpmfile: None,
        ignore_pnpmfile: false,
        reporter_prefix: None,
        reporter: InstallReporter::Default,
        print_summary: true,
    }
    .run()
    .await
    .map(|_| ())
}

fn linked_package_name(
    manifest: &PackageManifest,
    linked_dep_dir: &Path,
) -> miette::Result<String> {
    manifest
        .value()
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            linked_dep_dir.file_name().and_then(|name| name.to_str()).map(ToOwned::to_owned)
        })
        .ok_or_else(|| {
            miette::miette!("linked package is missing a name: {}", linked_dep_dir.display())
        })
}

fn maybe_warn_about_peer_dependencies(project_dir: &Path, manifest: &PackageManifest) {
    let Some(peer_dependencies) =
        manifest.value().get("peerDependencies").and_then(Value::as_object)
    else {
        return;
    };
    if peer_dependencies.is_empty() {
        return;
    }
    let package_name = manifest.value().get("name").and_then(Value::as_str).unwrap_or("<unknown>");
    let peer_lines = peer_dependencies
        .iter()
        .map(|(name, value)| format!("  - {name}@{}", value.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!(
        "WARN The package {package_name}, which you have just pacquet linked, has the following peerDependencies specified in its package.json:\n\n{peer_lines}\n\nThe linked dependency will not resolve peer dependencies from the target node_modules.\nThis might cause issues in your project. To resolve this, you may use the `file:` protocol instead.\nLocation: {}",
        project_dir.display()
    );
}

fn write_link_override(
    project_dir: &Path,
    linked_dep_dir: &Path,
    package_name: &str,
    link_spec: &str,
) -> miette::Result<()> {
    if let Some(workspace_root) = find_workspace_root(project_dir) {
        let workspace_path = workspace_root.join("pnpm-workspace.yaml");
        let workspace_link_spec = link_spec_from(&workspace_root, linked_dep_dir)?;
        return write_workspace_override(&workspace_path, package_name, &workspace_link_spec);
    }
    write_package_json_override(&project_dir.join("package.json"), package_name, link_spec)
}

fn write_workspace_override(
    workspace_manifest_path: &Path,
    package_name: &str,
    link_spec: &str,
) -> miette::Result<()> {
    let content = fs::read_to_string(workspace_manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", workspace_manifest_path.display()))?;
    let mut value = serde_yaml::from_str::<serde_yaml::Value>(&content)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse {}", workspace_manifest_path.display()))?;
    let root = ensure_yaml_mapping(&mut value)?;
    let overrides = root
        .entry(serde_yaml::Value::String("overrides".to_string()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let overrides = ensure_yaml_mapping_value(overrides)?;
    overrides.insert(
        serde_yaml::Value::String(package_name.to_string()),
        serde_yaml::Value::String(link_spec.to_string()),
    );
    let rendered = serde_yaml::to_string(&value)
        .into_diagnostic()
        .wrap_err("serialize pnpm-workspace.yaml")?;
    fs::write(workspace_manifest_path, rendered)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", workspace_manifest_path.display()))
}

fn write_package_json_override(
    package_json_path: &Path,
    package_name: &str,
    link_spec: &str,
) -> miette::Result<()> {
    let mut value = read_json_object(package_json_path)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    let pnpm = root.entry("pnpm").or_insert_with(|| Value::Object(Map::new()));
    let pnpm = pnpm
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;
    let overrides = pnpm.entry("overrides").or_insert_with(|| Value::Object(Map::new()));
    let overrides = overrides
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm.overrides must be an object"))?;
    overrides.insert(package_name.to_string(), Value::String(link_spec.to_string()));
    write_json_object(package_json_path, &value)
}

fn set_link_dependency_in_manifest(
    package_json_path: &Path,
    package_name: &str,
    link_spec: &str,
    target_group: DependencyGroup,
) -> miette::Result<()> {
    let mut value = read_json_object(package_json_path)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    for field in dependency_field_names() {
        if let Some(dependencies) = root.get_mut(field).and_then(Value::as_object_mut)
            && dependencies.contains_key(package_name)
        {
            dependencies.insert(package_name.to_string(), Value::String(link_spec.to_string()));
            return write_json_object(package_json_path, &value);
        }
    }
    let field_name = dependency_group_name(target_group);
    let dependencies =
        root.entry(field_name.to_string()).or_insert_with(|| Value::Object(Map::new()));
    let dependencies = dependencies
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json {field_name} field must be an object"))?;
    dependencies.insert(package_name.to_string(), Value::String(link_spec.to_string()));
    write_json_object(package_json_path, &value)
}

fn dependency_field_names() -> [&'static str; 4] {
    [
        dependency_group_name(DependencyGroup::Prod),
        dependency_group_name(DependencyGroup::Dev),
        dependency_group_name(DependencyGroup::Optional),
        dependency_group_name(DependencyGroup::Peer),
    ]
}

fn dependency_group_name(group: DependencyGroup) -> &'static str {
    match group {
        DependencyGroup::Prod => "dependencies",
        DependencyGroup::Dev => "devDependencies",
        DependencyGroup::Optional => "optionalDependencies",
        DependencyGroup::Peer => "peerDependencies",
    }
}

fn read_json_object(path: &Path) -> miette::Result<Value> {
    fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))
        .and_then(|content| {
            serde_json::from_str(&content)
                .into_diagnostic()
                .wrap_err_with(|| format!("parse {}", path.display()))
        })
}

fn write_json_object(path: &Path, value: &Value) -> miette::Result<()> {
    let content =
        serde_json::to_string_pretty(value).into_diagnostic().wrap_err("serialize package.json")?;
    fs::write(path, format!("{content}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", path.display()))
}

fn ensure_yaml_mapping(value: &mut serde_yaml::Value) -> miette::Result<&mut serde_yaml::Mapping> {
    if !matches!(value, serde_yaml::Value::Mapping(_)) {
        *value = serde_yaml::Value::Mapping(Default::default());
    }
    ensure_yaml_mapping_value(value)
}

fn ensure_yaml_mapping_value(
    value: &mut serde_yaml::Value,
) -> miette::Result<&mut serde_yaml::Mapping> {
    value.as_mapping_mut().ok_or_else(|| miette::miette!("expected YAML mapping"))
}

fn link_spec_from(project_dir: &Path, linked_dep_dir: &Path) -> miette::Result<String> {
    let project_dir = fs::canonicalize(project_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("canonicalize {}", project_dir.display()))?;
    let linked_dep_dir = fs::canonicalize(linked_dep_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("canonicalize {}", linked_dep_dir.display()))?;
    let relative = diff_paths(&linked_dep_dir, &project_dir)
        .ok_or_else(|| miette::miette!("compute relative path to {}", linked_dep_dir.display()))?;
    let normalized = relative.to_string_lossy().replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized).to_string();
    Ok(format!("link:{normalized}"))
}

fn diff_paths(path: &Path, base: &Path) -> Option<PathBuf> {
    let path = path.components().collect::<Vec<_>>();
    let base = base.components().collect::<Vec<_>>();
    let common = path.iter().zip(base.iter()).take_while(|(left, right)| left == right).count();
    if common == 0 && path.first()? != base.first()? {
        return None;
    }

    let mut result = PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for component in path.iter().skip(common) {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Some(result)
}

fn resolve_linked_dir(project_dir: &Path, package: &str) -> PathBuf {
    expand_home(package).unwrap_or_else(|| {
        let path = Path::new(package);
        if path.is_absolute() { path.to_path_buf() } else { project_dir.join(path) }
    })
}

fn expand_home(path: &str) -> Option<PathBuf> {
    let rest = path.strip_prefix("~/")?;
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(home::home_dir)?;
    Some(home.join(rest))
}

fn is_path_like(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value.contains(std::path::MAIN_SEPARATOR)
        || value.contains('/')
        || value.contains('\\')
}

fn global_package_root() -> miette::Result<PathBuf> {
    Ok(global_bin_dir()?.join("global"))
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
