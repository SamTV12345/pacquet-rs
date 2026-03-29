use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

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
        let registry = npmrc.registry_for_package_name(package);
        let registry = registry.trim_end_matches('/');
        let url = format!("{registry}/{package}");
        let auth = npmrc
            .auth_header_for_url(&url)
            .ok_or_else(|| miette::miette!("Not authenticated. Run `pacquet login` first."))?;

        let output = Command::new("curl")
            .args([
                "-s",
                "-H",
                &format!("Authorization: {auth}"),
                "-H",
                "Accept: application/json",
                &url,
            ])
            .output()
            .into_diagnostic()
            .wrap_err("fetch package")?;
        let body = String::from_utf8_lossy(&output.stdout);
        let mut doc: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;

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

        let output = Command::new("curl")
            .args([
                "-s",
                "-X",
                "PUT",
                "-H",
                &format!("Authorization: {auth}"),
                "-H",
                "Content-Type: application/json",
                "-d",
                &serde_json::to_string(&doc).unwrap_or_default(),
                &url,
            ])
            .output()
            .into_diagnostic()
            .wrap_err("update package")?;
        if !output.status.success() {
            miette::bail!("Failed to deprecate {}", self.package_version);
        }
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
