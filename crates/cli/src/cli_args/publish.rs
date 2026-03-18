use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_package_manifest::PackageManifest;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args, Default)]
pub struct PublishArgs {
    /// Directory or tarball to publish.
    target: Option<PathBuf>,

    /// Does everything a publish would do except actually publishing to the registry.
    #[arg(long)]
    dry_run: bool,

    /// Show information in JSON format.
    #[arg(long)]
    json: bool,

    /// Registers the published package with the given tag.
    #[arg(long)]
    tag: Option<String>,

    /// Tells the registry whether this package should be published as public or restricted.
    #[arg(long)]
    access: Option<String>,

    /// Ignores publish related lifecycle scripts.
    #[arg(long)]
    ignore_scripts: bool,

    /// Publish all packages from the workspace.
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Continue even if the current version may already exist.
    #[arg(long)]
    force: bool,
}

impl PublishArgs {
    pub fn run(self, dir: &Path, manifest_path: PathBuf) -> miette::Result<()> {
        if self.recursive {
            miette::bail!("`pacquet publish --recursive` is not implemented yet");
        }

        let manifest = PackageManifest::from_path(manifest_path).wrap_err("load package.json")?;
        let package_name =
            manifest.value().get("name").and_then(serde_json::Value::as_str).unwrap_or("package");
        let version =
            manifest.value().get("version").and_then(serde_json::Value::as_str).unwrap_or("0.0.0");

        let mut command = Command::new("npm");
        command.arg("publish");
        if self.dry_run {
            command.arg("--dry-run");
        }
        if self.json {
            command.arg("--json");
        }
        if self.ignore_scripts {
            command.arg("--ignore-scripts");
        }
        if self.force {
            command.arg("--force");
        }
        if let Some(tag) = &self.tag {
            command.args(["--tag", tag]);
        }
        if let Some(access) = &self.access {
            command.args(["--access", access]);
        }
        if let Some(target) = &self.target {
            command.arg(resolve_target(dir, target));
        }
        command.current_dir(dir);

        let status = command.status().into_diagnostic().wrap_err("run npm publish")?;
        if !status.success() {
            miette::bail!("publish failed with exit code {}", status.code().unwrap_or(1));
        }

        if self.dry_run && !self.json {
            println!("Prepared publish for {package_name}@{version}");
        }
        Ok(())
    }
}

fn resolve_target(dir: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() { target.to_path_buf() } else { dir.join(target) }
}
