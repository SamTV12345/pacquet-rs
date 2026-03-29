use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
};

/// Log in to a registry.
#[derive(Debug, Args, Default)]
pub struct LoginArgs {
    /// Base URL of the registry.
    #[arg(long)]
    registry: Option<String>,

    /// Scope to associate the auth token with.
    #[arg(long)]
    scope: Option<String>,

    /// Auth type to use (legacy or web).
    #[arg(long = "auth-type")]
    auth_type: Option<String>,
}

impl LoginArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = self.registry.as_deref().unwrap_or(&npmrc.registry);
        let registry = registry.trim_end_matches('/');

        // Prompt for credentials
        let username = prompt("Username: ")?;
        let password = prompt("Password: ")?;

        // Try to authenticate with the registry
        let url = format!("{registry}/-/user/org.couchdb.user:{username}");
        let payload = serde_json::json!({
            "_id": format!("org.couchdb.user:{username}"),
            "name": username,
            "password": password,
            "type": "user",
        });

        let output = Command::new("curl")
            .args([
                "-s",
                "-X",
                "PUT",
                "-H",
                "Content-Type: application/json",
                "-d",
                &serde_json::to_string(&payload).unwrap_or_default(),
                &url,
            ])
            .output()
            .into_diagnostic()
            .wrap_err("authenticate with registry")?;

        let body = String::from_utf8_lossy(&output.stdout);
        let response: serde_json::Value =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse auth response")?;

        let token = response.get("token").and_then(serde_json::Value::as_str).ok_or_else(|| {
            let error = response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            miette::miette!("Login failed: {error}")
        })?;

        // Write token to .npmrc
        let npmrc_path =
            home::home_dir().map(|h| h.join(".npmrc")).unwrap_or_else(|| PathBuf::from(".npmrc"));
        let registry_key = registry.trim_start_matches("https:").trim_start_matches("http:");
        let line = format!("{registry_key}:_authToken={token}\n");

        let mut content = fs::read_to_string(&npmrc_path).unwrap_or_default();
        let prefix = format!("{registry_key}:_authToken=");
        let lines: Vec<&str> = content.lines().filter(|l| !l.starts_with(&prefix)).collect();
        content = lines.join("\n");
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&line);

        fs::write(&npmrc_path, content)
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", npmrc_path.display()))?;

        println!("Logged in as {username} on {registry}");
        Ok(())
    }
}

fn prompt(message: &str) -> miette::Result<String> {
    eprint!("{message}");
    io::stderr().flush().into_diagnostic()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input).into_diagnostic().wrap_err("read input")?;
    Ok(input.trim().to_string())
}
