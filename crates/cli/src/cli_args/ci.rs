use crate::State;
use clap::Args;

use super::install::{InstallArgs, InstallDependencyOptions};

#[derive(Debug, Args)]
pub struct CiArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: InstallDependencyOptions,
}

impl CiArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        InstallArgs { dependency_options: self.dependency_options, frozen_lockfile: true }
            .run(state)
            .await
    }
}
