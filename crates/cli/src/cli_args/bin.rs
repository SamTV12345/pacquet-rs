use clap::Args;
use miette::IntoDiagnostic;
use std::{env, path::PathBuf};

#[derive(Debug, Args)]
pub struct BinArgs {
    /// Print the global executables directory.
    #[arg(short = 'g', long)]
    global: bool,
}

impl BinArgs {
    pub fn run(self, _dir: PathBuf) -> miette::Result<()> {
        let bin_dir = if self.global {
            global_bin_dir()?
        } else {
            env::current_dir().into_diagnostic()?.join("node_modules/.bin")
        };
        println!("{}", bin_dir.display());
        Ok(())
    }
}

pub(crate) fn global_bin_dir() -> miette::Result<PathBuf> {
    if let Some(home) = env::var_os("PNPM_HOME").or_else(|| env::var_os("PACQUET_HOME")) {
        return Ok(PathBuf::from(home));
    }

    if let Ok(current_exe) = env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        return Ok(parent.to_path_buf());
    }

    env::current_dir().into_diagnostic()
}
