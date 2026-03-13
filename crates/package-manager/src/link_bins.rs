use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashSet},
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
        if !package_manifest_path.is_file() {
            continue;
        }
        let package_manifest = PackageManifest::from_path(package_manifest_path.clone())
            .wrap_err_with(|| format!("load {}", package_manifest_path.display()))?;

        for (bin_name, relative_target) in collect_bin_entries(&package_manifest) {
            let target = package_dir.join(relative_target);
            write_bin_wrapper(config, &bin_dir, &bin_name, &target)
                .wrap_err_with(|| format!("link bin `{bin_name}` from {}", target.display()))?;
        }
    }

    Ok(())
}

pub fn link_bins_from_package_manifest(
    config: &Npmrc,
    manifest: &PackageManifest,
    package_dir: &Path,
    bin_dir: &Path,
) -> miette::Result<()> {
    for (bin_name, relative_target) in collect_bin_entries(manifest) {
        let target = package_dir.join(relative_target);
        write_bin_wrapper(config, bin_dir, &bin_name, &target)
            .wrap_err_with(|| format!("link bin `{bin_name}` from {}", target.display()))?;
    }
    Ok(())
}

pub(crate) fn collect_bin_entries(manifest: &PackageManifest) -> Vec<(String, String)> {
    let manifest_dir =
        manifest.path().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    match manifest.value().get("bin") {
        Some(Value::String(target)) => manifest
            .value()
            .get("name")
            .and_then(Value::as_str)
            .map(default_bin_name)
            .map(|name| collect_safe_bin_entry(&manifest_dir, &name, target))
            .unwrap_or_default(),
        Some(Value::Object(entries)) => entries
            .iter()
            .filter_map(|(name, target)| {
                let target = target.as_str()?;
                collect_safe_bin_entry(&manifest_dir, name, target).into_iter().next()
            })
            .collect(),
        _ => collect_bin_entries_from_directories(manifest),
    }
}

fn collect_bin_entries_from_directories(manifest: &PackageManifest) -> Vec<(String, String)> {
    let Some(bin_dir) = manifest
        .value()
        .get("directories")
        .and_then(Value::as_object)
        .and_then(|dirs| dirs.get("bin"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };

    let manifest_dir =
        manifest.path().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let absolute_bin_dir = manifest_dir.join(bin_dir);
    let Ok(manifest_root) = fs::canonicalize(&manifest_dir) else {
        return Vec::new();
    };
    let Ok(absolute_bin_dir) = fs::canonicalize(&absolute_bin_dir) else {
        return Vec::new();
    };
    if !absolute_bin_dir.starts_with(&manifest_root) {
        return Vec::new();
    }

    let mut files = Vec::new();
    collect_bin_files_iterative(&manifest_root, &absolute_bin_dir, &mut files);
    files
        .into_iter()
        .filter_map(|file| {
            let relative = file.strip_prefix(&manifest_root).ok()?;
            let name = file.file_name()?.to_string_lossy().to_string();
            Some((name, relative.to_string_lossy().replace('\\', "/")))
        })
        .collect()
}

fn collect_bin_files_iterative(manifest_root: &Path, root_dir: &Path, files: &mut Vec<PathBuf>) {
    let mut pending = vec![root_dir.to_path_buf()];
    let mut visited_dirs = HashSet::new();

    while let Some(dir) = pending.pop() {
        let Ok(canonical_dir) = fs::canonicalize(&dir) else {
            continue;
        };
        if !canonical_dir.starts_with(manifest_root) || !visited_dirs.insert(canonical_dir.clone())
        {
            continue;
        }

        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_file() {
                let Ok(canonical_file) = fs::canonicalize(&path) else {
                    continue;
                };
                if canonical_file.starts_with(manifest_root) {
                    files.push(canonical_file);
                }
                continue;
            }

            if file_type.is_dir() {
                pending.push(path);
            }
        }
    }
}

fn collect_safe_bin_entry(
    manifest_dir: &Path,
    command_name: &str,
    relative_target: &str,
) -> Vec<(String, String)> {
    let Some(bin_name) = normalize_bin_name(command_name) else {
        return Vec::new();
    };
    if !is_safe_bin_name(&bin_name) {
        return Vec::new();
    }
    let target_path = manifest_dir.join(relative_target);
    let Ok(manifest_root) = fs::canonicalize(manifest_dir) else {
        return Vec::new();
    };
    let Ok(target_path) = fs::canonicalize(target_path) else {
        return Vec::new();
    };
    if !target_path.starts_with(&manifest_root) {
        return Vec::new();
    }
    vec![(bin_name, relative_target.to_string())]
}

