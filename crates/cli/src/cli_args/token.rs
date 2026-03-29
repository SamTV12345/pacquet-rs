use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

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
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens");
    client.require_auth(&url)?;
    let value = client.get_json(&url)?;
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
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens");
    let payload = serde_json::json!({"readonly": readonly, "cidr_whitelist": []});
    let value = client.post_json(&url, &payload)?;
    if let Some(token) = value.get("token").and_then(Value::as_str) {
        println!("Created token: {token}");
    } else {
        let error = value.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        miette::bail!("Failed to create token: {error}");
    }
    Ok(())
}

fn revoke_token(npmrc: &Npmrc, token_key: &str) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = format!("{registry}/-/npm/v1/tokens/token/{token_key}");
    let resp = client.delete(&url)?;
    if !resp.status().is_success() {
        miette::bail!("Failed to revoke token");
    }
    println!("Revoked token {token_key}");
    Ok(())
}
