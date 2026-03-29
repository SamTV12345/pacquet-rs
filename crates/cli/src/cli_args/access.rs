use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;

use crate::cli_args::registry_client::RegistryClient;

#[derive(Debug, Args)]
pub struct AccessArgs {
    #[clap(subcommand)]
    command: AccessCommand,
}

#[derive(Debug, Subcommand)]
pub enum AccessCommand {
    /// Set a package to be publicly accessible.
    Public { package: String },
    /// Set a package to be restricted.
    Restricted { package: String },
    /// List packages with access info.
    #[clap(alias = "list")]
    Ls {
        /// User or org scope.
        entity: Option<String>,
    },
}

impl AccessArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            AccessCommand::Public { package } => set_access(&package, "public", npmrc),
            AccessCommand::Restricted { package } => set_access(&package, "restricted", npmrc),
            AccessCommand::Ls { entity } => list_access(entity.as_deref(), npmrc),
        }
    }
}

fn set_access(package: &str, level: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/-/package/{package}/access");
    let payload = serde_json::json!({"access": level});
    client.post_json(&url, &payload)?;
    println!("Set {package} to {level}");
    Ok(())
}

fn list_access(entity: Option<&str>, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.default_registry();
    let url = match entity {
        Some(e) => format!("{registry}/-/org/{e}/package"),
        None => {
            miette::bail!("Please specify an org or user scope");
        }
    };
    let value = client.get_json(&url)?;
    if let Some(obj) = value.as_object() {
        for (pkg, access) in obj {
            println!("{pkg}: {}", access.as_str().unwrap_or("?"));
        }
    }
    Ok(())
}
