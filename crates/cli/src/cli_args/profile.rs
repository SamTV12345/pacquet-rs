use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

#[derive(Debug, Args)]
pub struct ProfileArgs {
    #[clap(subcommand)]
    command: ProfileCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Display all profile settings.
    Get {
        /// Specific property to get.
        property: Option<String>,
    },
    /// Update a profile property.
    Set { property: String, value: String },
}

impl ProfileArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            ProfileCommand::Get { property } => get_profile(npmrc, property.as_deref()),
            ProfileCommand::Set { property, value } => set_profile(npmrc, &property, &value),
        }
    }
}

fn get_profile(npmrc: &Npmrc, property: Option<&str>) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/user");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))?;
    let output = Command::new("curl")
        .args([
            "-s",
            "-H",
            &format!("Authorization: {auth}"),
            "-H",
            "Accept: application/json",
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("fetch profile")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse profile")?;

    match property {
        Some(prop) => match value.get(prop) {
            Some(v) => println!("{}", format_value(v)),
            None => println!("undefined"),
        },
        None => {
            if let Some(obj) = value.as_object() {
                for (key, val) in obj {
                    println!("{key}: {}", format_value(val));
                }
            }
        }
    }
    Ok(())
}

fn set_profile(npmrc: &Npmrc, property: &str, value: &str) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/user");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))?;
    let payload = serde_json::json!({ property: value });
    let output = Command::new("curl")
        .args([
            "-s",
            "-X",
            "PATCH",
            "-H",
            &format!("Authorization: {auth}"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &serde_json::to_string(&payload).unwrap_or_default(),
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("update profile")?;
    if !output.status.success() {
        miette::bail!("Failed to update profile");
    }
    println!("Set {property} to {value}");
    Ok(())
}

fn format_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
