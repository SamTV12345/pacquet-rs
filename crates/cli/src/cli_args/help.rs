use clap::Args;
use miette::{Context, IntoDiagnostic};
use std::{env, process::Command};

#[derive(Debug, Args, Default)]
pub struct HelpArgs {
    /// Print all available commands.
    #[arg(short = 'a', long)]
    all: bool,

    /// Optional command to print detailed help for.
    command: Option<String>,
}

impl HelpArgs {
    pub fn run(self) -> miette::Result<()> {
        if let Some(target) = self.command.as_deref() {
            let target = canonical_help_target(target);
            let output = Command::new(
                env::current_exe().into_diagnostic().wrap_err("locate pacquet executable")?,
            )
            .args([target, "--help"])
            .output()
            .into_diagnostic()
            .wrap_err("run pacquet subcommand help")?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout)
                    .replace(&format!("Usage: pacquet {target}"), &format!("Usage: {target}"));
                print!("{stdout}");
            } else {
                println!("No results for \"{target}\"");
            }
            return Ok(());
        }

        let _ = self.all;
        let output = Command::new(
            env::current_exe().into_diagnostic().wrap_err("locate pacquet executable")?,
        )
        .arg("--help")
        .output()
        .into_diagnostic()
        .wrap_err("run pacquet --help")?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    }
}

fn canonical_help_target(target: &str) -> &str {
    match target {
        "c" => "config",
        "i" => "install",
        "ln" => "link",
        "rm" | "uninstall" | "un" | "uni" => "remove",
        "ls" | "la" | "ll" => "list",
        "dislink" => "unlink",
        "run-script" => "run",
        "it" => "install-test",
        "clean-install" | "ic" | "install-clean" => "ci",
        "rb" => "rebuild",
        "multi" | "m" => "recursive",
        "up" | "upgrade" => "update",
        other => other,
    }
}
