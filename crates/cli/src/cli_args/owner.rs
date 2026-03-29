use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

#[derive(Debug, Args)]
pub struct OwnerArgs {
    #[clap(subcommand)]
    command: OwnerCommand,
}

#[derive(Debug, Subcommand)]
pub enum OwnerCommand {
    /// List maintainers of a package.
    #[clap(alias = "list")]
    Ls { package: String },
    /// Add a maintainer to a package.
    Add { user: String, package: String },
    /// Remove a maintainer from a package.
    #[clap(alias = "remove")]
    Rm { user: String, package: String },
}

impl OwnerArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            OwnerCommand::Ls { package } => list_owners(&package, npmrc),
            OwnerCommand::Add { user, package } => modify_owners(&package, &user, true, npmrc),
            OwnerCommand::Rm { user, package } => modify_owners(&package, &user, false, npmrc),
        }
    }
}

fn list_owners(package: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/{package}");
    let value = client.get_json(&url)?;
    if let Some(maintainers) = value.get("maintainers").and_then(Value::as_array) {
        for m in maintainers {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
            let email = m.get("email").and_then(Value::as_str).unwrap_or("");
            if email.is_empty() {
                println!("{name}");
            } else {
                println!("{name} <{email}>");
            }
        }
    }
    Ok(())
}

fn modify_owners(package: &str, user: &str, add: bool, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/{package}");

    // Fetch current maintainers
    let mut value = client.get_json(&url)?;

    let maintainers = value
        .get_mut("maintainers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| miette::miette!("No maintainers field found"))?;

    if add {
        maintainers.push(serde_json::json!({"name": user}));
    } else {
        maintainers.retain(|m| m.get("name").and_then(Value::as_str) != Some(user));
    }

    let payload = serde_json::json!({"maintainers": maintainers});
    client.put_json(&url, &payload)?;
    let action = if add { "Added" } else { "Removed" };
    println!("{action} {user} as maintainer of {package}");
    Ok(())
}
