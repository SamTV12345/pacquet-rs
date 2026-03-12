pub mod add;
pub mod bin;
pub mod cache;
pub mod ci;
pub mod config;
pub mod dedupe;
pub mod dlx;
pub mod env;
pub mod exec;
pub mod fetch;
pub mod install;
pub mod link;
pub mod list;
pub mod outdated;
pub mod prune;
pub mod remove;
pub mod run;
pub mod store;
pub mod unlink;
pub mod why;

use crate::State;
use crate::state::find_workspace_root;
use add::AddArgs;
use bin::BinArgs;
use cache::CacheArgs;
use ci::CiArgs;
use clap::{Parser, Subcommand};
use config::{ConfigArgs, GetArgs, SetArgs};
use dedupe::DedupeArgs;
use dlx::DlxArgs;
use env::EnvArgs;
use exec::ExecArgs;
use fetch::FetchArgs;
use install::InstallArgs;
use link::LinkArgs;
use list::ListArgs;
use miette::{Context, IntoDiagnostic};
use outdated::OutdatedArgs;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use prune::PruneArgs;
use remove::RemoveArgs;
use run::{RunArgs, run_start, run_test};
use std::{env as std_env, path::PathBuf};
use store::StoreCommand;
use unlink::UnlinkArgs;
use why::WhyArgs;

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

    /// Run the command from the workspace root.
    #[clap(short = 'w', long)]
    pub workspace_root: bool,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Initialize a package.json
    Init,
    /// Add a package
    Add(AddArgs),
    /// Inspect and manage the metadata cache.
    Cache(CacheArgs),
    /// Manage the pacquet configuration files.
    #[clap(alias = "c")]
    Config(ConfigArgs),
    /// Print the directory where pacquet will install executables.
    Bin(BinArgs),
    /// Install packages
    Install(InstallArgs),
    /// Link packages from the filesystem or global link area.
    #[clap(alias = "ln")]
    Link(LinkArgs),
    /// Install with a frozen lockfile (CI mode)
    Ci(CiArgs),
    /// Manage Node.js versions.
    Env(EnvArgs),
    /// Re-resolve dependencies to deduplicate older lockfile entries.
    Dedupe(DedupeArgs),
    /// Run a package in a temporary environment.
    Dlx(DlxArgs),
    /// Run an arbitrary command in the current package context.
    Exec(ExecArgs),
    /// Fetch packages from the lockfile into the store without mutating the workspace.
    Fetch(FetchArgs),
    /// Remove package(s)
    #[clap(alias = "rm", alias = "uninstall", alias = "un", alias = "uni")]
    Remove(RemoveArgs),
    /// List installed dependencies.
    #[clap(alias = "ls", alias = "la", alias = "ll")]
    List(ListArgs),
    /// Check for outdated packages.
    Outdated(OutdatedArgs),
    /// Removes extraneous packages.
    Prune(PruneArgs),
    /// Shows all packages that depend on the specified package.
    Why(WhyArgs),
    /// Removes links created by `pacquet link` and reinstalls dependencies.
    #[clap(alias = "dislink")]
    Unlink(UnlinkArgs),
    /// Print the config value for the provided key.
    Get(GetArgs),
    /// Set the config key to the value provided.
    Set(SetArgs),
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
        let CliArgs { command, dir, workspace_root } = self;
        let current_dir =
            std_env::current_dir().into_diagnostic().wrap_err("get current directory")?;
        let mut dir = if dir.is_absolute() { dir } else { current_dir.join(dir) };
        if workspace_root {
            dir = find_workspace_root(&dir).ok_or_else(|| {
                miette::miette!("could not find pnpm-workspace.yaml from {}", dir.display())
            })?;
        }

        std_env::set_current_dir(&dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("set current directory to {dir}", dir = dir.display()))?;

        let manifest_path = || dir.join("package.json");
        let npmrc = Npmrc::current(std_env::current_dir, home::home_dir, Default::default)
            .wrap_err("load .npmrc")?
            .leak();
        let state = || State::init(manifest_path(), npmrc).wrap_err("initialize the state");

        match command {
            CliCommand::Init => {
                PackageManifest::init(&manifest_path()).wrap_err("initialize package.json")?;
            }
            CliCommand::Cache(args) => args.run(npmrc)?,
            CliCommand::Config(args) => args.run(&dir, npmrc)?,
            CliCommand::Bin(args) => args.run(dir)?,
            CliCommand::Add(mut args) => {
                args.invoked_with_workspace_root = workspace_root;
                args.run(state()?).await?
            }
            CliCommand::Install(args) => args.run(state()?).await?,
            CliCommand::Link(args) => args.run(dir, npmrc).await?,
            CliCommand::Ci(args) => args.run(state()?).await?,
            CliCommand::Env(args) => args.run().await?,
            CliCommand::Dedupe(args) => args.run(dir, npmrc).await?,
            CliCommand::Dlx(args) => args.run(dir, npmrc).await?,
            CliCommand::Exec(args) => args.run(dir)?,
            CliCommand::Fetch(args) => args.run(dir, npmrc).await?,
            CliCommand::Remove(args) => args.run(state()?).await?,
            CliCommand::List(args) => args.run(state()?)?,
            CliCommand::Outdated(args) => args.run(state()?).await?,
            CliCommand::Prune(args) => args.run(state()?).await?,
            CliCommand::Why(args) => args.run(state()?)?,
            CliCommand::Unlink(args) => args.run(state()?).await?,
            CliCommand::Get(args) => args.run(&dir, npmrc)?,
            CliCommand::Set(args) => args.run(&dir, npmrc)?,
            CliCommand::Test => run_test(manifest_path(), npmrc)?,
            CliCommand::Run(args) => args.run(manifest_path(), npmrc)?,
            CliCommand::Start => run_start(manifest_path(), npmrc)?,
            CliCommand::Store(command) => command.run(|| npmrc).await?,
        }

        Ok(())
    }
}
