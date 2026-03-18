pub mod add;
pub mod approve_builds;
pub mod audit;
pub mod bin;
pub mod cache;
pub mod cat_file;
pub mod cat_index;
pub mod ci;
pub mod completion;
pub mod config;
pub mod create;
pub mod dedupe;
pub mod deploy;
pub mod dlx;
pub mod doctor;
pub mod env;
pub mod exec;
pub mod fetch;
pub mod find_hash;
pub mod help;
pub mod ignored_builds;
pub mod import;
pub mod install;
pub mod install_test;
pub mod licenses;
pub mod link;
pub mod list;
pub mod outdated;
pub mod pack;
pub mod patch;
pub mod patch_commit;
pub mod patch_common;
pub mod patch_remove;
pub mod prune;
pub mod publish;
pub mod rebuild;
pub mod recursive;
pub mod remove;
pub mod restart;
pub mod root;
pub mod run;
pub mod self_update;
pub mod server;
pub mod setup;
pub mod store;
pub mod unlink;
pub mod update;
pub mod why;

use crate::State;
use crate::state::find_workspace_root;
use add::AddArgs;
use approve_builds::ApproveBuildsArgs;
use audit::AuditArgs;
use bin::BinArgs;
use cache::CacheArgs;
use cat_file::CatFileArgs;
use cat_index::CatIndexArgs;
use ci::CiArgs;
use clap::{Args, Parser, Subcommand};
use completion::CompletionArgs;
use config::{ConfigArgs, GetArgs, SetArgs};
use create::CreateArgs;
use dedupe::DedupeArgs;
use deploy::DeployArgs;
use dlx::DlxArgs;
use doctor::DoctorArgs;
use env::EnvArgs;
use exec::ExecArgs;
use fetch::FetchArgs;
use find_hash::FindHashArgs;
use help::HelpArgs;
use ignored_builds::IgnoredBuildsArgs;
use import::ImportArgs;
use install::InstallArgs;
use install_test::InstallTestArgs;
use licenses::LicensesArgs;
use link::LinkArgs;
use list::ListArgs;
use miette::{Context, IntoDiagnostic};
use outdated::OutdatedArgs;
use pack::PackArgs;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use patch::PatchArgs;
use patch_commit::PatchCommitArgs;
use patch_remove::PatchRemoveArgs;
use prune::PruneArgs;
use publish::PublishArgs;
use rebuild::RebuildArgs;
use recursive::RecursiveArgs;
use remove::RemoveArgs;
use restart::RestartArgs;
use root::RootArgs;
use run::{RunArgs, run_start, run_test};
use self_update::SelfUpdateArgs;
use server::ServerArgs;
use setup::SetupArgs;
use std::{env as std_env, ffi::OsString, path::PathBuf};
use store::StoreCommand;
use unlink::UnlinkArgs;
use update::UpdateArgs;
use why::WhyArgs;

/// Experimental package manager for node.js written in rust.
#[derive(Debug, Parser)]
#[clap(name = "pacquet")]
#[clap(bin_name = "pacquet")]
#[clap(version = "0.2.1")]
#[clap(about = "Experimental package manager for node.js")]
#[clap(disable_help_subcommand = true)]
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
    /// Approve dependencies for running build scripts during installation.
    ApproveBuilds(ApproveBuildsArgs),
    /// Inspect and manage the metadata cache.
    Cache(CacheArgs),
    /// Checks for known security issues with the installed packages.
    Audit(AuditArgs),
    /// Print a content-addressable store file by integrity hash.
    CatFile(CatFileArgs),
    /// Print the index file of a package from the store.
    CatIndex(CatIndexArgs),
    /// Display help information about pacquet.
    Help(HelpArgs),
    /// Manage the pacquet configuration files.
    #[clap(alias = "c")]
    Config(ConfigArgs),
    /// Print shell completion code to stdout.
    Completion(CompletionArgs),
    /// Create a project from a create-* starter kit.
    Create(CreateArgs),
    /// Print the directory where pacquet will install executables.
    Bin(BinArgs),
    /// Print the effective node_modules directory.
    Root(RootArgs),
    /// Install packages
    #[clap(alias = "i")]
    Install(InstallArgs),
    /// Link packages from the filesystem or global link area.
    #[clap(alias = "ln")]
    Link(LinkArgs),
    /// Install with a frozen lockfile (CI mode)
    #[clap(alias = "clean-install", alias = "ic", alias = "install-clean")]
    Ci(CiArgs),
    /// Run an install followed immediately by test.
    #[clap(alias = "it")]
    InstallTest(InstallTestArgs),
    /// Manage Node.js versions.
    Env(EnvArgs),
    /// Re-resolve dependencies to deduplicate older lockfile entries.
    Dedupe(DedupeArgs),
    /// Run a package in a temporary environment.
    Dlx(DlxArgs),
    /// Check for known common issues.
    Doctor(DoctorArgs),
    /// Deploy a package into a target directory.
    Deploy(DeployArgs),
    /// Run an arbitrary command in the current package context.
    Exec(ExecArgs),
    /// Fetch packages from the lockfile into the store without mutating the workspace.
    Fetch(FetchArgs),
    /// List packages in the store that contain a given integrity hash.
    FindHash(FindHashArgs),
    /// Print the list of packages with blocked build scripts.
    IgnoredBuilds(IgnoredBuildsArgs),
    /// Generate a pnpm-lock.yaml from an npm/yarn lockfile.
    Import(ImportArgs),
    /// Rebuild a package or re-run build lifecycle scripts.
    #[clap(alias = "rb")]
    Rebuild(RebuildArgs),
    /// Remove package(s)
    #[clap(alias = "rm", alias = "uninstall", alias = "un", alias = "uni")]
    Remove(RemoveArgs),
    /// List installed dependencies.
    #[clap(alias = "ls", alias = "la", alias = "ll")]
    List(ListArgs),
    /// Check for outdated packages.
    Outdated(OutdatedArgs),
    /// Check the licenses of the installed packages.
    Licenses(LicensesArgs),
    /// Create a tarball from a package.
    Pack(PackArgs),
    /// Prepare a package for patching.
    Patch(PatchArgs),
    /// Generate a patch out of a directory.
    PatchCommit(PatchCommitArgs),
    /// Remove existing patch files.
    PatchRemove(PatchRemoveArgs),
    /// Publishes a package to the npm registry.
    Publish(PublishArgs),
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
    /// Restart a package by running stop, restart, and start scripts.
    Restart(RestartArgs),
    /// Prefix command for recursive workspace execution.
    #[clap(alias = "multi", alias = "m")]
    Recursive(RecursiveArgs),
    /// Runs a package's "test" script, if one was provided.
    Test(ScriptShortcutArgs),
    /// Runs a defined package script.
    #[clap(alias = "run-script")]
    Run(RunArgs),
    /// Runs an arbitrary command specified in the package's start property of its scripts object.
    Start(ScriptShortcutArgs),
    /// Set up PNPM_HOME/PACQUET_HOME helper scripts.
    Setup(SetupArgs),
    /// Update pacquet to the latest version (or a specified one).
    SelfUpdate(SelfUpdateArgs),
    /// Manage a store server.
    Server(ServerArgs),
    /// Managing the package store.
    #[clap(subcommand)]
    Store(StoreCommand),
    /// Update packages to newer versions.
    #[clap(alias = "up", alias = "upgrade")]
    Update(UpdateArgs),
}

