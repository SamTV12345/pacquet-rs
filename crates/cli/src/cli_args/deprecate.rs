use clap::Args;
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

/// Deprecate a version of a package.
#[derive(Debug, Args)]
pub struct DeprecateArgs {
    /// Package@version to deprecate.
    package_version: String,
    /// Deprecation message (empty string to undeprecate).
    message: String,
}

impl DeprecateArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let (package, version_range) = self
            .package_version
            .rsplit_once('@')
            .ok_or_else(|| miette::miette!("Expected format: package@version-range"))?;
        let client = RegistryClient::new(npmrc);
        let registry = client.registry_url(package);
        let url = format!("{registry}/{package}");

        let mut doc = client.get_json(&url)?;

        if let Some(versions) = doc.get_mut("versions").and_then(Value::as_object_mut) {
            for (ver, ver_data) in versions.iter_mut() {
                if version_matches(ver, version_range)
                    && let Some(obj) = ver_data.as_object_mut()
                {
                    if self.message.is_empty() {
                        obj.remove("deprecated");
                    } else {
                        obj.insert("deprecated".to_string(), Value::String(self.message.clone()));
                    }
                }
            }
        }

        client.put_json(&url, &doc)?;
        if self.message.is_empty() {
            println!("Undeprecated {}", self.package_version);
        } else {
            println!("Deprecated {}: {}", self.package_version, self.message);
        }
        Ok(())
    }
}

fn version_matches(version: &str, range: &str) -> bool {
    if range == "*" || range == version {
        return true;
    }
    // Simple prefix matching for basic ranges
    if let Some(prefix) = range.strip_suffix(".x") {
        return version.starts_with(prefix);
    }
    version == range
}
