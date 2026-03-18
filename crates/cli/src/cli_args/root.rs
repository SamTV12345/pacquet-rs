use crate::cli_args::bin::global_bin_dir;
use clap::Args;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Args)]
pub struct RootArgs {
    /// Print the global node_modules directory.
    #[arg(short = 'g', long)]
    global: bool,
}

impl RootArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let root = if self.global {
            global_bin_dir()?.join("global").join("node_modules")
        } else {
            normalize_path(&dir).join("node_modules")
        };
        println!("{}", root.display());
        Ok(())
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