#[derive(Debug, Args, Default)]
pub struct ScriptShortcutArgs {
    /// Any additional arguments passed to the underlying script.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
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
            CliCommand::ApproveBuilds(args) => args.run(manifest_path(), npmrc).await?,
            CliCommand::Audit(args) => args.run(dir)?,
            CliCommand::Cache(args) => args.run(npmrc)?,
            CliCommand::CatFile(args) => args.run(npmrc)?,
            CliCommand::CatIndex(args) => args.run(npmrc)?,
            CliCommand::Help(args) => args.run()?,
            CliCommand::Config(args) => args.run(&dir, npmrc)?,
            CliCommand::Completion(args) => args.run()?,
            CliCommand::Create(args) => args.run(dir, npmrc).await?,
            CliCommand::Bin(args) => args.run(dir)?,
            CliCommand::Root(args) => args.run(dir)?,
            CliCommand::Add(mut args) => {
                args.invoked_with_workspace_root = workspace_root;
                args.run(state()?).await?
            }
            CliCommand::Install(args) => args.run(state()?).await?,
            CliCommand::InstallTest(args) => args.run(state()?).await?,
            CliCommand::Link(args) => args.run(dir, npmrc).await?,
            CliCommand::Ci(args) => args.run(state()?).await?,
            CliCommand::Env(args) => args.run().await?,
            CliCommand::Dedupe(args) => args.run(dir, npmrc).await?,
            CliCommand::Dlx(args) => args.run(dir, npmrc).await?,
            CliCommand::Doctor(args) => args.run()?,
            CliCommand::Deploy(args) => args.run(state()?).await?,
            CliCommand::Exec(args) => args.run(dir)?,
            CliCommand::Fetch(args) => args.run(dir, npmrc).await?,
            CliCommand::FindHash(args) => args.run(npmrc)?,
            CliCommand::IgnoredBuilds(args) => args.run(manifest_path())?,
            CliCommand::Import(args) => args.run(dir, npmrc).await?,
            CliCommand::Rebuild(args) => args.run(state()?).await?,
            CliCommand::Remove(args) => args.run(state()?).await?,
            CliCommand::List(args) => args.run(state()?)?,
            CliCommand::Outdated(args) => args.run(state()?).await?,
            CliCommand::Licenses(args) => args.run(manifest_path())?,
            CliCommand::Pack(args) => args.run(manifest_path(), npmrc)?,
            CliCommand::Patch(args) => args.run(manifest_path())?,
            CliCommand::PatchCommit(args) => args.run(manifest_path())?,
            CliCommand::PatchRemove(args) => args.run(manifest_path())?,
            CliCommand::Publish(args) => args.run(&dir, manifest_path())?,
            CliCommand::Prune(args) => args.run(state()?).await?,
            CliCommand::Why(args) => args.run(state()?)?,
            CliCommand::Unlink(args) => args.run(state()?).await?,
            CliCommand::Get(args) => args.run(&dir, npmrc)?,
            CliCommand::Set(args) => args.run(&dir, npmrc)?,
            CliCommand::Restart(args) => args.run(manifest_path(), npmrc)?,
            CliCommand::Recursive(args) => args.run()?,
            CliCommand::Test(args) => run_test(manifest_path(), &args.args, npmrc)?,
            CliCommand::Run(args) => args.run(manifest_path(), npmrc)?,
            CliCommand::Start(args) => run_start(manifest_path(), &args.args, npmrc)?,
            CliCommand::Setup(args) => args.run()?,
            CliCommand::SelfUpdate(args) => args.run(manifest_path())?,
            CliCommand::Server(args) => args.run(npmrc)?,
            CliCommand::Store(command) => command.run(|| npmrc).await?,
            CliCommand::Update(args) => args.run(state()?).await?,
        }

        Ok(())
    }
}
