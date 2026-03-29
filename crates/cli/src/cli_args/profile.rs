use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

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
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/user");
    client.require_auth(&url)?;
    let value = client.get_json(&url)?;

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
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/user");
    client.require_auth(&url)?;
    let payload = serde_json::json!({ property: value });
    let resp = client.patch_json(&url, &payload)?;
    if !resp.status().is_success() {
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
