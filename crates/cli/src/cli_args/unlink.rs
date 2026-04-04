use crate::State;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::Lockfile;
use pacquet_package_manager::{Install, InstallReporter, current_lockfile_for_installers};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Package names to unlink. When omitted, all linked packages are unlinked.
    packages: Vec<String>,

    /// Unlink in every workspace package.
    #[arg(short = 'r', long)]
    recursive: bool,
}

impl UnlinkArgs {
    pub async fn run(self, mut state: State) -> miette::Result<()> {
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
        } = &mut state;

        let install_ctx = UnlinkInstallContext {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            lockfile_dir,
            workspace_packages,
        };

        if self.recursive {
            return unlink_recursive(self.packages, lockfile, install_ctx).await;
        }

        let changed = unlink_in_project(
            manifest.path(),
            &self.packages,
            find_workspace_root(lockfile_dir.as_path()),
        )?;
        if !changed {
            println!("Nothing to unlink");
            return Ok(());
        }

        reinstall_project(
            install_ctx,
            manifest.path().to_path_buf(),
            lockfile.as_ref(),
            lockfile_importer_id,
        )
        .await
    }
}

#[derive(Clone, Copy)]
struct UnlinkInstallContext<'a> {
    tarball_mem_cache: &'a pacquet_tarball::MemCache,
    resolved_packages: &'a pacquet_package_manager::ResolvedPackages,
    http_client: &'a pacquet_network::ThrottledClient,
    config: &'static pacquet_npmrc::Npmrc,
    lockfile_dir: &'a Path,
    workspace_packages: &'a pacquet_package_manager::WorkspacePackages,
}

async fn unlink_recursive(
    packages: Vec<String>,
    lockfile: &mut Option<Lockfile>,
    install_ctx: UnlinkInstallContext<'_>,
) -> miette::Result<()> {
    let UnlinkInstallContext {
        tarball_mem_cache,
        resolved_packages,
        http_client,
        config,
        lockfile_dir,
        workspace_packages,
    } = install_ctx;
    let workspace_root = find_workspace_root(lockfile_dir);
    let mut targets = BTreeMap::<String, PathBuf>::new();
    let root_manifest = lockfile_dir.join("package.json");
    if root_manifest.is_file() {
        targets.insert(".".to_string(), root_manifest);
    }
    for info in workspace_packages.values() {
        let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
        targets.insert(importer_id, info.root_dir.join("package.json"));
    }

    let mut changed_importers = HashSet::<String>::new();
    let mut current_lockfile = lockfile.clone();
    for (importer_id, manifest_path) in targets {
        let changed = unlink_in_project(&manifest_path, &packages, workspace_root.clone())?;
        if !changed {
            continue;
        }
        changed_importers.insert(importer_id.clone());
        let manifest = PackageManifest::from_path(manifest_path.clone())
            .wrap_err_with(|| format!("reload {}", manifest_path.display()))?;
        let project_dir = manifest
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| lockfile_dir.to_path_buf());
        let mut target_config = config.clone();
        target_config.modules_dir = project_dir.join("node_modules");
        target_config.virtual_store_dir = target_config.modules_dir.join(".pnpm");
        let target_config = Box::leak(Box::new(target_config));

        Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config: target_config,
            manifest: &manifest,
            lockfile: current_lockfile.as_ref(),
            lockfile_dir,
            lockfile_importer_id: &importer_id,
            workspace_packages,
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
            reporter: InstallReporter::Default,
            print_summary: true,
            manage_progress_reporter: true,
            additional_importers: Vec::new(),
        }
        .run()
        .await
        .wrap_err("reinstall after unlink")?;

        current_lockfile =
            Lockfile::load_from_dir(lockfile_dir).wrap_err("reload lockfile after unlink")?;
    }

    if changed_importers.is_empty() {
        println!("Nothing to unlink");
        return Ok(());
    }

    if config.lockfile
        && let Some(lockfile) = current_lockfile.as_ref()
    {
        let current_lockfile = current_lockfile_for_installers(
            lockfile,
            &changed_importers,
            &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
            &HashSet::new(),
        );
        current_lockfile
            .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
            .wrap_err("write node_modules/.pnpm/lock.yaml after unlink")?;
    }
    *lockfile = current_lockfile;

    Ok(())
}

async fn reinstall_project(
    install_ctx: UnlinkInstallContext<'_>,
    manifest_path: PathBuf,
    lockfile: Option<&Lockfile>,
    lockfile_importer_id: &str,
) -> miette::Result<()> {
    let UnlinkInstallContext {
        tarball_mem_cache,
        resolved_packages,
        http_client,
        config,
        lockfile_dir,
        workspace_packages,
    } = install_ctx;
    let manifest =
        PackageManifest::from_path(manifest_path).wrap_err("reload package.json after unlink")?;
    Install {
        tarball_mem_cache,
        resolved_packages,
        http_client,
        config,
        manifest: &manifest,
        lockfile,
        lockfile_dir,
        lockfile_importer_id,
        workspace_packages,
        preferred_versions: None,
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
        manage_progress_reporter: true,
        additional_importers: Vec::new(),
    }
    .run()
    .await
    .map(|_| ())
}

