use crate::State;
use crate::cli_args::rebuild::RebuildArgs;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args, Default)]
pub struct ApproveBuildsArgs {
    /// Approve dependencies of global packages.
    #[arg(short = 'g', long)]
    global: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModulesManifestLite {
    #[serde(default)]
    pending_builds: Vec<String>,
    #[serde(default)]
    ignored_builds: Vec<String>,
}

impl ApproveBuildsArgs {
    pub async fn run(self, manifest_path: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        let _ = self.global;
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let modules_manifest_path = project_dir.join("node_modules/.modules.yaml");
        let modules_manifest = read_modules_manifest(&modules_manifest_path)?;
        let pending = modules_manifest.pending_builds.into_iter().collect::<BTreeSet<_>>();

        if pending.is_empty() {
            println!("There are no packages awaiting approval");
            return Ok(());
        }

        let approved_packages = pending.into_iter().collect::<Vec<_>>();
        update_package_manifest(&manifest_path, &approved_packages)?;
        write_modules_manifest(
            &modules_manifest_path,
            ModulesManifestLite { pending_builds: Vec::new(), ignored_builds: Vec::new() },
        )?;

        let state = State::init(manifest_path.clone(), npmrc).wrap_err("initialize the state")?;
        RebuildArgs::from_packages(approved_packages.clone()).run(state).await?;

        println!("Approved build scripts for: {}", approved_packages.join(", "));
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

fn write_modules_manifest(path: &Path, manifest: ModulesManifestLite) -> miette::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", parent.display()))?;
    }
    let rendered = serde_json::to_string_pretty(&manifest)
        .into_diagnostic()
        .wrap_err("serialize .modules.yaml")?
        + "\n";
    fs::write(path, rendered)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", path.display()))
}

fn update_package_manifest(
    manifest_path: &Path,
    approved_packages: &[String],
) -> miette::Result<()> {
    let content = fs::read_to_string(manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let mut value: Value =
        serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    let pnpm = root.entry("pnpm").or_insert_with(|| Value::Object(Map::new()));
    let pnpm = pnpm
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;

    let mut only_built = read_string_array(pnpm.get("onlyBuiltDependencies"))?;
    only_built.extend(approved_packages.iter().cloned());
    pnpm.insert(
        "onlyBuiltDependencies".to_string(),
        Value::Array(
            only_built
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );

    let ignored = read_string_array(pnpm.get("ignoredBuiltDependencies"))?;
    let filtered_ignored = ignored
        .into_iter()
        .filter(|package| !approved_packages.contains(package))
        .map(Value::String)
        .collect::<Vec<_>>();
    if filtered_ignored.is_empty() {
        pnpm.remove("ignoredBuiltDependencies");
    } else {
        pnpm.insert("ignoredBuiltDependencies".to_string(), Value::Array(filtered_ignored));
    }

    if pnpm.is_empty() {
        root.remove("pnpm");
    }

    let rendered = serde_json::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("serialize package.json")?;
    fs::write(manifest_path, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", manifest_path.display()))
}

fn read_string_array(value: Option<&Value>) -> miette::Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            Ok(items.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
        }
        Some(_) => miette::bail!("expected array of strings"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ModulesManifestLite, read_string_array, update_package_manifest, write_modules_manifest,
    };
    use serde_json::json;
    use std::fs;

    #[test]
    fn update_package_manifest_merges_only_built_dependencies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(
            &manifest_path,
            json!({
                "name": "app",
                "pnpm": {
                    "onlyBuiltDependencies": ["esbuild"],
                    "ignoredBuiltDependencies": ["sharp"]
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        update_package_manifest(&manifest_path, &["sharp".to_string(), "fsevents".to_string()])
            .expect("update package manifest");

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read package.json"))
                .expect("parse package.json");
        let pnpm = value.get("pnpm").and_then(serde_json::Value::as_object).expect("pnpm object");
        assert_eq!(
            read_string_array(pnpm.get("onlyBuiltDependencies"))
                .expect("read onlyBuiltDependencies"),
            vec!["esbuild".to_string(), "fsevents".to_string(), "sharp".to_string()]
        );
        assert_eq!(
            read_string_array(pnpm.get("ignoredBuiltDependencies"))
                .expect("read ignoredBuiltDependencies"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn write_modules_manifest_clears_pending_and_ignored_builds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join(".modules.yaml");
        write_modules_manifest(
            &manifest_path,
            ModulesManifestLite { pending_builds: Vec::new(), ignored_builds: Vec::new() },
        )
        .expect("write modules manifest");

        let written = fs::read_to_string(&manifest_path).expect("read modules manifest");
        assert!(written.contains("\"pendingBuilds\": []"));
        assert!(written.contains("\"ignoredBuilds\": []"));
    }
}
