use crate::cli_args::bugs::{get_repo_url, load_package_manifest, open_url};
use clap::Args;
use std::path::PathBuf;

/// Open the source code repository page for a package.
#[derive(Debug, Args)]
pub struct RepoArgs {
    /// Package name (defaults to current project).
    package: Option<String>,

    /// Print the URL without opening it.
    #[arg(long)]
    no_browser: bool,
}

impl RepoArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let manifest = load_package_manifest(&dir, self.package.as_deref())?;
        let url = get_repo_url(&manifest)
            .ok_or_else(|| miette::miette!("No repository URL found in package.json"))?;
        println!("{url}");
        if !self.no_browser {
            open_url(&url);
        }
        Ok(())
    }
}
