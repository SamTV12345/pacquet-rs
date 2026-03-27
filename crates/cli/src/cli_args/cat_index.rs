use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::PackageFilesIndex;
use serde_json::Value;
use std::fs::File;

#[derive(Debug, Args)]
pub struct CatIndexArgs {
    /// Package selector in the form <name>@<version>.
    package: String,
}

impl CatIndexArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let (name, version) = parse_package_selector(&self.package)
            .ok_or_else(|| miette::miette!("Cannot parse the `{}` selector", self.package))?;

        for path in npmrc.store_dir.index_file_paths() {
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let Ok(index) = serde_json::from_reader::<_, PackageFilesIndex>(file) else {
                continue;
            };
            if index.name.as_deref() != Some(name) || index.version.as_deref() != Some(version) {
                continue;
            }

            let file = File::open(&path)
                .into_diagnostic()
                .wrap_err_with(|| format!("open {}", path.display()))?;
            let value: Value = serde_json::from_reader(file)
                .into_diagnostic()
                .wrap_err("parse store index json")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&value)
                    .into_diagnostic()
                    .wrap_err("serialize store index json")?
            );
            return Ok(());
        }

        miette::bail!(
            "No corresponding index file found. You can use `pacquet list` to see if the package is installed."
        )
    }
}

fn parse_package_selector(selector: &str) -> Option<(&str, &str)> {
    let separator = if let Some(stripped) = selector.strip_prefix('@') {
        stripped.rfind('@').map(|index| index + 1)
    } else {
        selector.rfind('@')
    }?;
    let (name, rest) = selector.split_at(separator);
    let version = rest.get(1..)?;
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

#[cfg(test)]
mod tests {
    use super::parse_package_selector;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_package_selector_extracts_name_and_version() {
        assert_eq!(parse_package_selector("fastify@4.0.0"), Some(("fastify", "4.0.0")));
        assert_eq!(parse_package_selector("@scope/pkg@1.2.3"), Some(("@scope/pkg", "1.2.3")));
    }

    #[test]
    fn parse_package_selector_rejects_invalid_values() {
        assert_eq!(parse_package_selector("fastify"), None);
        assert_eq!(parse_package_selector("@scope/pkg"), None);
    }
}
