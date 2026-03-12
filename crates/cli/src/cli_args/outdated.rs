use crate::State;
use crate::cli_args::install::InstallDependencyOptions;
use clap::Args;
use glob::Pattern;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{ProjectSnapshot, ResolvedDependencyVersion, RootProjectSnapshot};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::{Package, PackageVersion};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct OutdatedArgs {
    /// Filter by package name or glob pattern.
    packages: Vec<String>,

    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    dependency_options: InstallDependencyOptions,

    /// Output format.
    #[arg(long, value_parser = ["table", "list", "json"], default_value = "table")]
    format: String,

    /// Print JSON output.
    #[arg(long)]
    json: bool,

    /// Print the outdated packages in a list instead of a table.
    #[arg(long = "no-table", default_value_t = false)]
    no_table: bool,

    /// Only show the latest version that still satisfies the declared range.
    #[arg(long)]
    compatible: bool,

    /// Include package details in the output.
    #[arg(long)]
    long: bool,

    /// Sort the output rows.
    #[arg(long, value_parser = ["name"], default_value = "name")]
    sort_by: String,

    /// Check for outdated dependencies in every workspace package.
    #[arg(short = 'r', long)]
    recursive: bool,
}

#[derive(Debug, Clone, Serialize)]
struct OutdatedEntry {
    package_name: String,
    dependency_type: String,
    current: Option<String>,
    wanted: String,
    latest: String,
    is_deprecated: bool,
    details: Option<String>,
    dependents: Vec<String>,
    dependent_packages: Vec<DependentPackage>,
}

#[derive(Debug, Clone, Serialize)]
struct DependentPackage {
    name: String,
    location: String,
}

struct OutdatedTarget {
    dependent_name: String,
    dependent_location: PathBuf,
    manifest: PackageManifest,
    project_snapshot: Option<ProjectSnapshot>,
}

impl OutdatedArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        let State {
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            ..
        } = state;
        let dependency_groups = self.dependency_options.dependency_groups().collect::<Vec<_>>();
        let filters = self
            .packages
            .iter()
            .map(|pattern| Pattern::new(pattern))
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()
            .wrap_err("parse outdated package filter")?;
        let format = if self.json {
            "json"
        } else if self.no_table {
            "list"
        } else {
            self.format.as_str()
        };

        let mut targets = BTreeMap::<String, OutdatedTarget>::new();
        targets.insert(
            lockfile_importer_id.clone(),
            OutdatedTarget {
                dependent_name: dependent_label(&manifest, &lockfile_importer_id),
                dependent_location: manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.clone()),
                manifest,
                project_snapshot: lockfile
                    .as_ref()
                    .and_then(|lockfile| {
                        select_project_snapshot(&lockfile.project_snapshot, &lockfile_importer_id)
                    })
                    .cloned(),
            },
        );
        if self.recursive {
            for info in workspace_packages.values() {
                let importer_id = to_importer_id(&lockfile_dir, &info.root_dir);
                let manifest = PackageManifest::from_path(info.root_dir.join("package.json"))
                    .wrap_err_with(|| {
                        format!("load workspace manifest: {}", info.root_dir.display())
                    })?;
                let dependent_name = dependent_label(&manifest, &importer_id);
                let project_snapshot = lockfile
                    .as_ref()
                    .and_then(|lockfile| {
                        select_project_snapshot(&lockfile.project_snapshot, &importer_id)
                    })
                    .cloned();
                targets.insert(
                    importer_id,
                    OutdatedTarget {
                        dependent_name,
                        dependent_location: info.root_dir.clone(),
                        manifest,
                        project_snapshot,
                    },
                );
            }
        }

        let mut outdated = BTreeMap::<(String, Option<String>, String), OutdatedEntry>::new();
        for target in targets.into_values() {
            for group in dependency_groups.iter().copied() {
                let group_name = dependency_group_name(group).to_string();
                let dependencies = target
                    .manifest
                    .dependencies([group])
                    .map(|(name, specifier)| (name.to_string(), specifier.to_string()))
                    .collect::<Vec<_>>();
                for (name, wanted) in dependencies {
                    if !filters.is_empty() && !filters.iter().any(|pattern| pattern.matches(&name))
                    {
                        continue;
                    }
                    if should_skip_specifier(&wanted) {
                        continue;
                    }

                    let registry = config.registry_for_package_name(&name);
                    let auth_header = config.auth_header_for_url(&format!("{registry}{name}"));
                    let package = Package::fetch_from_registry(
                        &name,
                        &http_client,
                        &registry,
                        auth_header.as_deref(),
                    )
                    .await
                    .wrap_err_with(|| format!("fetch metadata for {name}"))?;
                    let selected_version = if self.compatible {
                        package.pinned_version(&wanted).unwrap_or_else(|| package.latest())
                    } else {
                        package.latest()
                    };
                    let latest = selected_version.version.to_string();
                    let current = target
                        .project_snapshot
                        .as_ref()
                        .and_then(|snapshot| current_version(snapshot, group, &name));
                    let is_deprecated = selected_version.deprecated.is_some();
                    if current.as_deref().unwrap_or(&wanted) == latest && !is_deprecated {
                        continue;
                    }

                    let key = (name.clone(), current.clone(), group_name.clone());
                    outdated
                        .entry(key)
                        .and_modify(|entry| {
                            merge_dependent(
                                entry,
                                &target.dependent_name,
                                &target.dependent_location,
                            );
                        })
                        .or_insert_with(|| OutdatedEntry {
                            package_name: name,
                            dependency_type: group_name.clone(),
                            current,
                            wanted,
                            latest,
                            is_deprecated,
                            details: details_for_package(selected_version),
                            dependents: vec![target.dependent_name.clone()],
                            dependent_packages: vec![DependentPackage {
                                name: target.dependent_name.clone(),
                                location: target.dependent_location.display().to_string(),
                            }],
                        });
                }
            }
        }

        let outdated = outdated.into_values().collect::<Vec<_>>();
        match format {
            "table" => {
                let output = render_table(&outdated, self.recursive, self.long);
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            "json" => {
                let output = render_json(&outdated, self.recursive)
                    .into_diagnostic()
                    .wrap_err("serialize outdated packages")?;
                println!("{output}");
            }
            "list" => {
                let output = render_list(&outdated, self.recursive, self.long);
                if !output.is_empty() {
                    print!("{output}");
                }
            }
            _ => miette::bail!("Unsupported outdated format `{format}`"),
        }
        let _ = &self.sort_by;
        Ok(())
    }
}

