use crate::cli_args::bugs::{load_package_manifest, open_url};
use clap::Args;
use serde_json::Value;
use std::path::PathBuf;

/// Open the documentation page for a package.
#[derive(Debug, Args)]
pub struct DocsArgs {
    /// Package name (defaults to current project).
    package: Option<String>,

    /// Print the URL without opening it.
    #[arg(long)]
    no_browser: bool,
}

impl DocsArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let manifest = load_package_manifest(&dir, self.package.as_deref())?;
        let url = manifest
            .get("homepage")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| miette::miette!("No homepage found in package.json"))?;
        println!("{url}");
        if !self.no_browser {
            open_url(&url);
        }
        Ok(())
    }
}
