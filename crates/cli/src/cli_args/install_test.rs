use crate::State;
use crate::cli_args::install::InstallArgs;
use crate::cli_args::run::run_test;
use clap::Args;
use std::ffi::OsString;

#[derive(Debug, Args)]
pub struct InstallTestArgs {
    #[clap(flatten)]
    install: InstallArgs,

    /// Any additional arguments passed to the test script.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    test_args: Vec<OsString>,
}

impl InstallTestArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        let manifest_path = state.manifest.path().to_path_buf();
        let config = state.config;
        self.install.run(state).await?;
        run_test(manifest_path, &self.test_args, config)
    }
}
