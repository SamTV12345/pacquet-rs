use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde_json::{Map, Value};
use std::{fs, path::PathBuf};

#[derive(Debug, Args)]
pub struct SetScriptArgs {
    /// Script name.
    name: String,
    /// Script command.
    command: String,
}

impl SetScriptArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let content = fs::read_to_string(&manifest_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
        let mut value: Value =
            serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;
        let root = value
            .as_object_mut()
            .ok_or_else(|| miette::miette!("package.json must be an object"))?;
        let scripts = root.entry("scripts").or_insert_with(|| Value::Object(Map::new()));
        let scripts =
            scripts.as_object_mut().ok_or_else(|| miette::miette!("scripts must be an object"))?;
        scripts.insert(self.name.clone(), Value::String(self.command));
        let rendered = serde_json::to_string_pretty(&value)
            .into_diagnostic()
            .wrap_err("serialize package.json")?;
        fs::write(&manifest_path, format!("{rendered}\n"))
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", manifest_path.display()))?;
        println!("Set script \"{}\"", self.name);
        Ok(())
    }
}
