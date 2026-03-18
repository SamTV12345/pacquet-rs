use crate::cli_args::patch_common::{read_patched_dependencies, write_patched_dependencies};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct PatchRemoveArgs {
    /// Patch keys to remove. If omitted, all configured patches are removed.
    patches: Vec<String>,
}

impl PatchRemoveArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let mut patched_dependencies = read_patched_dependencies(&manifest_path)?;
        if patched_dependencies.is_empty() {
            miette::bail!("There are no patches that need to be removed");
        }

        let patches_to_remove = if self.patches.is_empty() {
            patched_dependencies.keys().cloned().collect::<Vec<_>>()
        } else {
            self.patches
        };

        for patch in &patches_to_remove {
            if !patched_dependencies.contains_key(patch) {
                miette::bail!("Patch \"{patch}\" not found in patched dependencies");
            }
        }

        let mut touched_dirs = BTreeMap::<PathBuf, ()>::new();
        for patch in patches_to_remove {
            if let Some(relative_path) = patched_dependencies.remove(&patch) {
                let patch_file = project_dir.join(relative_path);
                if let Some(parent) = patch_file.parent() {
                    touched_dirs.insert(parent.to_path_buf(), ());
                }
                let _ = fs::remove_file(&patch_file);
            }
        }

        for patch_dir in touched_dirs.keys() {
            if patch_dir.is_dir()
                && patch_dir
                    .read_dir()
                    .into_diagnostic()
                    .wrap_err_with(|| format!("read {}", patch_dir.display()))?
                    .next()
                    .is_none()
            {
                let _ = fs::remove_dir(patch_dir);
            }
        }

        write_patched_dependencies(&manifest_path, &patched_dependencies)?;
        Ok(())
    }
}
