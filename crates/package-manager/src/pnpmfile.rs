use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{Lockfile, lockfile_from_json_value, lockfile_to_json_value};
use pacquet_package_manifest::DependencyGroup;
use pacquet_registry::PackageVersion;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn apply_read_package_hook_to_manifest(
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
    manifest: &Value,
) -> miette::Result<Value> {
    apply_pnpmfile_hook(lockfile_dir, pnpmfile, ignore_pnpmfile, "readPackage", manifest)
}

pub(crate) fn apply_read_package_hook_to_package_version(
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
    package_version: &PackageVersion,
) -> miette::Result<PackageVersion> {
    apply_hook_to_serializable(
        lockfile_dir,
        pnpmfile,
        ignore_pnpmfile,
        "readPackage",
        package_version,
    )
    .wrap_err("parse package version returned by .pnpmfile.cjs hook")
}

pub(crate) fn apply_after_all_resolved_hook(
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
    lockfile: &Lockfile,
) -> miette::Result<Lockfile> {
    let value = lockfile_to_json_value(lockfile)
        .into_diagnostic()
        .wrap_err("serialize lockfile for .pnpmfile.cjs hook")?;
    let hooked =
        apply_pnpmfile_hook(lockfile_dir, pnpmfile, ignore_pnpmfile, "afterAllResolved", &value)?;
    lockfile_from_json_value(hooked)
        .into_diagnostic()
        .wrap_err("parse lockfile returned by .pnpmfile.cjs afterAllResolved hook")
}

pub(crate) fn dependencies_from_manifest_value(
    manifest: &Value,
    groups: impl IntoIterator<Item = DependencyGroup>,
) -> Vec<(String, String)> {
    dependencies_from_manifest_value_grouped(manifest, groups)
        .into_iter()
        .map(|(_, name, spec)| (name, spec))
        .collect()
}

pub(crate) fn dependencies_from_manifest_value_grouped(
    manifest: &Value,
    groups: impl IntoIterator<Item = DependencyGroup>,
) -> Vec<(DependencyGroup, String, String)> {
    groups
        .into_iter()
        .flat_map(|group| {
            manifest.get::<&str>(group.into()).and_then(Value::as_object).into_iter().flat_map(
                move |dependencies| {
                    dependencies.iter().filter_map(move |(name, spec)| {
                        spec.as_str().map(|spec| (group, name.clone(), spec.to_string()))
                    })
                },
            )
        })
        .collect()
}

/// Resolve a `catalog:` specifier to the actual version range from the
/// workspace catalogs. Returns the input unchanged if not a catalog specifier.
///
/// Formats:
/// - `catalog:` or `catalog:default` → look up in the default catalog
/// - `catalog:<name>` → look up in the named catalog
pub(crate) fn resolve_catalog_specifier(
    spec: &str,
    package_name: &str,
    catalogs: &HashMap<String, HashMap<String, String>>,
) -> Result<String, String> {
    let remainder = match spec.strip_prefix("catalog:") {
        Some(r) => r,
        None => return Ok(spec.to_string()),
    };

    let catalog_name =
        if remainder.is_empty() || remainder == "default" { "default" } else { remainder };

    catalogs
        .get(catalog_name)
        .and_then(|catalog| catalog.get(package_name))
        .cloned()
        .ok_or_else(|| format!("Missing version catalog: {catalog_name} on package {package_name}"))
}

/// Load catalogs from `pnpm-workspace.yaml`.
///
/// Supports:
/// ```yaml
/// catalog:          # implicit "default" catalog
///   react: ^18.0.0
/// catalogs:         # named catalogs
///   default:
///     react: ^18.0.0
///   legacy:
///     old-lib: 1.0.0
/// ```
pub(crate) fn load_catalogs_from_workspace(
    lockfile_dir: &Path,
) -> HashMap<String, HashMap<String, String>> {
    let workspace_yaml_path = lockfile_dir.join("pnpm-workspace.yaml");
    let content = match std::fs::read_to_string(&workspace_yaml_path) {
        Ok(content) => content,
        Err(_) => return HashMap::new(),
    };
    let yaml: serde_yaml::Value = match serde_yaml::from_str(&content) {
        Ok(yaml) => yaml,
        Err(_) => return HashMap::new(),
    };

    let mut catalogs = HashMap::<String, HashMap<String, String>>::new();

    // `catalog:` top-level key → implicit default catalog
    if let Some(serde_yaml::Value::Mapping(catalog)) = yaml.get("catalog") {
        let default = catalogs.entry("default".to_string()).or_default();
        for (name, spec) in catalog {
            if let (Some(name), Some(spec)) = (name.as_str(), spec.as_str()) {
                default.insert(name.to_string(), spec.to_string());
            }
        }
    }

    // `catalogs:` top-level key → named catalogs
    if let Some(serde_yaml::Value::Mapping(named)) = yaml.get("catalogs") {
        for (catalog_name, entries) in named {
            if let (Some(catalog_name), Some(serde_yaml::Value::Mapping(entries))) =
                (catalog_name.as_str(), Some(entries))
            {
                let catalog = catalogs.entry(catalog_name.to_string()).or_default();
                for (name, spec) in entries {
                    if let (Some(name), Some(spec)) = (name.as_str(), spec.as_str()) {
                        catalog.insert(name.to_string(), spec.to_string());
                    }
                }
            }
        }
    }

    catalogs
}