fn unlink_in_project(
    package_json_path: &Path,
    packages: &[String],
    workspace_root: Option<PathBuf>,
) -> miette::Result<bool> {
    let mut changed = false;
    let mut manifest = read_json_value(package_json_path)?;
    changed |= remove_link_specs_from_manifest(&mut manifest, packages)?;
    write_json_value(package_json_path, &manifest)?;

    if let Some(workspace_root) = workspace_root {
        let workspace_path = workspace_root.join("pnpm-workspace.yaml");
        if workspace_path.exists() {
            changed |= remove_link_overrides_from_workspace(&workspace_path, packages)?;
        }
    } else {
        changed |= remove_link_overrides_from_package_json(package_json_path, packages)?;
    }

    Ok(changed)
}

fn remove_link_specs_from_manifest(
    manifest: &mut serde_json::Value,
    packages: &[String],
) -> miette::Result<bool> {
    let Some(root) = manifest.as_object_mut() else {
        miette::bail!("package.json root must be an object");
    };
    let mut changed = false;
    for field in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
        let mut remove_field = false;
        if let Some(dependencies) = root.get_mut(field).and_then(serde_json::Value::as_object_mut) {
            let names = dependencies.keys().cloned().collect::<Vec<_>>();
            for name in names {
                let should_match =
                    packages.is_empty() || packages.iter().any(|package| package == &name);
                if !should_match {
                    continue;
                }
                if dependencies
                    .get(&name)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value.starts_with("link:"))
                {
                    dependencies.remove(&name);
                    changed = true;
                }
            }
            remove_field = dependencies.is_empty();
        }
        if remove_field {
            root.remove(field);
        }
    }
    Ok(changed)
}

fn remove_link_overrides_from_package_json(
    package_json_path: &Path,
    packages: &[String],
) -> miette::Result<bool> {
    let mut value = read_json_value(package_json_path)?;
    let Some(root) = value.as_object_mut() else {
        miette::bail!("package.json root must be an object");
    };
    let Some(pnpm) = root.get_mut("pnpm").and_then(serde_json::Value::as_object_mut) else {
        return Ok(false);
    };
    let Some(overrides) = pnpm.get_mut("overrides").and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(false);
    };

    let changed = remove_link_override_entries(overrides, packages);
    if overrides.is_empty() {
        pnpm.remove("overrides");
    }
    if pnpm.is_empty() {
        root.remove("pnpm");
    }
    if changed {
        write_json_value(package_json_path, &value)?;
    }
    Ok(changed)
}

fn remove_link_overrides_from_workspace(
    workspace_path: &Path,
    packages: &[String],
) -> miette::Result<bool> {
    let content = fs::read_to_string(workspace_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", workspace_path.display()))?;
    let mut value = serde_yaml::from_str::<serde_yaml::Value>(&content)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse {}", workspace_path.display()))?;
    let Some(root) = value.as_mapping_mut() else {
        return Ok(false);
    };
    let key = serde_yaml::Value::String("overrides".to_string());
    let Some(overrides_value) = root.get_mut(&key) else {
        return Ok(false);
    };
    let Some(overrides) = overrides_value.as_mapping_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    let keys = overrides.keys().cloned().collect::<Vec<_>>();
    for override_key in keys {
        let Some(name) = override_key.as_str() else {
            continue;
        };
        let should_match = packages.is_empty() || packages.iter().any(|package| package == name);
        if !should_match {
            continue;
        }
        if overrides
            .get(&override_key)
            .and_then(serde_yaml::Value::as_str)
            .is_some_and(|value| value.starts_with("link:"))
        {
            overrides.remove(&override_key);
            changed = true;
        }
    }
    if overrides.is_empty() {
        root.remove(&key);
    }
    if changed {
        let rendered = serde_yaml::to_string(&value)
            .into_diagnostic()
            .wrap_err("serialize pnpm-workspace.yaml")?;
        fs::write(workspace_path, rendered)
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", workspace_path.display()))?;
    }
    Ok(changed)
}

fn remove_link_override_entries(
    overrides: &mut serde_json::Map<String, serde_json::Value>,
    packages: &[String],
) -> bool {
    let mut changed = false;
    let names = overrides.keys().cloned().collect::<Vec<_>>();
    for name in names {
        let should_match = packages.is_empty() || packages.iter().any(|package| package == &name);
        if !should_match {
            continue;
        }
        if overrides
            .get(&name)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.starts_with("link:"))
        {
            overrides.remove(&name);
            changed = true;
        }
    }
    changed
}

fn read_json_value(path: &Path) -> miette::Result<serde_json::Value> {
    fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))
        .and_then(|content| {
            serde_json::from_str(&content)
                .into_diagnostic()
                .wrap_err_with(|| format!("parse {}", path.display()))
        })
}

fn write_json_value(path: &Path, value: &serde_json::Value) -> miette::Result<()> {
    let content =
        serde_json::to_string_pretty(value).into_diagnostic().wrap_err("serialize package.json")?;
    fs::write(path, format!("{content}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", path.display()))
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
    use serde_json::json;

    #[test]
    fn remove_link_specs_only_removes_matching_link_dependencies() {
        let mut manifest = json!({
            "dependencies": {
                "foo": "link:../foo",
                "bar": "^1.0.0"
            },
            "devDependencies": {
                "baz": "link:../baz"
            }
        });

        let changed = remove_link_specs_from_manifest(&mut manifest, &[String::from("foo")])
            .expect("remove link specs");

        assert!(changed);
        assert!(manifest["dependencies"].get("foo").is_none());
        assert_eq!(manifest["dependencies"]["bar"], "^1.0.0");
        assert_eq!(manifest["devDependencies"]["baz"], "link:../baz");
    }
}
