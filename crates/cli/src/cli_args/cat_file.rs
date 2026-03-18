use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::fs;

#[derive(Debug, Args)]
pub struct CatFileArgs {
    /// Integrity hash to read from the content-addressable store.
    hash: String,
}

impl CatFileArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let normal_path = npmrc
            .store_dir
            .cas_file_path_by_integrity(&self.hash, false)
            .ok_or_else(|| miette::miette!("Invalid integrity hash `{}`", self.hash))?;
        let executable_path = npmrc
            .store_dir
            .cas_file_path_by_integrity(&self.hash, true)
            .ok_or_else(|| miette::miette!("Invalid integrity hash `{}`", self.hash))?;

        let path = if normal_path.is_file() {
            normal_path
        } else if executable_path.is_file() {
            executable_path
        } else {
            miette::bail!("Corresponding hash file not found");
        };

        let contents = fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", path.display()))?;
        print!("{contents}");
        Ok(())
    }
}
