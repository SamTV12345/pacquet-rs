use crate::cli_args::install::{InstallArgs, InstallDependencyOptions};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::PreferredVersions;
use serde_json::Value as JsonValue;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args, Default)]
pub struct ImportArgs;

impl ImportArgs {
    pub async fn run(self, dir: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        let detected = detect_legacy_lockfile(&dir).ok_or_else(|| {
            miette::miette!("No package-lock.json, npm-shrinkwrap.json, or yarn.lock found")
        })?;
        let preferred_versions = read_preferred_versions(&dir, detected)?;

        let pnpm_lock = dir.join("pnpm-lock.yaml");
        if pnpm_lock.is_file() {
            fs::remove_file(&pnpm_lock)
                .into_diagnostic()
                .wrap_err_with(|| format!("remove {}", pnpm_lock.display()))?;
        }

        println!("Importing dependency graph hints from {detected}");
        let state =
            crate::State::init(dir.join("package.json"), npmrc).wrap_err("initialize the state")?;
        let recursive = state.lockfile_importer_id == "." && !state.workspace_packages.is_empty();
        InstallArgs {
            dependency_options: InstallDependencyOptions::default(),
            frozen_lockfile: false,
            prefer_frozen_lockfile: false,
            no_prefer_frozen_lockfile: true,
            fix_lockfile: false,
            ignore_scripts: false,
            lockfile_only: true,
            force: false,
            resolution_only: false,
            ignore_pnpmfile: false,
            pnpmfile: None,
            reporter: None,
            use_store_server: false,
            shamefully_hoist: false,
            filter: Vec::new(),
            recursive,
            prefer_offline: false,
            offline: false,
        }
        .run_with_preferred_versions(state, Some(preferred_versions))
        .await
    }
}

fn detect_legacy_lockfile(dir: &Path) -> Option<&'static str> {
    ["package-lock.json", "npm-shrinkwrap.json", "yarn.lock"]
        .into_iter()
        .find(|file_name| dir.join(file_name).is_file())
}

fn read_preferred_versions(dir: &Path, detected: &str) -> miette::Result<PreferredVersions> {
    match detected {
        "package-lock.json" | "npm-shrinkwrap.json" => {
            read_npm_preferred_versions(&dir.join(detected))
        }
        "yarn.lock" => read_yarn_preferred_versions(&dir.join(detected)),
        _ => Ok(PreferredVersions::new()),
    }
}

fn read_npm_preferred_versions(path: &Path) -> miette::Result<PreferredVersions> {
    let text = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    let value: JsonValue = serde_json::from_str(&text)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse {}", path.display()))?;
    let mut preferred_versions = PreferredVersions::new();
    collect_npm_versions(&value, &mut preferred_versions);
    Ok(preferred_versions)
}

fn collect_npm_versions(value: &JsonValue, preferred_versions: &mut PreferredVersions) {
    let Some(object) = value.as_object() else {
        return;
    };

    if let Some(packages) = object.get("packages").and_then(JsonValue::as_object) {
        for (package_path, package_value) in packages {
            let Some(package_object) = package_value.as_object() else {
                continue;
            };
            let package_name = package_object
                .get("name")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
                .or_else(|| package_name_from_npm_package_path(package_path));
            let version = package_object.get("version").and_then(JsonValue::as_str);
            if let (Some(package_name), Some(version)) = (package_name, version) {
                insert_preferred_version(preferred_versions, &package_name, version);
            }
            collect_npm_versions(package_value, preferred_versions);
        }
    }

    if let Some(dependencies) = object.get("dependencies").and_then(JsonValue::as_object) {
        for (package_name, dependency_value) in dependencies {
            match dependency_value {
                JsonValue::String(version) => {
                    insert_preferred_version(preferred_versions, package_name, version);
                }
                JsonValue::Object(dependency_object) => {
                    if let Some(version) =
                        dependency_object.get("version").and_then(JsonValue::as_str)
                    {
                        insert_preferred_version(preferred_versions, package_name, version);
                    }
                    collect_npm_versions(dependency_value, preferred_versions);
                }
                _ => {}
            }
        }
    }
}

fn package_name_from_npm_package_path(package_path: &str) -> Option<String> {
    if !package_path.contains("node_modules/") {
        return None;
    }
    Some(package_path.rsplit("node_modules/").next()?.replace('\\', "/"))
}

fn read_yarn_preferred_versions(path: &Path) -> miette::Result<PreferredVersions> {
    let text = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    let mut preferred_versions = PreferredVersions::new();
    let mut current_selectors = Vec::<String>::new();
    let mut current_version = None::<String>;

    for line in text.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() || trimmed_end.starts_with('#') {
            continue;
        }
        if is_yarn_entry_header(line, trimmed_end) {
            flush_yarn_entry(&mut preferred_versions, &mut current_selectors, &mut current_version);
            current_selectors = parse_yarn_selectors(trimmed_end);
            continue;
        }
        if let Some(version) = parse_yarn_version_line(trimmed_end.trim()) {
            current_version = Some(version);
        }
    }
    flush_yarn_entry(&mut preferred_versions, &mut current_selectors, &mut current_version);

    Ok(preferred_versions)
}

