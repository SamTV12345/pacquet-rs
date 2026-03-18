use crate::cli_args::patch_common::{
    PatchEditState, copy_dir_recursive, installed_package_dir, parse_package_spec,
    patch_state_file_path, read_patch_state, read_patched_dependencies, write_patch_state,
};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args)]
pub struct PatchArgs {
    /// Package that should be modified.
    package: String,

    /// Directory to extract the package into.
    #[arg(short = 'd', long = "edit-dir")]
    edit_dir: Option<PathBuf>,

    /// Ignore existing patch files when patching.
    #[arg(long)]
    ignore_existing: bool,
}

impl PatchArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let modules_dir = project_dir.join("node_modules");
        let (package_name, expected_version) = parse_package_spec(&self.package);
        let package_dir = installed_package_dir(&project_dir, &package_name);
        if !package_dir.join("package.json").is_file() {
            miette::bail!(
                "Can not find {} in project {}, did you forget to install it?",
                self.package,
                project_dir.display()
            );
        }

        let installed_manifest = read_installed_manifest(&package_dir.join("package.json"))?;
        let installed_version = installed_manifest
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0")
            .to_string();
        if let Some(expected_version) = expected_version.as_deref()
            && expected_version != installed_version
        {
            miette::bail!(
                "Installed version mismatch for {}: expected {}, found {}",
                package_name,
                expected_version,
                installed_version
            );
        }

        let edit_dir = self.edit_dir.unwrap_or_else(|| {
            modules_dir.join(".pnpm_patches").join(format!(
                "{}@{}",
                package_name.replace('/', "__"),
                installed_version
            ))
        });
        if edit_dir.exists()
            && edit_dir
                .read_dir()
                .into_diagnostic()
                .wrap_err_with(|| format!("read {}", edit_dir.display()))?
                .next()
                .is_some()
        {
            miette::bail!("The directory {} is not empty", edit_dir.display());
        }

        copy_dir_recursive(&package_dir, &edit_dir)?;

        if !self.ignore_existing {
            apply_existing_patch_if_any(
                &manifest_path,
                &project_dir,
                &package_name,
                &installed_version,
                &edit_dir,
            )?;
        }

        let mut state = read_patch_state(&modules_dir)?;
        state.insert(
            edit_dir.display().to_string(),
            PatchEditState {
                original_dir: package_dir,
                package_name: package_name.clone(),
                package_version: installed_version,
                patched_pkg: self.package.clone(),
                apply_to_all: expected_version.is_none(),
            },
        );
        write_patch_state(&modules_dir, &state)?;

        println!("Patch directory prepared at {}", edit_dir.display());
        println!("To commit your changes, run: pacquet patch-commit \"{}\"", edit_dir.display());
        Ok(())
    }
}

fn read_installed_manifest(path: &Path) -> miette::Result<Value> {
    let content = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).into_diagnostic().wrap_err("parse installed package.json")
}

fn apply_existing_patch_if_any(
    manifest_path: &Path,
    project_dir: &Path,
    package_name: &str,
    package_version: &str,
    edit_dir: &Path,
) -> miette::Result<()> {
    let patched_dependencies = read_patched_dependencies(manifest_path)?;
    let patch_path = patched_dependencies
        .get(&format!("{package_name}@{package_version}"))
        .or_else(|| patched_dependencies.get(package_name))
        .map(|relative| project_dir.join(relative));
    let Some(patch_path) = patch_path else {
        return Ok(());
    };
    if !patch_path.is_file() {
        miette::bail!("Unable to find patch file {}", patch_path.display());
    }
    let status = Command::new("git")
        .args(["apply", "--reject", "--whitespace=nowarn", patch_path.to_string_lossy().as_ref()])
        .current_dir(edit_dir)
        .status()
        .into_diagnostic()
        .wrap_err("run git apply")?;
    if !status.success() {
        miette::bail!("failed to apply existing patch {}", patch_path.display());
    }
    let _ = patch_state_file_path;
    Ok(())
}
