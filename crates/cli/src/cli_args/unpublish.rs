use clap::Args;
use pacquet_npmrc::Npmrc;

use crate::cli_args::registry_client::RegistryClient;

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

        let client = RegistryClient::new(npmrc);
        let registry = client.registry_url(name);
        let url = match version {
            Some(v) => {
                format!(
                    "{registry}/{name}/-/{}-{v}.tgz",
                    name.split('/').next_back().unwrap_or(name)
                )
            }
            None => format!("{registry}/{name}/-rev/all"),
        };
        client.delete(&url)?;
        match version {
            Some(v) => println!("Unpublished {name}@{v}"),
            None => println!("Unpublished {name} (all versions)"),
        }
        Ok(())
    }
}
