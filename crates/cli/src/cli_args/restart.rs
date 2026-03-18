use crate::cli_args::run::run_named_script;
use clap::Args;
use pacquet_npmrc::Npmrc;
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Args)]
pub struct RestartArgs {
    /// Avoid exiting with a non-zero exit code when one of the scripts is undefined.
    #[clap(long)]
    if_present: bool,

    /// Any additional arguments passed to the underlying scripts.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

impl RestartArgs {
    pub fn run(self, manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
        let args =
            self.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        run_named_script(manifest_path.clone(), "stop", &args, self.if_present, false, config)?;
        run_named_script(manifest_path.clone(), "restart", &args, self.if_present, false, config)?;
        run_named_script(manifest_path, "start", &args, self.if_present, true, config)
    }
}
