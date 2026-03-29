use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::process::Command;

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
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/-/package/{package}/access");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;
    let payload = serde_json::json!({"access": level});
    let output = Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
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
        .wrap_err("set access")?;
    if !output.status.success() {
        miette::bail!("Failed to set access for {package}");
    }
    println!("Set {package} to {level}");
    Ok(())
}

fn list_access(entity: Option<&str>, npmrc: &Npmrc) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = match entity {
        Some(e) => format!("{registry}/-/org/{e}/package"),
        None => {
            miette::bail!("Please specify an org or user scope");
        }
    };
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;
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
        .wrap_err("list access")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&body).into_diagnostic().wrap_err("parse response")?;
    if let Some(obj) = value.as_object() {
        for (pkg, access) in obj {
            println!("{pkg}: {}", access.as_str().unwrap_or("?"));
        }
    }
    Ok(())
}