fn normalize_bin_name(command_name: &str) -> Option<String> {
    if let Some(stripped) = command_name.strip_prefix('@') {
        let (_, name) = stripped.split_once('/')?;
        return Some(name.to_string());
    }
    Some(command_name.to_string())
}

fn is_safe_bin_name(bin_name: &str) -> bool {
    bin_name == "$"
        || bin_name.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        })
}

fn default_bin_name(package_name: &str) -> String {
    package_name.rsplit('/').next().unwrap_or(package_name).to_string()
}

pub(crate) fn write_bin_wrapper(
    config: &Npmrc,
    bin_dir: &Path,
    bin_name: &str,
    target: &Path,
) -> miette::Result<()> {
    #[cfg(windows)]
    let _ = config;

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
    let prefer_symlinked_executables = config_prefers_symlinked_executables(config);

    #[cfg(unix)]
    {
        if prefer_symlinked_executables && !use_node {
            let bin_path = bin_dir.join(bin_name);
            if bin_path.exists() {
                fs::remove_file(&bin_path)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("remove {}", bin_path.display()))?;
            }
            std::os::unix::fs::symlink(&target, &bin_path)
                .into_diagnostic()
                .wrap_err_with(|| format!("symlink {}", bin_path.display()))?;
            return Ok(());
        }

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

#[cfg(any(test, unix))]
fn config_prefers_symlinked_executables(config: &Npmrc) -> bool {
    config.prefer_symlinked_executables_enabled()
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
        fs::create_dir_all(dir.path().join("bin")).expect("create bin dir");
        fs::write(dir.path().join("bin/hello.js"), "console.log('hello')\n")
            .expect("write hello.js");
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
        fs::create_dir_all(dir.path().join("bin")).expect("create bin dir");
        fs::write(dir.path().join("bin/hello.js"), "console.log('hello')\n")
            .expect("write hello.js");
        fs::write(dir.path().join("bin/alt.js"), "console.log('hello-alt')\n")
            .expect("write alt.js");
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
    fn directories_bin_uses_file_names_and_collects_recursively() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        let bin_dir = dir.path().join("bin/nested");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::write(dir.path().join("bin/root"), "#!/bin/sh\necho hi\n").expect("write root");
        fs::write(bin_dir.join("cli"), "#!/bin/sh\necho hi\n").expect("write cli");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "@scope/pkg-with-directories-bin",
                "directories": {
                    "bin": "bin"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        let mut entries = collect_bin_entries(&manifest);
        entries.sort();
        assert_eq!(
            entries,
            vec![
                ("cli".to_string(), "bin/nested/cli".to_string()),
                ("root".to_string(), "bin/root".to_string()),
            ]
        );
    }

    #[test]
    fn directories_bin_skips_path_traversal_outside_package_root() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "malicious",
                "directories": {
                    "bin": "../outside"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert!(collect_bin_entries(&manifest).is_empty());
    }

    #[test]
    fn directories_bin_skips_real_path_traversal_outside_package_root() {
        let dir = tempdir().expect("tempdir");
        let secret_dir = dir.path().join("secret");
        let pkg_dir = dir.path().join("pkg");
        fs::create_dir_all(&secret_dir).expect("create secret dir");
        fs::create_dir_all(&pkg_dir).expect("create pkg dir");
        fs::write(secret_dir.join("secret.sh"), "echo secret").expect("write secret");
        let manifest_path = pkg_dir.join("package.json");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "malicious",
                "directories": {
                    "bin": "../secret"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert!(collect_bin_entries(&manifest).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn directories_bin_skips_symlink_cycles_without_recursing_forever() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        let bin_dir = dir.path().join("bin");
        let nested_dir = bin_dir.join("nested");
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        fs::write(nested_dir.join("cli"), "#!/bin/sh\necho hi\n").expect("write cli");
        std::os::unix::fs::symlink(&bin_dir, nested_dir.join("loop")).expect("create symlink loop");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "@scope/pkg-with-directories-bin",
                "directories": {
                    "bin": "bin"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("cli".to_string(), "bin/nested/cli".to_string())]
        );
    }

    #[test]
    fn object_bin_skips_path_traversal_outside_package_root() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(dir.path().join("good.js"), "console.log('ok')").expect("write good.js");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "pkg",
                "bin": {
                    "safe": "good.js",
                    "unsafe": "../escape.js"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("safe".to_string(), "good.js".to_string())]
        );
    }

    #[test]
    fn object_bin_accepts_scoped_name_and_skips_scoped_path_traversal_name() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(dir.path().join("good.js"), "console.log('ok')").expect("write good.js");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "pkg",
                "bin": {
                    "@scope/../../.npmrc": "./malicious.js",
                    "@scope/../etc/passwd": "./evil.js",
                    "@scope/legit": "./good.js"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("legit".to_string(), "./good.js".to_string())]
        );
    }

    #[test]
    fn object_bin_normalizes_scoped_command_name() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(dir.path().join("a"), "echo ok").expect("write a");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "@foo/a",
                "bin": {
                    "@foo/a": "./a"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(collect_bin_entries(&manifest), vec![("a".to_string(), "./a".to_string())]);
    }

    #[test]
    fn object_bin_allows_dollar_command_name() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(dir.path().join("undollar.js"), "console.log('ok')").expect("write undollar.js");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "undollar",
                "bin": {
                    "$": "./undollar.js"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("$".to_string(), "./undollar.js".to_string())]
        );
    }

    #[test]
    fn string_bin_skips_path_traversal_outside_package_root() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "foo",
                "bin": "../bad"
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert!(collect_bin_entries(&manifest).is_empty());
    }

    #[test]
    fn object_bin_skips_dangerous_bin_names() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("package.json");
        fs::write(dir.path().join("good"), "echo ok").expect("write good");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "foo",
                "bin": {
                    "../bad": "./bad",
                    "..\\\\bad": "./bad",
                    "good": "./good",
                    "~/bad": "./bad"
                }
            })
            .to_string(),
        )
        .expect("write package.json");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        assert_eq!(
            collect_bin_entries(&manifest),
            vec![("good".to_string(), "./good".to_string())]
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

    #[test]
    fn hoisted_node_linker_prefers_symlinked_executables_by_default() {
        let mut config = Npmrc::new();
        config.node_linker = pacquet_npmrc::NodeLinker::Hoisted;

        assert!(config_prefers_symlinked_executables(&config));
    }

    #[cfg(unix)]
    #[test]
    fn prefer_symlinked_executables_creates_symlink_for_non_node_binary() {
        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("hello");
        let bin_dir = dir.path().join(".bin");
        let mut file = fs::File::create(&target).expect("create target");
        writeln!(file, "#!/bin/sh").expect("write shebang");
        writeln!(file, "echo hello").expect("write body");

        let mut config = Npmrc::new();
        config.prefer_symlinked_executables = Some(true);

        write_bin_wrapper(&config, &bin_dir, "hello", &target).expect("write bin");

        let metadata = fs::symlink_metadata(bin_dir.join("hello")).expect("read metadata");
        assert!(metadata.file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn prefer_symlinked_executables_symlinks_directories_bin_entry() {
        let dir = tempdir().expect("tempdir");
        let project_dir = dir.path().join("project");
        let manifest_path = project_dir.join("package.json");
        let modules_dir = dir.path().join("node_modules");
        let target_dir = modules_dir.join("@scope/pkg-with-directories-bin");
        let target = target_dir.join("bin/cli");
        fs::create_dir_all(&project_dir).expect("create project dir");
        fs::create_dir_all(target.parent().expect("target parent")).expect("create bin dir");
        fs::create_dir_all(&modules_dir).expect("create modules dir");
        fs::write(
            &manifest_path,
            serde_json::json!({
                "name": "app",
                "dependencies": {
                    "@scope/pkg-with-directories-bin": "1.0.0"
                }
            })
            .to_string(),
        )
        .expect("write project package.json");
        fs::write(
            target_dir.join("package.json"),
            serde_json::json!({
                "name": "@scope/pkg-with-directories-bin",
                "directories": {
                    "bin": "bin"
                }
            })
            .to_string(),
        )
        .expect("write dependency package.json");
        fs::write(&target, "#!/bin/sh\necho hi\n").expect("write cli");

        let manifest = PackageManifest::from_path(manifest_path).expect("load manifest");
        let mut config = Npmrc::new();
        config.modules_dir = modules_dir;
        config.node_linker = pacquet_npmrc::NodeLinker::Hoisted;

        link_bins_for_manifest(&config, &manifest, [DependencyGroup::Prod]).expect("link bins");

        let metadata =
            fs::symlink_metadata(config.modules_dir.join(".bin/cli")).expect("read metadata");
        assert!(metadata.file_type().is_symlink());
    }
}
