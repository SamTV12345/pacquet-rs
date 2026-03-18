use crate::State;
use crate::cli_args::install::run_install_lifecycle_scripts;
use clap::Args;
use glob::Pattern;
use miette::Context;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub struct RebuildArgs {
    /// Optional package names or globs to rebuild.
    packages: Vec<String>,

    /// Rebuild every workspace project recursively (including the workspace root).
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    filter: Vec<String>,

    /// Rebuild packages that were skipped during install with --ignore-scripts.
    #[arg(long)]
    pending: bool,
}

impl RebuildArgs {
    pub fn from_packages(packages: Vec<String>) -> Self {
        Self { packages, recursive: false, filter: Vec::new(), pending: false }
    }

    pub async fn run(self, state: State) -> miette::Result<()> {
        let State {
            config, manifest, lockfile_dir, lockfile_importer_id, workspace_packages, ..
        } = state;
        let selectors = self
            .packages
            .iter()
            .map(|value| {
                Pattern::new(value).map_err(|error| {
                    miette::miette!("invalid rebuild package selector `{value}`: {error}")
                })
            })
            .collect::<miette::Result<Vec<_>>>()?;
        let _ = self.pending;

        let targets = select_rebuild_targets(
            &manifest,
            &lockfile_dir,
            &lockfile_importer_id,
            &workspace_packages,
            self.recursive,
            &self.filter,
        )?;

        for manifest_path in targets.into_values() {
            let project_dir = manifest_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| lockfile_dir.to_path_buf());
            let target_config = config_for_project(config, &project_dir).leak();

            if selectors.is_empty() {
                run_install_lifecycle_scripts(manifest_path.clone(), target_config)?;
                continue;
            }

            let target_manifest = PackageManifest::from_path(manifest_path.clone())
                .wrap_err("load package.json for rebuild")?;
            for dependency_name in target_manifest
                .dependencies([
                    DependencyGroup::Prod,
                    DependencyGroup::Dev,
                    DependencyGroup::Optional,
                    DependencyGroup::Peer,
                ])
                .map(|(name, _)| name.to_string())
            {
                if !selectors.iter().any(|pattern| pattern.matches(&dependency_name)) {
                    continue;
                }
                let installed_manifest_path =
                    installed_package_manifest_path(&project_dir, &dependency_name);
                if installed_manifest_path.is_file() {
                    run_install_lifecycle_scripts(installed_manifest_path, target_config)?;
                }
            }
        }

        Ok(())
    }
}

fn installed_package_manifest_path(project_dir: &Path, dependency_name: &str) -> PathBuf {
    let mut path = project_dir.join("node_modules");
    for segment in dependency_name.split('/') {
        path.push(segment);
    }
    path.join("package.json")
}

fn select_rebuild_targets(
    manifest: &PackageManifest,
    lockfile_dir: &Path,
    lockfile_importer_id: &str,
    workspace_packages: &pacquet_package_manager::WorkspacePackages,
    recursive: bool,
    filters: &[String],
) -> miette::Result<BTreeMap<String, PathBuf>> {
    if !recursive && filters.is_empty() {
        return Ok(BTreeMap::from([(
            lockfile_importer_id.to_string(),
            manifest.path().to_path_buf(),
        )]));
    }

    let mut targets = BTreeMap::<String, PathBuf>::new();
    if lockfile_dir.join("pnpm-workspace.yaml").is_file() {
        let root_manifest = lockfile_dir.join("package.json");
        if root_manifest.is_file()
            && (recursive
                || matches_filter(".", &root_manifest, filters, workspace_packages, lockfile_dir))
        {
            targets.insert(".".to_string(), root_manifest);
        }
    }
    for (name, info) in workspace_packages.iter() {
        let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
        if recursive
            || filters.iter().any(|selector| selector_matches(selector, &importer_id, name))
        {
            targets.insert(importer_id, info.root_dir.join("package.json"));
        }
    }

    if targets.is_empty() {
        if recursive {
            miette::bail!(
                "No workspace projects found for --recursive. Ensure pnpm-workspace.yaml includes package patterns."
            );
        }
        miette::bail!("No workspace projects matched --filter selectors: {}", filters.join(", "));
    }

    Ok(targets)
}

fn matches_filter(
    importer_id: &str,
    manifest_path: &Path,
    filters: &[String],
    workspace_packages: &pacquet_package_manager::WorkspacePackages,
    lockfile_dir: &Path,
) -> bool {
    filters.iter().any(|selector| {
        let normalized = selector.trim_start_matches("./").replace('\\', "/");
        normalized == importer_id
            || (importer_id == "."
                && root_package_name(manifest_path).is_some_and(|name| normalized == name))
            || workspace_packages.iter().any(|(name, info)| {
                to_lockfile_importer_id(lockfile_dir, &info.root_dir) == importer_id
                    && normalized == *name
            })
    })
}

fn selector_matches(selector: &str, importer_id: &str, package_name: &str) -> bool {
    let normalized = selector.trim_start_matches("./").replace('\\', "/");
    normalized == importer_id || normalized == package_name
}

fn config_for_project(config: &Npmrc, project_dir: &Path) -> Npmrc {
    let mut next = config.clone();
    next.modules_dir = project_dir.join("node_modules");
    next.virtual_store_dir = next.modules_dir.join(".pnpm");
    next
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

fn root_package_name(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    value.get("name").and_then(serde_json::Value::as_str).map(ToString::to_string)
}
