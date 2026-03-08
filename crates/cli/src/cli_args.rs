pub mod add;
pub mod ci;
pub mod env;
pub mod install;
pub mod remove;
pub mod run;
pub mod store;

use crate::State;
use add::AddArgs;
use ci::CiArgs;
use clap::{Parser, Subcommand};
use env::EnvArgs;
use install::InstallArgs;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use remove::RemoveArgs;
use run::{RunArgs, run_start, run_test};
use std::{env as std_env, path::PathBuf};
use store::StoreCommand;

/// Experimental package manager for node.js written in rust.
#[derive(Debug, Parser)]
#[clap(name = "pacquet")]
#[clap(bin_name = "pacquet")]
#[clap(version = "0.2.1")]
#[clap(about = "Experimental package manager for node.js")]
pub struct CliArgs {
    #[clap(subcommand)]
    pub command: CliCommand,

    /// Set working directory.
    #[clap(short = 'C', long, default_value = ".")]
    pub dir: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Initialize a package.json
    Init,
    /// Add a package
    Add(AddArgs),
    /// Install packages
    Install(InstallArgs),
    /// Install with a frozen lockfile (CI mode)
    Ci(CiArgs),
    /// Manage Node.js versions.
    Env(EnvArgs),
    /// Remove package(s)
    #[clap(alias = "rm", alias = "uninstall", alias = "un", alias = "uni")]
    Remove(RemoveArgs),
    /// Runs a package's "test" script, if one was provided.
    Test,
    /// Runs a defined package script.
    Run(RunArgs),
    /// Runs an arbitrary command specified in the package's start property of its scripts object.
    Start,
    /// Managing the package store.
    #[clap(subcommand)]
    Store(StoreCommand),
}

impl CliArgs {
    /// Execute the command
    pub async fn run(self) -> miette::Result<()> {
        let CliArgs { command, dir } = self;
        let dir = if dir.is_absolute() {
            dir
        } else {
            std_env::current_dir().into_diagnostic().wrap_err("get current directory")?.join(dir)
        };

        std_env::set_current_dir(&dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("set current directory to {dir}", dir = dir.display()))?;

        let manifest_path = || dir.join("package.json");
        let npmrc =
            || Npmrc::current(std_env::current_dir, home::home_dir, Default::default).leak();
        let state = || State::init(manifest_path(), npmrc()).wrap_err("initialize the state");

        match command {
            CliCommand::Init => {
                PackageManifest::init(&manifest_path()).wrap_err("initialize package.json")?;
            }
            CliCommand::Add(args) => args.run(state()?).await?,
            CliCommand::Install(args) => args.run(state()?).await?,
            CliCommand::Ci(args) => args.run(state()?).await?,
            CliCommand::Env(args) => args.run().await?,
            CliCommand::Remove(args) => args.run(state()?).await?,
            CliCommand::Test => run_test(manifest_path(), npmrc())?,
            CliCommand::Run(args) => args.run(manifest_path(), npmrc())?,
            CliCommand::Start => run_start(manifest_path(), npmrc())?,
            CliCommand::Store(command) => command.run(|| npmrc())?,
        }

        Ok(())
    }
}
