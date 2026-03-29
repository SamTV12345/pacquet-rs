use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

#[derive(Debug, Args)]
pub struct DistTagArgs {
    #[clap(subcommand)]
    command: DistTagCommand,
}

#[derive(Debug, Subcommand)]
pub enum DistTagCommand {
    /// List distribution tags for a package.
    Ls { package: String },
    /// Add a distribution tag to a specific version.
    Add {
        /// Package@version (e.g., pkg@1.0.0).
        package_version: String,
        /// Tag name.
        tag: String,
    },
    /// Remove a distribution tag.
    Rm {
        /// Package name.
        package: String,
        /// Tag name.
        tag: String,
    },
}

impl DistTagArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            DistTagCommand::Ls { package } => list_tags(&package, npmrc),
            DistTagCommand::Add { package_version, tag } => add_tag(&package_version, &tag, npmrc),
            DistTagCommand::Rm { package, tag } => remove_tag(&package, &tag, npmrc),
        }
    }
}

fn list_tags(package: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/{package}");
    let value = client.get_json(&url)?;
    if let Some(tags) = value.get("dist-tags").and_then(Value::as_object) {
        for (tag, version) in tags {
            println!("{tag}: {}", version.as_str().unwrap_or("?"));
        }
    } else {
        println!("No dist-tags found");
    }
    Ok(())
}

fn add_tag(package_version: &str, tag: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let (package, version) = package_version
        .rsplit_once('@')
        .ok_or_else(|| miette::miette!("Expected format: package@version"))?;
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/-/package/{package}/dist-tags/{tag}");
    client.put_string(&url, &format!("\"{version}\""))?;
    println!("Added tag {tag} to {package}@{version}");
    Ok(())
}

fn remove_tag(package: &str, tag: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/-/package/{package}/dist-tags/{tag}");
    client.delete(&url)?;
    println!("Removed tag {tag} from {package}");
    Ok(())
}