fn select_project_snapshot<'a>(
    project_snapshot: &'a RootProjectSnapshot,
    importer_id: &str,
) -> Option<&'a ProjectSnapshot> {
    match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => Some(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(importer_id),
    }
}

fn current_version(
    project_snapshot: &ProjectSnapshot,
    group: DependencyGroup,
    package_name: &str,
) -> Option<String> {
    let dependency = project_snapshot.get_map_by_group(group)?.get(&package_name.parse().ok()?)?;
    Some(match &dependency.version {
        ResolvedDependencyVersion::PkgVerPeer(value) => value.version().to_string(),
        ResolvedDependencyVersion::PkgNameVerPeer(value) => value.suffix.version().to_string(),
        ResolvedDependencyVersion::Link(value) => value.clone(),
    })
}

fn display_package_name(entry: &OutdatedEntry) -> String {
    match entry.dependency_type.as_str() {
        "devDependencies" => format!("{} (dev)", entry.package_name),
        "optionalDependencies" => format!("{} (optional)", entry.package_name),
        _ => entry.package_name.clone(),
    }
}

fn display_current(entry: &OutdatedEntry) -> String {
    match entry.current.as_deref() {
        Some(current) if current != entry.wanted => format!("{current} (wanted {})", entry.wanted),
        Some(current) => current.to_string(),
        None => format!("missing (wanted {})", entry.wanted),
    }
}

fn display_latest(entry: &OutdatedEntry) -> String {
    if entry.is_deprecated { "Deprecated".to_string() } else { entry.latest.clone() }
}

