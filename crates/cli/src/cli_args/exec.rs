use clap::Args;
use miette::IntoDiagnostic;
use pacquet_executor::{ExecuteCommand, execute_command};
use pacquet_package_manifest::PackageManifest;
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// The command to run.
    pub command: String,

    /// Arguments passed to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl ExecArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let mut extra_env = vec![
            (OsString::from("npm_config_verify_deps_before_run"), OsString::from("false")),
            (OsString::from("pnpm_config_verify_deps_before_run"), OsString::from("false")),
            (OsString::from("npm_command"), OsString::from("exec")),
        ];
        let manifest_path = dir.join("package.json");
        if manifest_path.is_file() {
            let manifest = PackageManifest::from_path(manifest_path).into_diagnostic()?;
            if let Some(name) = manifest.value().get("name").and_then(serde_json::Value::as_str) {
                extra_env.push((OsString::from("PNPM_PACKAGE_NAME"), OsString::from(name)));
            }
        }
        execute_command(ExecuteCommand {
            pkg_root: &dir,
            program: &self.command,
            args: &self.args,
            extra_env: &extra_env,
        })
        .into_diagnostic()?;
        Ok(())
    }
}
