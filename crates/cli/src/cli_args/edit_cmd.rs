use clap::Args;
use miette::IntoDiagnostic;
use std::{path::PathBuf, process::Command};

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Package name to edit.
    package: String,
}

impl EditArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let mut package_dir = dir.join("node_modules");
        for segment in self.package.split('/') {
            package_dir.push(segment);
        }
        if !package_dir.is_dir() {
            miette::bail!("Package {} is not installed", self.package);
        }
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "vi".to_string());
        Command::new(&editor)
            .arg(&package_dir)
            .status()
            .into_diagnostic()
            .map_err(|_| miette::miette!("failed to run editor: {editor}"))?;
        Ok(())
    }
}
