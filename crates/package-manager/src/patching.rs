use crate::progress_reporter;
use miette::{Context, IntoDiagnostic};
use node_semver::{Range, Version};
use serde_json::Value as JsonValue;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectedPatch {
    pub key: String,
    pub file: PathBuf,
    pub strict: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PatchRange {
    version: String,
    patch: SelectedPatch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PatchGroup {
    exact: HashMap<String, SelectedPatch>,
    range: Vec<PatchRange>,
    all: Option<SelectedPatch>,
}

type RawPatchedDependencies = BTreeMap<String, String>;
type PatchGroups = HashMap<String, PatchGroup>;

pub(crate) fn manifest_patched_dependencies_for_lockfile(
    lockfile_dir: &Path,
) -> miette::Result<Option<HashMap<String, serde_yaml::Value>>> {
    let patched_dependencies = raw_patched_dependencies(lockfile_dir)?;
    if patched_dependencies.is_empty() {
        return Ok(None);
    }
    let _ = group_patched_dependencies(lockfile_dir, &patched_dependencies)?;
    Ok(Some(
        patched_dependencies
            .into_iter()
            .map(|(key, value)| (key, serde_yaml::Value::String(value)))
            .collect(),
    ))
}

pub(crate) fn selected_patch_for_package(
    lockfile_dir: &Path,
    package_name: &str,
    package_version: &str,
) -> miette::Result<Option<SelectedPatch>> {
    let patched_dependencies = raw_patched_dependencies(lockfile_dir)?;
    if patched_dependencies.is_empty() {
        return Ok(None);
    }
    let groups = group_patched_dependencies(lockfile_dir, &patched_dependencies)?;
    get_patch_info(&groups, package_name, package_version)
}

pub(crate) fn apply_patch_if_needed(
    lockfile_dir: &Path,
    package_name: &str,
    package_version: &str,
    package_dir: &Path,
) -> miette::Result<bool> {
    let Some(patch) = selected_patch_for_package(lockfile_dir, package_name, package_version)?
    else {
        return Ok(false);
    };

    let mut check = Command::new("git");
    check.current_dir(package_dir).args(["apply", "--check", "--unsafe-paths"]).arg(&patch.file);
    let check_output = check
        .output()
        .into_diagnostic()
        .wrap_err_with(|| format!("run git apply --check for {}", patch.file.display()))?;
    if !check_output.status.success() {
        let stderr = String::from_utf8_lossy(&check_output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!(
                "Could not apply patch {} to {}@{}",
                patch.file.display(),
                package_name,
                package_version
            )
        } else {
            format!(
                "Could not apply patch {} to {}@{}: {stderr}",
                patch.file.display(),
                package_name,
                package_version
            )
        };
        if patch.strict {
            return Err(miette::miette!("{message}"));
        }
        progress_reporter::warn(&message);
        return Ok(false);
    }

    let mut apply = Command::new("git");
    apply
        .current_dir(package_dir)
        .args(["apply", "--unsafe-paths", "--whitespace=nowarn"])
        .arg(&patch.file);
    let apply_output = apply
        .output()
        .into_diagnostic()
        .wrap_err_with(|| format!("apply patch {}", patch.file.display()))?;
    if !apply_output.status.success() {
        let stderr = String::from_utf8_lossy(&apply_output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(miette::miette!(
                "Could not apply patch {} to {}@{}",
                patch.file.display(),
                package_name,
                package_version
            ));
        }
        return Err(miette::miette!(
            "Could not apply patch {} to {}@{}: {}",
            patch.file.display(),
            package_name,
            package_version,
            stderr
        ));
    }

    Ok(true)
}

fn raw_patched_dependencies(lockfile_dir: &Path) -> miette::Result<RawPatchedDependencies> {
    let manifest_path = lockfile_dir.join("package.json");
    if !manifest_path.is_file() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let manifest: JsonValue = serde_json::from_str(&text)
        .into_diagnostic()
        .wrap_err_with(|| format!("parse {}", manifest_path.display()))?;
    let Some(patched_dependencies) =
        manifest.get("pnpm").and_then(|pnpm| pnpm.get("patchedDependencies"))
    else {
        return Ok(BTreeMap::new());
    };
    let patched_dependencies = patched_dependencies.as_object().ok_or_else(|| {
        miette::miette!("{}: pnpm.patchedDependencies must be an object", manifest_path.display())
    })?;
    patched_dependencies
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().ok_or_else(|| {
                miette::miette!(
                    "{}: pnpm.patchedDependencies[\"{}\"] must be a string",
                    manifest_path.display(),
                    key
                )
            })?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}

fn group_patched_dependencies(
    lockfile_dir: &Path,
    patched_dependencies: &RawPatchedDependencies,
) -> miette::Result<PatchGroups> {
    let mut result = PatchGroups::new();
    for (key, file) in patched_dependencies {
        let (name, selector) = parse_patched_dependency_key(key);
        let file = resolve_patch_file(lockfile_dir, file);
        match selector {
            Some(selector) => {
                let patch = SelectedPatch { key: key.clone(), file: file.clone(), strict: true };
                if selector.parse::<Version>().is_ok() {
                    result.entry(name).or_default().exact.insert(selector, patch);
                    continue;
                }

                selector.parse::<Range>().map_err(|_| {
                    miette::miette!("{selector} is not a valid semantic version range.")
                })?;
                if selector.trim() == "*" {
                    result.entry(name).or_default().all = Some(patch);
                } else {
                    result
                        .entry(name)
                        .or_default()
                        .range
                        .push(PatchRange { version: selector, patch });
                }
            }
            None => {
                result.entry(name).or_default().all =
                    Some(SelectedPatch { key: key.clone(), file, strict: false });
            }
        }
    }
    Ok(result)
}

fn get_patch_info(
    groups: &PatchGroups,
    package_name: &str,
    package_version: &str,
) -> miette::Result<Option<SelectedPatch>> {
    let Some(group) = groups.get(package_name) else {
        return Ok(None);
    };

    if let Some(patch) = group.exact.get(package_version) {
        return Ok(Some(patch.clone()));
    }

    let version = package_version.parse::<Version>().map_err(|error| {
        miette::miette!("parse package version `{package_version}` for {package_name}: {error}")
    })?;
    let satisfied = group
        .range
        .iter()
        .filter(|candidate| {
            candidate.version.parse::<Range>().is_ok_and(|range| version.satisfies(&range))
        })
        .collect::<Vec<_>>();
    if satisfied.len() > 1 {
        let ranges = satisfied
            .iter()
            .map(|candidate| candidate.version.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(miette::miette!(
            "Unable to choose between {} version ranges to patch {}@{}: {}",
            satisfied.len(),
            package_name,
            package_version,
            ranges
        ));
    }
    if let Some(patch) = satisfied.first() {
        return Ok(Some(patch.patch.clone()));
    }

    Ok(group.all.clone())
}

fn parse_patched_dependency_key(key: &str) -> (String, Option<String>) {
    if let Some(rest) = key.strip_prefix('@')
        && let Some((scope, tail)) = rest.split_once('/')
        && let Some((name, version)) = tail.rsplit_once('@')
        && !version.is_empty()
    {
        return (format!("@{scope}/{name}"), Some(version.to_string()));
    }
    if let Some((name, version)) = key.rsplit_once('@')
        && !name.is_empty()
        && !version.is_empty()
    {
        return (name.to_string(), Some(version.to_string()));
    }
    (key.to_string(), None)
}

fn resolve_patch_file(lockfile_dir: &Path, file: &str) -> PathBuf {
    let file = Path::new(file);
    if file.is_absolute() {
        return file.to_path_buf();
    }
    lockfile_dir.join(file)
}

#[cfg(test)]
mod tests {
    use super::{
        SelectedPatch, get_patch_info, group_patched_dependencies,
        manifest_patched_dependencies_for_lockfile,
    };
    use std::{
        collections::{BTreeMap, HashMap},
        fs,
    };
    use tempfile::tempdir;

    #[test]
    fn exact_patch_has_priority_over_legacy_bare_key() {
        let dir = tempdir().expect("tempdir");
        let groups = group_patched_dependencies(
            dir.path(),
            &BTreeMap::from([
                ("foo".to_string(), "patches/foo.patch".to_string()),
                ("foo@1.0.0".to_string(), "patches/foo@1.patch".to_string()),
            ]),
        )
        .expect("group patches");

        let patch =
            get_patch_info(&groups, "foo", "1.0.0").expect("get patch info").expect("patch");
        assert_eq!(patch.key, "foo@1.0.0");
        assert!(patch.strict);
    }

    #[test]
    fn range_patch_matches_semver_and_remains_strict() {
        let dir = tempdir().expect("tempdir");
        let groups = group_patched_dependencies(
            dir.path(),
            &BTreeMap::from([("foo@^1.0.0".to_string(), "patches/foo.patch".to_string())]),
        )
        .expect("group patches");

        let patch =
            get_patch_info(&groups, "foo", "1.2.3").expect("get patch info").expect("patch");
        assert_eq!(patch.key, "foo@^1.0.0");
        assert!(patch.strict);
    }

    #[test]
    fn overlapping_ranges_error_like_pnpm() {
        let dir = tempdir().expect("tempdir");
        let groups = group_patched_dependencies(
            dir.path(),
            &BTreeMap::from([
                ("foo@^1.0.0".to_string(), "patches/foo-a.patch".to_string()),
                ("foo@>=1.0.0 <2.0.0".to_string(), "patches/foo-b.patch".to_string()),
            ]),
        )
        .expect("group patches");

        let error = get_patch_info(&groups, "foo", "1.2.3").expect_err("expected conflict");
        assert!(error.to_string().contains("Unable to choose between 2 version ranges"));
    }

    #[test]
    fn bare_key_is_non_strict_legacy_match() {
        let dir = tempdir().expect("tempdir");
        let groups = group_patched_dependencies(
            dir.path(),
            &BTreeMap::from([("foo".to_string(), "patches/foo.patch".to_string())]),
        )
        .expect("group patches");

        let patch =
            get_patch_info(&groups, "foo", "9.9.9").expect("get patch info").expect("patch");
        assert_eq!(
            patch,
            SelectedPatch {
                key: "foo".to_string(),
                file: dir.path().join("patches/foo.patch"),
                strict: false,
            }
        );
    }

    #[test]
    fn manifest_patched_dependencies_are_copied_to_lockfile_shape() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            serde_json::json!({
                "name": "workspace",
                "pnpm": {
                    "patchedDependencies": {
                        "foo@1.0.0": "patches/foo.patch"
                    }
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let patched_dependencies =
            manifest_patched_dependencies_for_lockfile(dir.path()).expect("lockfile patched deps");
        assert_eq!(
            patched_dependencies,
            Some(HashMap::from([(
                "foo@1.0.0".to_string(),
                serde_yaml::Value::String("patches/foo.patch".to_string()),
            )]))
        );
    }
}
