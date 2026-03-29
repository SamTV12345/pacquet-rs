use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

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
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/{package}");
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-H", "Accept: application/json"]);
    if let Some(auth) = npmrc.auth_header_for_url(&url) {
        cmd.args(["-H", &format!("Authorization: {auth}")]);
    }
    cmd.arg(&url);
    let output = cmd.output().into_diagnostic().wrap_err("fetch package")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse response")?;
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
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/-/package/{package}/dist-tags/{tag}");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;
    let output = Command::new("curl")
        .args([
            "-s",
            "-X",
            "PUT",
            "-H",
            &format!("Authorization: {auth}"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &format!("\"{version}\""),
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("set dist-tag")?;
    if !output.status.success() {
        miette::bail!("Failed to set dist-tag {tag} on {package}@{version}");
    }
    println!("Added tag {tag} to {package}@{version}");
    Ok(())
}

fn remove_tag(package: &str, tag: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/-/package/{package}/dist-tags/{tag}");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;
    let output = Command::new("curl")
        .args(["-s", "-X", "DELETE", "-H", &format!("Authorization: {auth}"), &url])
        .output()
        .into_diagnostic()
        .wrap_err("remove dist-tag")?;
    if !output.status.success() {
        miette::bail!("Failed to remove dist-tag {tag} from {package}");
    }
    println!("Removed tag {tag} from {package}");
    Ok(())
}
