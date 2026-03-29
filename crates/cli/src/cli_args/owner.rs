use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

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
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/{package}");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;

    // Fetch current maintainers
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
        .wrap_err("fetch package")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let mut value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;

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
            &serde_json::to_string(&payload).unwrap_or_default(),
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("update maintainers")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        miette::bail!("Failed to update maintainers: {err}");
    }
    let action = if add { "Added" } else { "Removed" };
    println!("{action} {user} as maintainer of {package}");
    Ok(())
}
