use crate::cli_args::bin::global_bin_dir;
use clap::Args;
use miette::IntoDiagnostic;
use std::{env, path::PathBuf};

#[derive(Debug, Args)]
pub struct RootArgs {
    /// Print the global node_modules directory.
    #[arg(short = 'g', long)]
    global: bool,
}

impl RootArgs {
    pub fn run(self, _dir: PathBuf) -> miette::Result<()> {
        let root = if self.global {
            global_bin_dir()?.join("global").join("node_modules")
        } else {
            env::current_dir().into_diagnostic()?.join("node_modules")
        };
        println!("{}", root.display());
        Ok(())
    }
}
