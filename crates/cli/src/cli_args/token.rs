use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[clap(subcommand)]
    command: TokenCommand,
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// List all active tokens.
    #[clap(alias = "ls")]
    List,
    /// Create a new token.
    Create {
        /// Make the token readonly.
        #[arg(long)]
        readonly: bool,
    },
    /// Revoke a token by ID or key.
    Revoke { token_key: String },
}

impl TokenArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            TokenCommand::List => list_tokens(npmrc),
            TokenCommand::Create { readonly } => create_token(npmrc, readonly),
            TokenCommand::Revoke { token_key } => revoke_token(npmrc, &token_key),
        }
    }
}

fn list_tokens(npmrc: &Npmrc) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens");
    let auth = npmrc.auth_header_for_url(&url).ok_or_else(|| miette::miette!("Not logged in."))?;
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
        .wrap_err("list tokens")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;
    if let Some(objects) = value.get("objects").and_then(Value::as_array) {
        for obj in objects {
            let key = obj.get("key").and_then(Value::as_str).unwrap_or("?");
            let created = obj.get("created").and_then(Value::as_str).unwrap_or("?");
            let readonly = obj.get("readonly").and_then(Value::as_bool).unwrap_or(false);
            let ro = if readonly { " (readonly)" } else { "" };
            println!("{key} — created {created}{ro}");
        }
    }
    Ok(())
}

fn create_token(npmrc: &Npmrc, readonly: bool) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens");
    let auth = npmrc.auth_header_for_url(&url).ok_or_else(|| miette::miette!("Not logged in."))?;
    let payload = serde_json::json!({"readonly": readonly, "cidr_whitelist": []});
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
        .wrap_err("create token")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;
    if let Some(token) = value.get("token").and_then(Value::as_str) {
        println!("Created token: {token}");
    } else {
        let error = value.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        miette::bail!("Failed to create token: {error}");
    }
    Ok(())
}

fn revoke_token(npmrc: &Npmrc, token_key: &str) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens/token/{token_key}");
    let auth = npmrc.auth_header_for_url(&url).ok_or_else(|| miette::miette!("Not logged in."))?;
    let output = Command::new("curl")
        .args(["-s", "-X", "DELETE", "-H", &format!("Authorization: {auth}"), &url])
        .output()
        .into_diagnostic()
        .wrap_err("revoke token")?;
    if !output.status.success() {
        miette::bail!("Failed to revoke token");
    }
    println!("Revoked token {token_key}");
    Ok(())
}
