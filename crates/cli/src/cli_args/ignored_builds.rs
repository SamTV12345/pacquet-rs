use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args, Default)]
pub struct IgnoredBuildsArgs;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModulesManifestLite {
    #[serde(default)]
    ignored_builds: Vec<String>,
}

impl IgnoredBuildsArgs {
    pub fn run(self, manifest_path: PathBuf) -> miette::Result<()> {
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let modules_manifest =
            read_modules_manifest(&project_dir.join("node_modules/.modules.yaml"))?;
        let explicit_ignored = read_explicit_ignored_builds(&manifest_path)?;
        let automatically_ignored = modules_manifest
            .ignored_builds
            .into_iter()
            .filter_map(|package| parse_package_name(&package))
            .filter(|package| !explicit_ignored.contains(package))
            .collect::<std::collections::BTreeSet<_>>();

        println!("Automatically ignored builds during installation:");
        match automatically_ignored.is_empty() {
            true => println!("  None"),
            false => {
                for package in automatically_ignored {
                    println!("  {package}");
                }
                println!(
                    "hint: To allow the execution of build scripts for a package, add its name to \"pnpm.onlyBuiltDependencies\" in your \"package.json\", then run \"pacquet rebuild\"."
                );
                println!(
                    "hint: If you don't want to build a package, add it to the \"pnpm.ignoredBuiltDependencies\" list."
                );
            }
        }

        if !explicit_ignored.is_empty() {
            println!();
            println!("Explicitly ignored package builds (via pnpm.ignoredBuiltDependencies):");
            for package in explicit_ignored {
                println!("  {package}");
            }
        }

        Ok(())
    }
}

fn read_modules_manifest(path: &Path) -> miette::Result<ModulesManifestLite> {
    if !path.is_file() {
        return Ok(ModulesManifestLite::default());
    }
    let content = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", path.display()))?;
    serde_yaml::from_str(&content).into_diagnostic().wrap_err("parse .modules.yaml")
}

fn read_explicit_ignored_builds(manifest_path: &Path) -> miette::Result<Vec<String>> {
    let content = fs::read_to_string(manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let value: Value =
        serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;
    Ok(value
        .get("pnpm")
        .and_then(|pnpm| pnpm.get("ignoredBuiltDependencies"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect())
}

fn parse_package_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('/') {
        return parse_slash_prefixed_dep_path(rest);
    }
    Some(trimmed.to_string())
}

fn parse_slash_prefixed_dep_path(rest: &str) -> Option<String> {
    if let Some(rest) = rest.strip_prefix('@') {
        let (scope, tail) = rest.split_once('/')?;
        let (name, _) = split_name_and_version(tail);
        return Some(format!("@{scope}/{name}"));
    }
    let (name, _) = split_name_and_version(rest);
    Some(name.to_string())
}

fn split_name_and_version(value: &str) -> (&str, Option<&str>) {
    match value.find('@') {
        Some(index) => (&value[..index], Some(&value[index + 1..])),
        None => (value, None),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_package_name;

    #[test]
    fn parse_package_name_supports_store_dep_paths() {
        assert_eq!(parse_package_name("/is-positive@3.1.0"), Some("is-positive".to_string()));
        assert_eq!(
            parse_package_name("/is-positive@3.1.0(peer@2.0.0)"),
            Some("is-positive".to_string())
        );
        assert_eq!(
            parse_package_name("/@scope/pkg@1.0.0(peer@2.0.0)"),
            Some("@scope/pkg".to_string())
        );
        assert_eq!(parse_package_name("left-pad"), Some("left-pad".to_string()));
    }
}
