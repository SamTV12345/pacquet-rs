use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(Debug, Args)]
pub struct VersionCmdArgs {
    /// Version bump: major, minor, patch, premajor, preminor, prepatch, prerelease, or an explicit semver.
    version: Option<String>,

    /// Prevent git tagging.
    #[arg(long)]
    no_git_tag_version: bool,

    /// Disable commit hooks.
    #[arg(long)]
    no_commit_hooks: bool,
}

impl VersionCmdArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let content = fs::read_to_string(&manifest_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
        let mut manifest: Value =
            serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;

        let current_version =
            manifest.get("version").and_then(Value::as_str).unwrap_or("0.0.0").to_string();

        let Some(bump) = &self.version else {
            println!("{current_version}");
            return Ok(());
        };

        let new_version = compute_new_version(&current_version, bump)?;
        manifest
            .as_object_mut()
            .ok_or_else(|| miette::miette!("package.json must be an object"))?
            .insert("version".to_string(), Value::String(new_version.clone()));

        let rendered = serde_json::to_string_pretty(&manifest)
            .into_diagnostic()
            .wrap_err("serialize package.json")?;
        fs::write(&manifest_path, format!("{rendered}\n"))
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", manifest_path.display()))?;

        println!("v{new_version}");
        Ok(())
    }
}

fn compute_new_version(current: &str, bump: &str) -> miette::Result<String> {
    let parts: Vec<u64> = current
        .split('-')
        .next()
        .unwrap_or(current)
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (major, minor, patch) = (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    match bump {
        "major" => Ok(format!("{}.0.0", major + 1)),
        "minor" => Ok(format!("{major}.{}.0", minor + 1)),
        "patch" => Ok(format!("{major}.{minor}.{}", patch + 1)),
        "premajor" => Ok(format!("{}.0.0-0", major + 1)),
        "preminor" => Ok(format!("{major}.{}.0-0", minor + 1)),
        "prepatch" => Ok(format!("{major}.{minor}.{}-0", patch + 1)),
        "prerelease" => {
            if current.contains('-') {
                let pre_parts: Vec<&str> = current.splitn(2, '-').collect();
                let pre = pre_parts.get(1).unwrap_or(&"0");
                let pre_num: u64 = pre.parse().unwrap_or(0);
                Ok(format!("{major}.{minor}.{patch}-{}", pre_num + 1))
            } else {
                Ok(format!("{major}.{minor}.{}-0", patch + 1))
            }
        }
        explicit if explicit.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            Ok(explicit.to_string())
        }
        _ => miette::bail!(
            "Invalid version bump: {bump}. Use major, minor, patch, premajor, preminor, prepatch, prerelease, or an explicit version."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::compute_new_version;

    #[test]
    fn bumps() {
        assert_eq!(compute_new_version("1.2.3", "major").unwrap(), "2.0.0");
        assert_eq!(compute_new_version("1.2.3", "minor").unwrap(), "1.3.0");
        assert_eq!(compute_new_version("1.2.3", "patch").unwrap(), "1.2.4");
        assert_eq!(compute_new_version("1.2.3", "premajor").unwrap(), "2.0.0-0");
        assert_eq!(compute_new_version("1.2.3-0", "prerelease").unwrap(), "1.2.3-1");
        assert_eq!(compute_new_version("1.2.3", "4.0.0").unwrap(), "4.0.0");
    }
}
