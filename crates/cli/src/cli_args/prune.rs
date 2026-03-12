use crate::State;
use crate::cli_args::install::{InstallArgs, InstallDependencyOptions};
use clap::Args;

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Remove the packages specified in devDependencies.
    #[arg(long)]
    prod: bool,
    /// Remove the packages specified in optionalDependencies.
    #[arg(long)]
    no_optional: bool,
    /// Skip lifecycle scripts during prune.
    #[arg(long)]
    ignore_scripts: bool,
    /// Reporter name.
    #[arg(long)]
    reporter: Option<String>,
}

impl PruneArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        let PruneArgs { prod, no_optional, ignore_scripts, reporter } = self;
        let State {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            resolved_packages,
        } = state;

        let mut config = config.clone();
        config.modules_cache_max_age = 0;
        let config = config.leak();

        InstallArgs {
            dependency_options: InstallDependencyOptions { prod, dev: false, no_optional },
            frozen_lockfile: false,
            prefer_frozen_lockfile: false,
            no_prefer_frozen_lockfile: false,
            fix_lockfile: false,
            ignore_scripts,
            lockfile_only: false,
            force: true,
            resolution_only: false,
            ignore_pnpmfile: false,
            pnpmfile: None,
            reporter,
            use_store_server: false,
            shamefully_hoist: false,
            filter: Vec::new(),
            recursive: false,
            prefer_offline: false,
            offline: false,
        }
        .run(State {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            resolved_packages,
        })
        .await
    }
}
