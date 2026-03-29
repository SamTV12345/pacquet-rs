use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args, Default)]
pub struct PrefixArgs {
    /// Print the global prefix.
    #[arg(short = 'g', long)]
    global: bool,
}

impl PrefixArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        if self.global {
            let global = home::home_dir()
                .map(|h| h.join(".local/share/pnpm/global"))
                .unwrap_or_else(|| PathBuf::from("/usr/local"));
            println!("{}", global.display());
        } else {
            println!("{}", dir.display());
        }
        Ok(())
    }
}
