use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub fn link_bins_for_manifest(
    config: &Npmrc,
    manifest: &PackageManifest,
    dependency_groups: impl IntoIterator<Item = DependencyGroup>,
) -> miette::Result<()> {
    let direct_dependencies = manifest
        .dependencies(dependency_groups)
        .map(|(name, _)| name.to_string())
        .collect::<BTreeSet<_>>();
    let bin_dir = config.modules_dir.join(".bin");

    for dependency_name in direct_dependencies {
        let package_dir = config.modules_dir.join(&dependency_name);
        if !package_dir.exists() {
            continue;
        }

        let package_manifest_path = package_dir.join("package.json");
        let package_manifest = PackageManifest::from_path(package_manifest_path.clone())
            .wrap_err_with(|| format!("load {}", package_manifest_path.display()))?;

        for (bin_name, relative_target) in collect_bin_entries(&package_manifest) {
            let target = package_dir.join(relative_target);
            write_bin_wrapper(&bin_dir, &bin_name, &target)
                .wrap_err_with(|| format!("link bin `{bin_name}` from {}", target.display()))?;
        }
    }

    Ok(())
}

fn collect_bin_entries(manifest: &PackageManifest) -> Vec<(String, String)> {
    match manifest.value().get("bin") {
        Some(Value::String(target)) => manifest
            .value()
            .get("name")
            .and_then(Value::as_str)
            .map(default_bin_name)
            .map(|name| vec![(name, target.clone())])
            .unwrap_or_default(),
        Some(Value::Object(entries)) => entries
            .iter()
            .filter_map(|(name, target)| {
                target.as_str().map(|target| (name.clone(), target.to_string()))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn default_bin_name(package_name: &str) -> String {
    package_name.rsplit('/').next().unwrap_or(package_name).to_string()
}

fn write_bin_wrapper(bin_dir: &Path, bin_name: &str, target: &Path) -> miette::Result<()> {
    if !target.is_file() {
        miette::bail!("bin target does not exist: {}", target.display());
    }

    let target = target
        .canonicalize()
        .into_diagnostic()
        .wrap_err_with(|| format!("canonicalize {}", target.display()))?;
    #[cfg(windows)]
    let target = normalize_windows_script_path(target);
    fs::create_dir_all(bin_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", bin_dir.display()))?;
    let use_node = should_launch_with_node(&target);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let bin_path = bin_dir.join(bin_name);
        let content = create_unix_wrapper(&target, use_node);
        fs::write(&bin_path, content)
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", bin_path.display()))?;
        let mut permissions = fs::metadata(&bin_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("stat {}", bin_path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bin_path, permissions)
            .into_diagnostic()
            .wrap_err_with(|| format!("chmod {}", bin_path.display()))?;
    }

    #[cfg(windows)]
    {
        let cmd_path = bin_dir.join(format!("{bin_name}.cmd"));
        fs::write(&cmd_path, create_windows_cmd_wrapper(&target, use_node))
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", cmd_path.display()))?;

        let ps1_path = bin_dir.join(format!("{bin_name}.ps1"));
        fs::write(&ps1_path, create_windows_ps1_wrapper(&target, use_node))
            .into_diagnostic()
            .wrap_err_with(|| format!("write {}", ps1_path.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
fn create_unix_wrapper(target: &Path, use_node: bool) -> String {
    let target = shell_quote_posix(&target.to_string_lossy());
    if use_node {
        format!("#!/bin/sh\nexec node {target} \"$@\"\n")
    } else {
        format!("#!/bin/sh\nexec {target} \"$@\"\n")
    }
}

#[cfg(windows)]
fn create_windows_cmd_wrapper(target: &Path, use_node: bool) -> String {
    let target = escape_windows_cmd_arg(target);
    if use_node {
        format!("@ECHO off\r\nnode \"{target}\" %*\r\n")
    } else {
        format!("@ECHO off\r\n\"{target}\" %*\r\n")
    }
}

#[cfg(windows)]
fn create_windows_ps1_wrapper(target: &Path, use_node: bool) -> String {
    let target = target.to_string_lossy().replace('\'', "''");
    if use_node {
        format!("& node '{target}' @args\r\n")
    } else {
        format!("& '{target}' @args\r\n")
    }
}

fn should_launch_with_node(target: &Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "js" | "cjs" | "mjs"))
        || fs::read_to_string(target)
            .ok()
            .and_then(|content| content.lines().next().map(str::to_string))
            .is_some_and(|line| line.starts_with("#!") && line.contains("node"))
}

#[cfg(unix)]
fn shell_quote_posix(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    let is_safe = input.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@' | '+' | '=')
    });
    if is_safe { input.to_string() } else { format!("'{}'", input.replace('\'', "'\"'\"'")) }
}

#[cfg(windows)]
fn escape_windows_cmd_arg(path: &Path) -> String {
    path.to_string_lossy().replace('"', "\"\"")
}

#[cfg(windows)]
fn normalize_windows_script_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{stripped}"));
    }
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn string_bin_uses_unscoped_package_name() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "@scope/hello",
                "bin": "bin/hello.js"
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("hello".to_string(), "bin/hello.js".to_string())]
        );
    }

    #[test]
    fn object_bin_preserves_all_entries() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "hello",
                "bin": {
                    "hello": "bin/hello.js",
                    "hello-alt": "bin/alt.js"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        let mut received = collect_bin_entries(&manifest);
        received.sort();
        assert_eq!(
            received,
            vec![
                ("hello".to_string(), "bin/hello.js".to_string()),
                ("hello-alt".to_string(), "bin/alt.js".to_string()),
            ]
        );
    }

    #[test]
    fn shebang_node_script_is_detected() {
        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("hello");
        let mut file = fs::File::create(&target).expect("create target");
        writeln!(file, "#!/usr/bin/env node").expect("write shebang");
        writeln!(file, "console.log('hello')").expect("write body");

        assert!(should_launch_with_node(&target));
    }
}
