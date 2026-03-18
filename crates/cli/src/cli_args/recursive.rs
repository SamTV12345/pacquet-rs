use clap::Args;

const HELP_TEXT: &str = "\
Usage: pacquet recursive [command] [flags] [--filter <package selector>]
       pacquet multi [command] [flags] [--filter <package selector>]
       pacquet m [command] [flags] [--filter <package selector>]

Supported recursive wrappers:
  add
  exec
  install
  outdated
  remove
  run
  test
  unlink

For the supported commands above, pacquet rewrites `recursive` to the matching
command with `--recursive`.
";

#[derive(Debug, Args, Default)]
pub struct RecursiveArgs {}

impl RecursiveArgs {
    pub fn run(self) -> miette::Result<()> {
        println!("{HELP_TEXT}");
        std::process::exit(1);
    }
}