fn is_yarn_entry_header(line: &str, trimmed_end: &str) -> bool {
    !line.starts_with(' ') && !line.starts_with('\t') && trimmed_end.ends_with(':')
}

fn parse_yarn_selectors(header: &str) -> Vec<String> {
    header
        .trim_end_matches(':')
        .split(',')
        .map(|selector| selector.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|selector| !selector.is_empty())
        .collect()
}

fn parse_yarn_version_line(line: &str) -> Option<String> {
    if let Some(version) = line.strip_prefix("version ") {
        return Some(version.trim().trim_matches('"').trim_matches('\'').to_string());
    }
    if let Some(version) = line.strip_prefix("version:") {
        return Some(version.trim().trim_matches('"').trim_matches('\'').to_string());
    }
    None
}

fn flush_yarn_entry(
    preferred_versions: &mut PreferredVersions,
    selectors: &mut Vec<String>,
    version: &mut Option<String>,
) {
    let Some(version) = version.take() else {
        selectors.clear();
        return;
    };
    for selector in selectors.drain(..) {
        if let Some(package_name) = package_name_from_selector(&selector) {
            insert_preferred_version(preferred_versions, &package_name, &version);
        }
    }
}

fn package_name_from_selector(selector: &str) -> Option<String> {
    let selector = selector.trim().trim_matches('"').trim_matches('\'');
    if let Some((package_name, _)) = selector.split_once("@npm:") {
        return Some(package_name.to_string());
    }
    if let Some(rest) = selector.strip_prefix('@')
        && let Some((scope, tail)) = rest.split_once('/')
        && let Some((name, _)) = tail.rsplit_once('@')
    {
        return Some(format!("@{scope}/{name}"));
    }
    selector.split_once('@').map(|(package_name, _)| package_name.to_string())
}

fn insert_preferred_version(
    preferred_versions: &mut PreferredVersions,
    package_name: &str,
    version: &str,
) {
    if package_name.is_empty() || version.is_empty() {
        return;
    }
    preferred_versions.entry(package_name.to_string()).or_default().insert(version.to_string());
}

#[cfg(test)]
mod tests {
    use super::{
        collect_npm_versions, package_name_from_selector, parse_yarn_selectors,
        parse_yarn_version_line, read_yarn_preferred_versions,
    };
    use pacquet_package_manager::PreferredVersions;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn npm_lockfile_reader_collects_versions_from_packages_and_dependencies() {
        let value = serde_json::json!({
            "lockfileVersion": 3,
            "packages": {
                "": { "name": "app", "version": "1.0.0" },
                "node_modules/foo": { "version": "1.2.3" },
                "node_modules/@scope/bar": { "version": "4.5.6" }
            },
            "dependencies": {
                "baz": { "version": "7.8.9" }
            }
        });

        let mut preferred_versions = PreferredVersions::new();
        collect_npm_versions(&value, &mut preferred_versions);

        assert_eq!(
            preferred_versions.get("foo").expect("foo versions"),
            &std::collections::HashSet::from(["1.2.3".to_string()])
        );
        assert_eq!(
            preferred_versions.get("@scope/bar").expect("bar versions"),
            &std::collections::HashSet::from(["4.5.6".to_string()])
        );
        assert_eq!(
            preferred_versions.get("baz").expect("baz versions"),
            &std::collections::HashSet::from(["7.8.9".to_string()])
        );
    }

    #[test]
    fn yarn_helpers_parse_v1_and_v2_entries() {
        assert_eq!(
            parse_yarn_selectors("\"foo@^1.0.0\", \"@scope/bar@npm:^2.0.0\":"),
            vec!["foo@^1.0.0".to_string(), "@scope/bar@npm:^2.0.0".to_string()]
        );
        assert_eq!(parse_yarn_version_line("version \"1.2.3\""), Some("1.2.3".to_string()));
        assert_eq!(parse_yarn_version_line("version: 4.5.6"), Some("4.5.6".to_string()));
        assert_eq!(package_name_from_selector("foo@npm:^1.0.0"), Some("foo".to_string()));
        assert_eq!(package_name_from_selector("@scope/bar@^2.0.0"), Some("@scope/bar".to_string()));
    }

    #[test]
    fn yarn_lockfile_reader_collects_versions() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("yarn.lock");
        fs::write(
            &path,
            "\"foo@^1.0.0\":\n  version \"1.2.3\"\n\"@scope/bar@npm:^2.0.0\":\n  version: 2.4.6\n",
        )
        .expect("write yarn lock");

        let preferred_versions = read_yarn_preferred_versions(&path).expect("read yarn lock");
        assert_eq!(
            preferred_versions.get("foo").expect("foo versions"),
            &std::collections::HashSet::from(["1.2.3".to_string()])
        );
        assert_eq!(
            preferred_versions.get("@scope/bar").expect("bar versions"),
            &std::collections::HashSet::from(["2.4.6".to_string()])
        );
    }
}
