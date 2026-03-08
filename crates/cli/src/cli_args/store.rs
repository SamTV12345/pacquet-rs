use clap::{Args, Subcommand};
use miette::Context;
use pacquet_npmrc::Npmrc;

#[derive(Debug, Subcommand)]
pub enum StoreCommand {
    /// Checks for modified packages in the store.
    #[clap(alias = "store")]
    Status,
    /// Functionally equivalent to pnpm add, except this adds new packages to the store directly
    /// without modifying any projects or files outside of the store.
    Add(StoreAddArgs),
    /// Removes unreferenced packages from the store.
    /// Unreferenced packages are packages that are not used by any projects on the system.
    /// Packages can become unreferenced after most installation operations, for instance when
    /// dependencies are made redundant.
    Prune,
    /// Returns the path to the active store directory.
    Path,
}

#[derive(Debug, Args)]
pub struct StoreAddArgs {
    /// Packages to add to the store (for example: express@4 typescript@5).
    packages: Vec<String>,
}

impl StoreCommand {
    /// Execute the subcommand.
    pub fn run<'a>(self, config: impl FnOnce() -> &'a Npmrc) -> miette::Result<()> {
        match self {
            StoreCommand::Status => {
                miette::bail!("`pacquet store status` is not implemented yet");
            }
            StoreCommand::Add(args) => {
                if args.packages.is_empty() {
                    miette::bail!("Please specify at least one package");
                }
                miette::bail!("`pacquet store add` is not implemented yet");
            }
            StoreCommand::Prune => {
                config().store_dir.prune().wrap_err("pruning store")?;
            }
            StoreCommand::Path => {
                println!("{}", config().store_dir.display());
            }
        }

        Ok(())
    }
}
