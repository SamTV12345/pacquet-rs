use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(Debug, Args, Default)]
pub struct SelfUpdateArgs {
    /// Target version/tag to record for pacquet.
    version: Option<String>,
}

impl SelfUpdateArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let target = self.version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        if !manifest_path.is_file() {
            println!("pacquet v{} is already active", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        let content = fs::read_to_string(&manifest_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
        let mut value: Value =
            serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;
        let root = value
            .as_object_mut()
            .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
        let next = format!("pacquet@{target}");

        if root.get("packageManager").and_then(Value::as_str) == Some(next.as_str()) {
            println!("The current project is already set to use pacquet v{target}");
            return Ok(());
        }

        root.insert("packageManager".to_string(), Value::String(next));
        let rendered = serde_json::to_string_pretty(&value)
            .into_diagnostic()
            .wrap_err("serialize package.json")?;
        fs::write(&manifest_path, format!("{rendered}\n"))
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", manifest_path.display()))?;
        println!("The current project has been updated to use pacquet v{target}");
        Ok(())
    }
}