fn apply_hook_to_serializable<T>(
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
    hook_name: &str,
    value: &T,
) -> miette::Result<T>
where
    T: Serialize + DeserializeOwned,
{
    let value = serde_json::to_value(value)
        .into_diagnostic()
        .wrap_err_with(|| format!("serialize payload for .pnpmfile.cjs {hook_name} hook"))?;
    let hooked = apply_pnpmfile_hook(lockfile_dir, pnpmfile, ignore_pnpmfile, hook_name, &value)?;
    serde_json::from_value(hooked)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse payload returned by .pnpmfile.cjs {hook_name} hook"))
}

fn apply_pnpmfile_hook(
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
    hook_name: &str,
    payload: &Value,
) -> miette::Result<Value> {
    if ignore_pnpmfile {
        return Ok(payload.clone());
    }
    let pnpmfile_path = resolve_pnpmfile_path(lockfile_dir, pnpmfile);
    if !pnpmfile_path.is_file() {
        return Ok(payload.clone());
    }

    let output = run_pnpmfile_hook(&pnpmfile_path, hook_name, payload)?;
    for message in output.logs {
        crate::progress_reporter::log(&format!("{hook_name}: {message}"));
    }
    Ok(output.result.unwrap_or_else(|| payload.clone()))
}

pub(crate) fn resolve_pnpmfile_path(lockfile_dir: &Path, pnpmfile: Option<&Path>) -> PathBuf {
    match pnpmfile {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => lockfile_dir.join(path),
        None => lockfile_dir.join(".pnpmfile.cjs"),
    }
}

