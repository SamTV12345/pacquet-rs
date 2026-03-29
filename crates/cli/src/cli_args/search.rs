use crate::cli_args::registry_client::RegistryClient;
use clap::Args;
use pacquet_npmrc::Npmrc;
use serde::Deserialize;

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

        let client = RegistryClient::new(npmrc);
        let registry = client.default_registry();
        let query = self.query.join("+");
        let url = format!("{registry}/-/v1/search?text={query}&size={}", self.size);

        if self.json {
            let value = client.get_json(&url)?;
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            return Ok(());
        }

        let value = client.get_json(&url)?;
        let response: SearchResponse = serde_json::from_value(value)
            .map_err(|e| miette::miette!("parse search response: {e}"))?;

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
