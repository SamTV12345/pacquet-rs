use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

#[derive(Debug, Args, Default)]
pub struct LicensesArgs {
    /// Subcommand, either `ls` or `list`.
    subcommand: Option<String>,

    /// Show more details such as package path.
    #[arg(long)]
    long: bool,

    /// Show information in JSON format.
    #[arg(long)]
    json: bool,

    /// Check only dependencies and optionalDependencies.
    #[arg(short = 'P', long = "prod")]
    prod: bool,

    /// Check only devDependencies.
    #[arg(short = 'D', long = "dev")]
    dev: bool,

    /// Don't check optionalDependencies.
    #[arg(long = "no-optional")]
    no_optional: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct LicenseRow {
    name: String,
    version: String,
    license: String,
    path: String,
    repository: Option<String>,
}

impl LicensesArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        match self.subcommand.as_deref() {
            Some("ls") | Some("list") => {}
            None => miette::bail!("Please specify the subcommand"),
            Some(_) => miette::bail!("This subcommand is not known"),
        }

        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let rows = collect_license_rows(
            &project_dir,
            &manifest_path,
            self.prod,
            self.dev,
            self.no_optional,
        )?;

        if rows.is_empty() {
            println!("No licenses in packages found");
            return Ok(());
        }

        if self.json {
            let rendered =
                serde_json::to_string_pretty(&rows).into_diagnostic().wrap_err("serialize JSON")?;
            println!("{rendered}");
            return Ok(());
        }

        for row in rows {
            if self.long {
                println!(
                    "{}@{}  {}  {}{}",
                    row.name,
                    row.version,
                    row.license,
                    row.path,
                    row.repository
                        .as_deref()
                        .map(|repository| format!("  {repository}"))
                        .unwrap_or_default()
                );
            } else {
                println!("{}@{}  {}", row.name, row.version, row.license);
            }
        }
        Ok(())
    }
}

fn collect_license_rows(
    project_dir: &Path,
    manifest_path: &Path,
    prod: bool,
    dev: bool,
    no_optional: bool,
) -> miette::Result<Vec<LicenseRow>> {
    let manifest =
        pacquet_package_manifest::PackageManifest::from_path(manifest_path.to_path_buf())
            .wrap_err("load package.json")?;

    let groups = if dev && !prod {
        vec![pacquet_package_manifest::DependencyGroup::Dev]
    } else if prod && !dev {
        let mut groups = vec![pacquet_package_manifest::DependencyGroup::Prod];
        if !no_optional {
            groups.push(pacquet_package_manifest::DependencyGroup::Optional);
        }
        groups
    } else {
        let mut groups = vec![
            pacquet_package_manifest::DependencyGroup::Prod,
            pacquet_package_manifest::DependencyGroup::Dev,
        ];
        if !no_optional {
            groups.push(pacquet_package_manifest::DependencyGroup::Optional);
        }
        groups
    };

    let mut queue =
        manifest.dependencies(groups).map(|(name, _)| name.to_string()).collect::<VecDeque<_>>();
    let mut seen = BTreeSet::<String>::new();
    let mut rows = BTreeMap::<(String, String), LicenseRow>::new();

    while let Some(package_name) = queue.pop_front() {
        if !seen.insert(package_name.clone()) {
            continue;
        }
        let Some(package_manifest_path) = installed_manifest_path(project_dir, &package_name)
        else {
            continue;
        };
        let package_manifest =
            pacquet_package_manifest::PackageManifest::from_path(package_manifest_path.clone())
                .wrap_err_with(|| format!("load {}", package_manifest_path.display()))?;
        let value = package_manifest.value();
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(package_name.as_str())
            .to_string();
        let version =
            value.get("version").and_then(serde_json::Value::as_str).unwrap_or("0.0.0").to_string();
        let license = license_label(value);
        let repository = repository_label(value);
        rows.entry((name.clone(), version.clone())).or_insert_with(|| LicenseRow {
            name,
            version,
            license,
            path: package_manifest_path.display().to_string(),
            repository,
        });

        for dependency_name in package_manifest
            .dependencies([
                pacquet_package_manifest::DependencyGroup::Prod,
                pacquet_package_manifest::DependencyGroup::Optional,
            ])
            .map(|(dependency_name, _)| dependency_name.to_string())
        {
            if !seen.contains(&dependency_name) {
                queue.push_back(dependency_name);
            }
        }
    }

    Ok(rows.into_values().collect())
}

fn installed_manifest_path(project_dir: &Path, dependency_name: &str) -> Option<PathBuf> {
    let mut path = project_dir.join("node_modules");
    for segment in dependency_name.split('/') {
        path.push(segment);
    }
    let manifest_path = path.join("package.json");
    manifest_path.is_file().then_some(manifest_path)
}

fn license_label(value: &serde_json::Value) -> String {
    value
        .get("license")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value.get("licenses").and_then(serde_json::Value::as_array).and_then(|licenses| {
                let items = licenses
                    .iter()
                    .filter_map(|license| {
                        license
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| license.as_str())
                    })
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                (!items.is_empty()).then(|| items.join(", "))
            })
        })
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

fn repository_label(value: &serde_json::Value) -> Option<String> {
    value.get("repository").and_then(|repository| {
        repository.as_str().map(ToString::to_string).or_else(|| {
            repository.get("url").and_then(serde_json::Value::as_str).map(ToString::to_string)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{license_label, repository_label};
    use serde_json::json;

    #[test]
    fn license_label_prefers_license_then_licenses_array() {
        assert_eq!(license_label(&json!({ "license": "MIT" })), "MIT".to_string());
        assert_eq!(
            license_label(&json!({ "licenses": [{ "type": "Apache-2.0" }, { "type": "MIT" }] })),
            "Apache-2.0, MIT".to_string()
        );
        assert_eq!(license_label(&json!({})), "UNKNOWN".to_string());
    }

    #[test]
    fn repository_label_supports_string_and_object_forms() {
        assert_eq!(
            repository_label(&json!({ "repository": "https://example.test/repo" })),
            Some("https://example.test/repo".to_string())
        );
        assert_eq!(
            repository_label(&json!({ "repository": { "url": "https://example.test/repo" } })),
            Some("https://example.test/repo".to_string())
        );
    }
}