fn render_table(entries: &[OutdatedEntry], recursive: bool, long: bool) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut rows = vec![vec!["Package".to_string(), "Current".to_string(), "Latest".to_string()]];
    if recursive {
        rows[0].push("Dependents".to_string());
    }
    if long {
        rows[0].push("Details".to_string());
    }
    for entry in entries {
        let mut row =
            vec![display_package_name(entry), display_current(entry), display_latest(entry)];
        if recursive {
            row.push(entry.dependents.join(", "));
        }
        if long {
            row.push(entry.details.clone().unwrap_or_default());
        }
        rows.push(row);
    }

    let widths = (0..rows[0].len())
        .map(|index| rows.iter().map(|row| row[index].chars().count()).max().unwrap_or_default())
        .collect::<Vec<_>>();

    let border = |left: char, fill: char, mid: char, right: char| -> String {
        let mut line = String::new();
        line.push(left);
        for (index, width) in widths.iter().enumerate() {
            if index > 0 {
                line.push(mid);
            }
            line.push_str(&std::iter::repeat_n(fill, width + 2).collect::<String>());
        }
        line.push(right);
        line
    };

    let render_row = |row: &[String]| -> String {
        let mut line = String::from("│");
        for (index, value) in row.iter().enumerate() {
            line.push(' ');
            line.push_str(value);
            line.push_str(&" ".repeat(widths[index].saturating_sub(value.chars().count()) + 1));
            line.push('│');
        }
        line
    };

    let mut output =
        vec![border('┌', '─', '┬', '┐'), render_row(&rows[0]), border('├', '─', '┼', '┤')];
    for (index, row) in rows.iter().enumerate().skip(1) {
        output.push(render_row(row));
        if index + 1 != rows.len() {
            output.push(border('├', '─', '┼', '┤'));
        }
    }
    output.push(border('└', '─', '┴', '┘'));
    output.join("\n")
}

fn render_list(entries: &[OutdatedEntry], recursive: bool, long: bool) -> String {
    let mut output = String::new();
    for entry in entries {
        output.push_str(&display_package_name(entry));
        output.push('\n');
        output.push_str(&format!("{} => {}\n", display_current(entry), display_latest(entry)));
        if recursive {
            let label = if entry.dependents.len() > 1 { "Dependents" } else { "Dependent" };
            output.push_str(&format!("{label}: {}\n", entry.dependents.join(", ")));
        }
        if long && let Some(details) = entry.details.as_deref() {
            output.push_str(details);
            output.push('\n');
        }
        output.push('\n');
    }
    output
}

fn render_json(entries: &[OutdatedEntry], recursive: bool) -> serde_json::Result<String> {
    let mut root = Map::<String, Value>::new();
    for entry in entries {
        let mut object = Map::<String, Value>::new();
        object.insert("current".to_string(), serialize_string_or_null(entry.current.as_deref()));
        object.insert("latest".to_string(), Value::String(entry.latest.clone()));
        object.insert("wanted".to_string(), Value::String(entry.wanted.clone()));
        object.insert("isDeprecated".to_string(), Value::Bool(entry.is_deprecated));
        object.insert("dependencyType".to_string(), Value::String(entry.dependency_type.clone()));
        if recursive {
            object.insert(
                "dependentPackages".to_string(),
                serde_json::to_value(&entry.dependent_packages)?,
            );
        }
        root.insert(entry.package_name.clone(), Value::Object(object));
    }
    serde_json::to_string_pretty(&Value::Object(root))
}

fn serialize_string_or_null(value: Option<&str>) -> Value {
    value.map(|value| Value::String(value.to_string())).unwrap_or(Value::Null)
}

fn details_for_package(version: &PackageVersion) -> Option<String> {
    version
        .deprecated
        .clone()
        .or_else(|| version.homepage.clone())
        .or_else(|| repository_url(version.repository.as_ref()))
}

fn repository_url(repository: Option<&Value>) -> Option<String> {
    let repository = repository?;
    repository.as_str().map(ToOwned::to_owned).or_else(|| {
        repository.as_object().and_then(|repository| {
            repository.get("url").and_then(Value::as_str).map(ToOwned::to_owned)
        })
    })
}

fn merge_dependent(entry: &mut OutdatedEntry, dependent_name: &str, dependent_location: &Path) {
    if !entry.dependents.iter().any(|existing| existing == dependent_name) {
        entry.dependents.push(dependent_name.to_string());
        entry.dependents.sort();
    }
    let location = dependent_location.display().to_string();
    if !entry
        .dependent_packages
        .iter()
        .any(|dependent| dependent.name == dependent_name && dependent.location == location)
    {
        entry
            .dependent_packages
            .push(DependentPackage { name: dependent_name.to_string(), location });
        entry.dependent_packages.sort_by(|left, right| {
            left.name.cmp(&right.name).then(left.location.cmp(&right.location))
        });
    }
}

fn should_skip_specifier(specifier: &str) -> bool {
    specifier.starts_with("workspace:")
        || specifier.starts_with("file:")
        || specifier.starts_with("link:")
        || specifier.starts_with("npm:")
        || specifier.contains("github:")
        || specifier.contains("git+")
}

