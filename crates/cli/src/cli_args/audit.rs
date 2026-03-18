use clap::Args;
use miette::{Context, IntoDiagnostic};
use std::{path::PathBuf, process::Command};

#[derive(Debug, Args, Default)]
pub struct AuditArgs {
    /// Output audit report in JSON format.
    #[arg(long)]
    json: bool,

    /// Only print advisories with severity greater than or equal to the provided one.
    #[arg(long = "audit-level")]
    audit_level: Option<String>,

    /// Only audit devDependencies.
    #[arg(short = 'D', long)]
    dev: bool,

    /// Only audit dependencies and optionalDependencies.
    #[arg(short = 'P', long = "prod")]
    prod: bool,

    /// Don't audit optionalDependencies.
    #[arg(long = "no-optional")]
    no_optional: bool,

    /// Use exit code 0 if the registry responds with an error.
    #[arg(long = "ignore-registry-errors")]
    ignore_registry_errors: bool,

    /// Add overrides to package.json to force non-vulnerable versions.
    #[arg(long)]
    fix: bool,
}

impl AuditArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        if !dir.join("pnpm-lock.yaml").is_file() {
            miette::bail!("No pnpm-lock.yaml found: Cannot audit a project without a lockfile");
        }
        if self.fix {
            miette::bail!("`pacquet audit --fix` is not implemented yet");
        }

        let mut command = Command::new("npm");
        command.arg("audit");
        if self.json {
            command.arg("--json");
        }
        if let Some(level) = &self.audit_level {
            command.args(["--audit-level", level]);
        }
        if self.dev && !self.prod {
            command.args(["--omit", "prod"]);
        } else if self.prod && !self.dev {
            command.args(["--omit", "dev"]);
        }
        if self.no_optional {
            command.args(["--omit", "optional"]);
        }
        command.current_dir(dir);

        let status = command.status().into_diagnostic().wrap_err("run npm audit")?;
        if !status.success() && !self.ignore_registry_errors {
            miette::bail!("audit reported issues");
        }
        Ok(())
    }
}
