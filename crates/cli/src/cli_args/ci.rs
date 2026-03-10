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
        InstallArgs {
            dependency_options: self.dependency_options,
            frozen_lockfile: true,
            prefer_frozen_lockfile: false,
            no_prefer_frozen_lockfile: false,
            fix_lockfile: false,
            ignore_scripts: true,
            lockfile_only: false,
            force: false,
            resolution_only: false,
            reporter: None,
            use_store_server: false,
            shamefully_hoist: false,
            filter: vec![],
            recursive: false,
            prefer_offline: false,
            offline: false,
        }
        .run(state)
        .await
    }
}