fn dependent_label(manifest: &PackageManifest, importer_id: &str) -> String {
    manifest
        .value()
        .get("name")
        .and_then(|name| name.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| importer_id.to_string())
}

fn to_importer_id(lockfile_dir: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(lockfile_dir) else {
        return ".".to_string();
    };
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn dependency_group_name(group: DependencyGroup) -> &'static str {
    match group {
        DependencyGroup::Prod => "dependencies",
        DependencyGroup::Dev => "devDependencies",
        DependencyGroup::Optional => "optionalDependencies",
        DependencyGroup::Peer => "peerDependencies",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn entry(name: &str) -> OutdatedEntry {
        OutdatedEntry {
            package_name: name.to_string(),
            dependency_type: "dependencies".to_string(),
            current: Some("1.0.0".to_string()),
            wanted: "1.0.0".to_string(),
            latest: "2.0.0".to_string(),
            is_deprecated: false,
            details: None,
            dependents: vec!["workspace".to_string()],
            dependent_packages: vec![DependentPackage {
                name: "workspace".to_string(),
                location: "/repo".to_string(),
            }],
        }
    }

    #[test]
    fn render_json_uses_pnpm_object_shape() {
        let output = render_json(&[entry("pkg")], false).expect("render json");
        let value: Value = serde_json::from_str(&output).expect("parse json");

        assert_eq!(value["pkg"]["current"], "1.0.0");
        assert_eq!(value["pkg"]["latest"], "2.0.0");
        assert_eq!(value["pkg"]["wanted"], "1.0.0");
        assert_eq!(value["pkg"]["isDeprecated"], false);
        assert_eq!(value["pkg"]["dependencyType"], "dependencies");
    }

    #[test]
    fn render_json_includes_recursive_dependent_packages() {
        let output = render_json(&[entry("pkg")], true).expect("render json");
        let value: Value = serde_json::from_str(&output).expect("parse json");

        assert_eq!(value["pkg"]["dependentPackages"][0]["name"], "workspace");
        assert_eq!(value["pkg"]["dependentPackages"][0]["location"], "/repo");
    }

    #[test]
    fn deprecated_packages_render_as_deprecated_latest() {
        let mut entry = entry("pkg");
        entry.is_deprecated = true;

        assert_eq!(display_latest(&entry), "Deprecated");
    }

    #[test]
    fn details_prefer_deprecation_message_over_links() {
        let version = PackageVersion {
            name: "pkg".to_string(),
            version: "1.0.0".parse().expect("parse version"),
            dist: Default::default(),
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: Some("deprecated".to_string()),
            bin: None,
            homepage: Some("https://example.com/pkg".to_string()),
            repository: Some(serde_json::json!({"url":"git+https://github.com/example/pkg.git"})),
        };

        assert_eq!(details_for_package(&version).as_deref(), Some("deprecated"));
    }

    #[test]
    fn details_fall_back_to_homepage_then_repository() {
        let homepage_version = PackageVersion {
            name: "pkg".to_string(),
            version: "1.0.0".parse().expect("parse version"),
            dist: Default::default(),
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: None,
            bin: None,
            homepage: Some("https://example.com/pkg".to_string()),
            repository: Some(serde_json::json!({"url":"git+https://github.com/example/pkg.git"})),
        };
        let repository_version = PackageVersion { homepage: None, ..homepage_version.clone() };

        assert_eq!(
            details_for_package(&homepage_version).as_deref(),
            Some("https://example.com/pkg")
        );
        assert_eq!(
            details_for_package(&repository_version).as_deref(),
            Some("git+https://github.com/example/pkg.git")
        );
    }

    #[test]
    fn merge_dependent_keeps_unique_sorted_dependents() {
        let mut entry = entry("pkg");
        merge_dependent(&mut entry, "app", Path::new("/repo/packages/app"));
        merge_dependent(&mut entry, "workspace", Path::new("/repo"));

        assert_eq!(entry.dependents, vec!["app".to_string(), "workspace".to_string()]);
        assert_eq!(entry.dependent_packages.len(), 2);
    }

    #[test]
    fn render_json_without_entries_is_empty_object() {
        let output = render_json(&[], false).expect("render json");
        assert_eq!(output, "{}");
    }

    #[test]
    fn sort_key_is_name_only_for_now() {
        let mut names = BTreeSet::new();
        names.insert(entry("b").package_name);
        names.insert(entry("a").package_name);

        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec!["a".to_string(), "b".to_string()]);
    }
}