pub(crate) fn pnpmfile_exports_value(pnpmfile_path: &Path) -> miette::Result<Option<bool>> {
    if !pnpmfile_path.is_file() {
        return Ok(None);
    }

    let output = Command::new("node")
        .arg("-e")
        .arg(
            r#"
const pnpmfilePath = process.argv[1];
const loaded = require(pnpmfilePath);
process.stdout.write(typeof loaded === 'undefined' ? 'undefined' : 'defined');
"#,
        )
        .arg(pnpmfile_path)
        .output()
        .into_diagnostic()
        .wrap_err_with(|| format!("inspect {}", pnpmfile_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        miette::bail!("Failed to inspect pnpmfile {}: {}", pnpmfile_path.display(), stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Some(stdout.trim() == "defined"))
}

fn run_pnpmfile_hook(
    pnpmfile_path: &Path,
    hook_name: &str,
    payload: &Value,
) -> miette::Result<HookOutput> {
    let mut child = Command::new("node")
        .arg("-e")
        .arg(
            r#"
const fs = require('node:fs');
const path = require('node:path');

async function main () {
  const pnpmfilePath = process.argv[1];
  const hookName = process.argv[2];
  const source = fs.readFileSync(0, 'utf8');
  const payload = JSON.parse(source);
  const loaded = require(pnpmfilePath);

  if (typeof loaded === 'undefined') {
    process.stdout.write(JSON.stringify({ result: payload, logs: [] }));
    return;
  }

  const hook = loaded?.hooks?.[hookName];
  if (hook == null) {
    process.stdout.write(JSON.stringify({ result: payload, logs: [] }));
    return;
  }
  if (typeof hook !== 'function') {
    throw new TypeError(`hooks.${hookName} should be a function`);
  }

  const logs = [];
  const ctx = {
    log: (message) => {
      logs.push(String(message));
    },
    pnpmfileDir: path.dirname(pnpmfilePath),
  };

  if (hookName === 'readPackage') {
    payload.dependencies = payload.dependencies ?? {};
    payload.devDependencies = payload.devDependencies ?? {};
    payload.optionalDependencies = payload.optionalDependencies ?? {};
    payload.peerDependencies = payload.peerDependencies ?? {};
  }

  const result = await hook(payload, ctx);
  const finalResult = result ?? payload;
  if (hookName === 'readPackage') {
    if (typeof finalResult !== 'object' || finalResult == null || Array.isArray(finalResult)) {
      throw new Error('readPackage hook did not return a package manifest object.');
    }
    for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) {
      if (finalResult[field] != null && typeof finalResult[field] !== 'object') {
        throw new Error(`readPackage hook returned package manifest object's property '${field}' must be an object.`);
      }
    }
  }
  process.stdout.write(JSON.stringify({ result: finalResult, logs }));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
"#,
        )
        .arg(pnpmfile_path)
        .arg(hook_name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .into_diagnostic()
        .wrap_err_with(|| format!("spawn node for {}", pnpmfile_path.display()))?;

    let source = serde_json::to_vec(payload)
        .into_diagnostic()
        .wrap_err_with(|| format!("serialize payload for .pnpmfile.cjs {hook_name} hook"))?;
    child
        .stdin
        .as_mut()
        .expect("node stdin available")
        .write_all(&source)
        .into_diagnostic()
        .wrap_err_with(|| {
            format!("write payload to node stdin for .pnpmfile.cjs {hook_name} hook")
        })?;

    let output = child
        .wait_with_output()
        .into_diagnostic()
        .wrap_err_with(|| format!("wait for .pnpmfile.cjs {hook_name} hook node process"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        miette::bail!(".pnpmfile.cjs {hook_name} hook failed: {}", stderr.trim());
    }

    serde_json::from_slice(&output.stdout)
        .into_diagnostic()
        .wrap_err_with(|| format!(".pnpmfile.cjs {hook_name} hook returned invalid JSON"))
}

#[derive(Debug, Default, serde::Deserialize)]
struct HookOutput {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    logs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::{ComVer, ProjectSnapshot, RootProjectSnapshot};
    use serde_json::json;
    use tempfile::tempdir;

    fn write_pnpmfile(dir: &Path, source: &str) {
        std::fs::write(dir.join(".pnpmfile.cjs"), source).expect("write pnpmfile");
    }

    fn empty_lockfile() -> Lockfile {
        Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: None,
            never_built_dependencies: None,
            ignored_optional_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            extra_fields: Default::default(),
            project_snapshot: RootProjectSnapshot::Single(ProjectSnapshot::default()),
            packages: None,
        }
    }

    #[test]
    fn read_package_hook_can_mutate_dependencies() {
        let dir = tempdir().expect("create temp dir");
        write_pnpmfile(
            dir.path(),
            r#"
module.exports = {
  hooks: {
    readPackage (pkg) {
      pkg.dependencies = { ...(pkg.dependencies ?? {}), addedByHook: "1.2.3" };
      return pkg;
    }
  }
};
"#,
        );

        let hooked = apply_read_package_hook_to_manifest(
            dir.path(),
            None,
            false,
            &json!({
                "name": "app",
                "version": "1.0.0",
                "dependencies": {
                    "left-pad": "1.3.0"
                }
            }),
        )
        .expect("apply hook");

        let deps = dependencies_from_manifest_value(&hooked, [DependencyGroup::Prod]);
        assert!(deps.iter().any(|(name, spec)| name == "left-pad" && spec == "1.3.0"));
        assert!(deps.iter().any(|(name, spec)| name == "addedByHook" && spec == "1.2.3"));
    }

    #[test]
    fn ignore_pnpmfile_skips_hook_execution() {
        let dir = tempdir().expect("create temp dir");
        write_pnpmfile(
            dir.path(),
            r#"module.exports = { hooks: { readPackage (pkg) { pkg.dependencies = { changed: "1.0.0" }; return pkg } } };"#,
        );

        let hooked = apply_read_package_hook_to_manifest(
            dir.path(),
            None,
            true,
            &json!({
                "name": "app",
                "version": "1.0.0"
            }),
        )
        .expect("apply hook");

        assert!(hooked.get("dependencies").is_none());
    }

    #[test]
    fn after_all_resolved_hook_can_add_top_level_field() {
        let dir = tempdir().expect("create temp dir");
        write_pnpmfile(
            dir.path(),
            r#"
module.exports = {
  hooks: {
    afterAllResolved (lockfile) {
      lockfile.foo = "bar";
      return lockfile;
    }
  }
};
"#,
        );

        let hooked = apply_after_all_resolved_hook(dir.path(), None, false, &empty_lockfile())
            .expect("apply hook");

        assert_eq!(
            hooked.extra_fields.get("foo"),
            Some(&serde_yaml::Value::String("bar".to_string()))
        );
    }

    #[test]
    fn pnpmfile_context_log_is_reported() {
        let dir = tempdir().expect("create temp dir");
        write_pnpmfile(
            dir.path(),
            r#"
module.exports = {
  hooks: {
    readPackage (pkg, ctx) {
      ctx.log("hello from hook");
      return pkg;
    }
  }
};
"#,
        );

        let output = run_pnpmfile_hook(
            &dir.path().join(".pnpmfile.cjs"),
            "readPackage",
            &json!({ "name": "app" }),
        )
        .expect("run hook");

        assert_eq!(output.logs, vec!["hello from hook".to_string()]);
    }

    #[test]
    fn pnpmfile_exports_value_detects_undefined_export() {
        let dir = tempdir().expect("create temp dir");
        write_pnpmfile(dir.path(), "module.exports = undefined;\n");

        assert_eq!(
            pnpmfile_exports_value(&dir.path().join(".pnpmfile.cjs")).expect("inspect pnpmfile"),
            Some(false)
        );
    }
}
