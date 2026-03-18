use miette::{Context, IntoDiagnostic};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchEditState {
    pub original_dir: PathBuf,
    pub package_name: String,
    pub package_version: String,
    pub patched_pkg: String,
    pub apply_to_all: bool,
}

pub fn parse_package_spec(spec: &str) -> (String, Option<String>) {
    if let Some(rest) = spec.strip_prefix('@')
        && let Some((scope, tail)) = rest.split_once('/')
        && let Some((name, version)) = tail.rsplit_once('@')
    {
        return (format!("@{scope}/{name}"), Some(version.to_string()));
    }
    if let Some((name, version)) = spec.rsplit_once('@')
        && !name.is_empty()
    {
        return (name.to_string(), Some(version.to_string()));
    }
    (spec.to_string(), None)
}

pub fn installed_package_dir(project_dir: &Path, package_name: &str) -> PathBuf {
    let mut path = project_dir.join("node_modules");
    for segment in package_name.split('/') {
        path.push(segment);
    }
    path
}

pub fn patch_state_file_path(modules_dir: &Path) -> PathBuf {
    modules_dir.join(".pnpm_patches").join("state.json")
}

pub fn read_patch_state(modules_dir: &Path) -> miette::Result<BTreeMap<String, PatchEditState>> {
    let state_file = patch_state_file_path(modules_dir);
    if !state_file.is_file() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(&state_file)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", state_file.display()))?;
    serde_json::from_str(&content).into_diagnostic().wrap_err("parse patch state")
}

pub fn write_patch_state(
    modules_dir: &Path,
    state: &BTreeMap<String, PatchEditState>,
) -> miette::Result<()> {
    let state_file = patch_state_file_path(modules_dir);
    if let Some(parent) = state_file.parent() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", parent.display()))?;
    }
    let rendered =
        serde_json::to_string_pretty(state).into_diagnostic().wrap_err("serialize patch state")?;
    fs::write(&state_file, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", state_file.display()))
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) -> miette::Result<()> {
    fn walk(src_root: &Path, dst_root: &Path, dir: &Path) -> miette::Result<()> {
        for entry in fs::read_dir(dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", dir.display()))?
        {
            let entry = entry.into_diagnostic().wrap_err("read patch entry")?;
            let path = entry.path();
            let file_type = entry.file_type().into_diagnostic().wrap_err("read patch file type")?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if matches!(file_name.as_str(), "node_modules" | ".git" | "target") {
                continue;
            }
            let relative = path.strip_prefix(src_root).unwrap_or(&path);
            let target = dst_root.join(relative);
            if file_type.is_dir() {
                fs::create_dir_all(&target)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("create {}", target.display()))?;
                walk(src_root, dst_root, &path)?;
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .into_diagnostic()
                        .wrap_err_with(|| format!("create {}", parent.display()))?;
                }
                fs::copy(&path, &target)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("copy {} to {}", path.display(), target.display()))?;
            }
        }
        Ok(())
    }

    fs::create_dir_all(dst)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", dst.display()))?;
    walk(src, dst, src)
}

pub fn read_patched_dependencies(manifest_path: &Path) -> miette::Result<BTreeMap<String, String>> {
    let value = read_package_json_value(manifest_path)?;
    Ok(value
        .get("pnpm")
        .and_then(|pnpm| pnpm.get("patchedDependencies"))
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default())
}

pub fn write_patched_dependencies(
    manifest_path: &Path,
    patched_dependencies: &BTreeMap<String, String>,
) -> miette::Result<()> {
    let mut value = read_package_json_value(manifest_path)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    let pnpm = root.entry("pnpm").or_insert_with(|| Value::Object(Map::new()));
    let pnpm = pnpm
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;

    if patched_dependencies.is_empty() {
        pnpm.remove("patchedDependencies");
    } else {
        let patched_value = patched_dependencies
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<Map<_, _>>();
        pnpm.insert("patchedDependencies".to_string(), Value::Object(patched_value));
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

pub fn patch_file_name(key: &str) -> String {
    key.replace('/', "__")
}

pub fn relative_patch_path(base_dir: &Path, patch_file: &Path) -> String {
    patch_file.strip_prefix(base_dir).unwrap_or(patch_file).display().to_string().replace('\\', "/")
}

fn read_package_json_value(manifest_path: &Path) -> miette::Result<Value> {
    let content = fs::read_to_string(manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")
}

#[cfg(test)]
mod tests {
    use super::parse_package_spec;

    #[test]
    fn parse_package_spec_supports_scoped_and_unscoped_names() {
        assert_eq!(
            parse_package_spec("@scope/pkg@1.2.3"),
            ("@scope/pkg".to_string(), Some("1.2.3".to_string()))
        );
        assert_eq!(parse_package_spec("pkg@1.2.3"), ("pkg".to_string(), Some("1.2.3".to_string())));
        assert_eq!(parse_package_spec("pkg"), ("pkg".to_string(), None));
    }
}
