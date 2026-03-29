use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(Debug, Args)]
pub struct PkgArgs {
    #[clap(subcommand)]
    command: PkgCommand,
}

#[derive(Debug, Subcommand)]
pub enum PkgCommand {
    /// Get a field from package.json.
    Get { key: String },
    /// Set a field in package.json.
    Set { key: String, value: String },
    /// Delete a field from package.json.
    Delete { key: String },
}

impl PkgArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        match self.command {
            PkgCommand::Get { key } => {
                let value = read_manifest(&manifest_path)?;
                match get_nested(&value, &key) {
                    Some(v) => println!("{}", format_value(v)),
                    None => println!("undefined"),
                }
            }
            PkgCommand::Set { key, value } => {
                let mut manifest = read_manifest(&manifest_path)?;
                let parsed: Value =
                    serde_json::from_str(&value).unwrap_or_else(|_| Value::String(value));
                set_nested(&mut manifest, &key, parsed);
                write_manifest(&manifest_path, &manifest)?;
            }
            PkgCommand::Delete { key } => {
                let mut manifest = read_manifest(&manifest_path)?;
                delete_nested(&mut manifest, &key);
                write_manifest(&manifest_path, &manifest)?;
            }
        }
        Ok(())
    }
}

fn read_manifest(path: &std::path::Path) -> miette::Result<Value> {
    let content = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")
}

fn write_manifest(path: &std::path::Path, value: &Value) -> miette::Result<()> {
    let rendered =
        serde_json::to_string_pretty(value).into_diagnostic().wrap_err("serialize package.json")?;
    fs::write(path, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", path.display()))
}

fn get_nested<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn set_nested(value: &mut Value, key: &str, new_value: Value) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = value;
    for &part in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = Value::Object(Default::default());
        }
        let obj = current.as_object_mut().unwrap();
        if !obj.contains_key(part) {
            obj.insert(part.to_string(), Value::Object(Default::default()));
        }
        current = obj.get_mut(part).unwrap();
    }
    if let Some(obj) = current.as_object_mut() {
        obj.insert(parts.last().unwrap().to_string(), new_value);
    }
}

fn delete_nested(value: &mut Value, key: &str) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = value;
    for &part in &parts[..parts.len() - 1] {
        match current.as_object_mut().and_then(|obj| obj.get_mut(part)) {
            Some(next) => current = next,
            None => return,
        }
    }
    if let Some(obj) = current.as_object_mut() {
        obj.remove(*parts.last().unwrap());
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}
