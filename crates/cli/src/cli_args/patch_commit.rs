use crate::cli_args::patch_common::{
    patch_file_name, read_patch_state, relative_patch_path, write_patch_state,
    write_patched_dependencies,
};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args)]
pub struct PatchCommitArgs {
    /// Patch directory created by `pacquet patch`.
    edit_dir: PathBuf,

    /// Directory in which patch files should be stored.
    #[arg(long = "patches-dir")]
    patches_dir: Option<PathBuf>,
}

impl PatchCommitArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let modules_dir = project_dir.join("node_modules");
        let edit_dir = if self.edit_dir.is_absolute() {
            self.edit_dir
        } else {
            project_dir.join(self.edit_dir)
        };

        let mut state = read_patch_state(&modules_dir)?;
        let state_key = edit_dir.display().to_string();
        let patch_state = state.get(&state_key).cloned().ok_or_else(|| {
            miette::miette!("{} is not a valid patch directory", edit_dir.display())
        })?;

        let diff = diff_directories(&patch_state.original_dir, &edit_dir)?;
        if diff.trim().is_empty() {
            println!("No changes were found to the following directory: {}", edit_dir.display());
            return Ok(());
        }

        let patches_dir = self
            .patches_dir
            .map(|path| if path.is_absolute() { path } else { project_dir.join(path) })
            .unwrap_or_else(|| project_dir.join("patches"));
        fs::create_dir_all(&patches_dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", patches_dir.display()))?;

        let patch_key = if patch_state.apply_to_all {
            patch_state.package_name.clone()
        } else {
            format!("{}@{}", patch_state.package_name, patch_state.package_version)
        };
        let patch_file = patches_dir.join(format!("{}.patch", patch_file_name(&patch_key)));
        fs::write(&patch_file, diff)
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", patch_file.display()))?;

        let mut patched_dependencies =
            crate::cli_args::patch_common::read_patched_dependencies(&manifest_path)?;
        patched_dependencies
            .insert(patch_key.clone(), relative_patch_path(&project_dir, &patch_file));
        write_patched_dependencies(&manifest_path, &patched_dependencies)?;

        state.remove(&state_key);
        write_patch_state(&modules_dir, &state)?;

        println!("Patch file written to {}", patch_file.display());
        Ok(())
    }
}

fn diff_directories(original_dir: &Path, edited_dir: &Path) -> miette::Result<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.safecrlf=false",
            "diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--ignore-cr-at-eol",
            "--irreversible-delete",
            "--full-index",
            "--no-index",
            "--text",
            "--no-ext-diff",
            "--no-color",
            original_dir.to_string_lossy().as_ref(),
            edited_dir.to_string_lossy().as_ref(),
        ])
        .output()
        .into_diagnostic()
        .wrap_err("run git diff")?;

    if !output.stderr.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            miette::bail!("Unable to diff directories: {stderr}");
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(normalize_diff_paths(&stdout, original_dir, edited_dir))
}

fn normalize_diff_paths(diff: &str, original_dir: &Path, edited_dir: &Path) -> String {
    let original = normalize_path(original_dir);
    let edited = normalize_path(edited_dir);
    diff.replace(&format!("a/{original}/"), "a/")
        .replace(&format!("b/{edited}/"), "b/")
        .replace(&format!("{original}/"), "")
        .replace(&format!("{edited}/"), "")
}

fn normalize_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/").trim_matches('/').to_string()
}
