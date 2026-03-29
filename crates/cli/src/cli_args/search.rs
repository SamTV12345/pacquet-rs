use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde::Deserialize;
use serde_json::Value;
use std::process::Command;

/// Search the npm registry.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search query.
    query: Vec<String>,

    /// Output in JSON format.
    #[arg(long)]
    json: bool,

    /// Restrict results count.
    #[arg(long, default_value = "20")]
    size: usize,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    objects: Vec<SearchObject>,
}

#[derive(Debug, Deserialize)]
struct SearchObject {
    package: SearchPackage,
}

#[derive(Debug, Deserialize)]
struct SearchPackage {
    name: String,
    version: String,
    description: Option<String>,
    date: Option<String>,
}

impl SearchArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        if self.query.is_empty() {
            miette::bail!("search requires a query");
        }

        let registry = npmrc.registry.trim_end_matches('/');
        let query = self.query.join("+");
        let url = format!("{registry}/-/v1/search?text={query}&size={}", self.size);

        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-H", "Accept: application/json"]);
        if let Some(auth) = npmrc.auth_header_for_url(&url) {
            cmd.args(["-H", &format!("Authorization: {auth}")]);
        }
        cmd.arg(&url);

        let output = cmd.output().into_diagnostic().wrap_err("search registry")?;
        let body = String::from_utf8_lossy(&output.stdout);

        if self.json {
            let value: Value =
                serde_json::from_str(&body).into_diagnostic().wrap_err("parse search response")?;
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            return Ok(());
        }

        let response: SearchResponse =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse search response")?;

        if response.objects.is_empty() {
            println!("No results found");
            return Ok(());
        }

        for obj in &response.objects {
            let pkg = &obj.package;
            let desc = pkg.description.as_deref().unwrap_or("");
            let date = pkg.date.as_deref().unwrap_or("");
            println!(
                "{:<40} {:<12} {}  {}",
                pkg.name,
                pkg.version,
                date.get(..10).unwrap_or(date),
                desc
            );
        }
        Ok(())
    }
}
