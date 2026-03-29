use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::process::Command;

/// Remove a package version from the registry.
#[derive(Debug, Args)]
pub struct UnpublishArgs {
    /// Package or package@version to remove.
    package: String,
    /// Allow unpublishing without confirmation.
    #[arg(long)]
    force: bool,
}

impl UnpublishArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let (name, version) = if let Some((n, v)) = self.package.rsplit_once('@') {
            if n.is_empty() || n.starts_with('@') && !n.contains('/') {
                (self.package.as_str(), None)
            } else {
                (n, Some(v))
            }
        } else {
            (self.package.as_str(), None)
        };

        if version.is_none() && !self.force {
            miette::bail!(
                "Refusing to unpublish entire package without --force. \
                 Use `pacquet unpublish {}@<version>` to remove a specific version.",
                name
            );
        }

        let registry = npmrc.registry_for_package_name(name);
        let registry = registry.trim_end_matches('/');
        let url = match version {
            Some(v) => {
                format!("{registry}/{name}/-/{}-{v}.tgz", name.split('/').next_back().unwrap_or(name))
            }
            None => format!("{registry}/{name}/-rev/all"),
        };
        let auth = npmrc
            .auth_header_for_url(&url)
            .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;

        let output = Command::new("curl")
            .args(["-s", "-X", "DELETE", "-H", &format!("Authorization: {auth}"), &url])
            .output()
            .into_diagnostic()
            .wrap_err("unpublish")?;
        if !output.status.success() {
            let body = String::from_utf8_lossy(&output.stdout);
            miette::bail!("Unpublish failed: {body}");
        }
        match version {
            Some(v) => println!("Unpublished {name}@{v}"),
            None => println!("Unpublished {name} (all versions)"),
        }
        Ok(())
    }
}
