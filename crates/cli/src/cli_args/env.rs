use clap::{Args, Subcommand};
use miette::Context;
use pacquet_env::EnvManager;

#[derive(Debug, Args)]
#[command(
    about = "Manage Node.js versions.",
    override_usage = "pacquet env [command] [options] <version> [<additional-versions>...]",
    after_help = "Examples:
  pacquet env use --global 18
  pacquet env use --global lts
  pacquet env use --global argon
  pacquet env use --global latest
  pacquet env use --global rc/18
  pacquet env add --global 18
  pacquet env add --global 18 19 20.6.0
  pacquet env remove --global 18 lts
  pacquet env remove --global argon
  pacquet env remove --global latest
  pacquet env remove --global rc/18 18 20.6.0
  pacquet env list
  pacquet env list --remote
  pacquet env list --remote 18
  pacquet env list --remote lts
  pacquet env list --remote argon
  pacquet env list --remote latest
  pacquet env list --remote rc/18

Visit https://pnpm.io/10.x/cli/env for documentation about this command."
)]
pub struct EnvArgs {
    #[command(subcommand)]
    command: EnvCommand,
}

#[derive(Debug, Subcommand)]
enum EnvCommand {
    /// Installs the specified version(s) of Node.js without activating them.
    Add(EnvVersionArgs),
    /// Installs the specified version of Node.js and activates it.
    Use(EnvUseArgs),
    /// Removes the specified version(s) of Node.js.
    #[clap(alias = "rm", alias = "uninstall", alias = "un")]
    Remove(EnvVersionArgs),
    /// List Node.js versions available locally or remotely.
    #[clap(alias = "ls")]
    List(EnvListArgs),
}

#[derive(Debug, Args)]
struct EnvVersionArgs {
    /// Manages Node.js versions globally.
    #[arg(short = 'g', long)]
    global: bool,
    /// Node.js version specifier(s).
    versions: Vec<String>,
}

#[derive(Debug, Args)]
struct EnvUseArgs {
    /// Manages Node.js versions globally.
    #[arg(short = 'g', long)]
    global: bool,
    /// Node.js version specifier.
    version: String,
}

#[derive(Debug, Args)]
struct EnvListArgs {
    /// List remote versions of Node.js.
    #[arg(long)]
    remote: bool,
    /// Optional version specifier to filter remote versions.
    version: Option<String>,
}

impl EnvArgs {
    pub async fn run(self) -> miette::Result<()> {
        let manager = EnvManager::from_system().wrap_err("initialize env manager")?;
        match self.command {
            EnvCommand::Add(args) => {
                if args.versions.is_empty() {
                    miette::bail!("Please specify at least one Node.js version");
                }
                let output = manager
                    .add_versions(args.global, &args.versions)
                    .await
                    .wrap_err("add Node.js versions")?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            EnvCommand::Use(args) => {
                let output = manager
                    .use_version(args.global, &args.version)
                    .await
                    .wrap_err("activate Node.js version")?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            EnvCommand::Remove(args) => {
                if args.versions.is_empty() {
                    miette::bail!("Please specify at least one Node.js version");
                }
                let result = manager
                    .remove_versions(args.global, &args.versions)
                    .await
                    .wrap_err("remove Node.js versions")?;
                if result.exit_code != 0 {
                    miette::bail!("{}", result.failures.join("\n"));
                }
            }
            EnvCommand::List(args) => {
                let output = manager
                    .list_versions(args.remote, args.version.as_deref())
                    .await
                    .wrap_err("list Node.js versions")?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
        }
        Ok(())
    }
}
