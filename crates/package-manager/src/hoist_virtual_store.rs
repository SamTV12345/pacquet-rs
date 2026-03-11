use crate::link_package;
use pacquet_npmrc::{NodeLinker, Npmrc};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn discover_package_dirs(virtual_store_dir: &Path) -> Vec<PathBuf> {
    let mut package_dirs = Vec::new();
    let Ok(entries) = fs::read_dir(virtual_store_dir) else {
        return package_dirs;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry.file_name().to_string_lossy() == "node_modules" {
            continue;
        }

        let store_node_modules = path.join("node_modules");
        let Ok(store_entries) = fs::read_dir(&store_node_modules) else {
            continue;
        };
        for store_entry in store_entries.flatten() {
            let store_path = store_entry.path();
            let name = store_entry.file_name().to_string_lossy().to_string();
            if name.starts_with('@') {
                let Ok(scoped) = fs::read_dir(store_path) else {
                    continue;
                };
                for scoped_entry in scoped.flatten() {
                    let scoped_path = scoped_entry.path();
                    if scoped_path.join("package.json").is_file() {
                        package_dirs.push(scoped_path);
                    }
                }
                continue;
            }

            if store_path.join("package.json").is_file() {
                package_dirs.push(store_path);
            }
        }
    }

    package_dirs
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts = pattern.split('*').filter(|part| !part.is_empty()).collect::<Vec<_>>();
    if parts.is_empty() {
        return false;
    }

    let mut search_start = 0usize;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 && !pattern.starts_with('*') {
            if !value[search_start..].starts_with(part) {
                return false;
            }
            search_start += part.len();
            continue;
        }
        let Some(offset) = value[search_start..].find(part) else {
            return false;
        };
        search_start += offset + part.len();
    }

    pattern.ends_with('*') || value.ends_with(parts.last().expect("checked above"))
}

fn matches_patterns(name: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }

    let mut matched = false;
    for pattern in patterns {
        if let Some(negative) = pattern.strip_prefix('!') {
            if wildcard_match(name, negative) {
                matched = false;
            }
            continue;
        }
        if wildcard_match(name, pattern) {
            matched = true;
        }
    }

    matched
}

fn select_packages_by_patterns(
    virtual_store_dir: &Path,
    patterns: &[String],
    include_all: bool,
) -> BTreeMap<String, (node_semver::Version, PathBuf)> {
    if !virtual_store_dir.exists() {
        return BTreeMap::new();
    }

    let mut selected = BTreeMap::<String, (node_semver::Version, PathBuf)>::new();
    for package_dir in discover_package_dirs(virtual_store_dir) {
        let manifest_path = package_dir.join("package.json");
        let Ok(manifest_text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_text) else {
            continue;
        };
        let Some(name) = manifest.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !include_all && !matches_patterns(name, patterns) {
            continue;
        }
        let Some(version_text) = manifest.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(version) = version_text.parse::<node_semver::Version>() else {
            continue;
        };

        let should_replace = selected.get(name).is_none_or(|(current, _)| version > *current);
        if should_replace {
            selected.insert(name.to_string(), (version, package_dir));
        }
    }

    selected
}

fn hoist_selected_packages(
    symlink: bool,
    root_dir: &Path,
    selected: BTreeMap<String, (node_semver::Version, PathBuf)>,
) -> miette::Result<()> {
    fs::create_dir_all(root_dir).map_err(|error| {
        miette::miette!("create hoisted root directory at {}: {error}", root_dir.display())
    })?;

    for (name, (_, target)) in selected {
        let link_path = root_dir.join(name);
        if link_path.exists() {
            continue;
        }
        link_package(symlink, &target, &link_path)
            .map_err(|error| miette::miette!("create hoisted package symlink: {error}"))?;
    }

    Ok(())
}

pub(crate) fn hoist_virtual_store_packages(config: &Npmrc) -> miette::Result<()> {
    if !config.virtual_store_dir.exists() {
        return Ok(());
    }

    let shared_modules_dir = config
        .virtual_store_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.modules_dir.clone());

    if config.shamefully_hoist {
        let selected = select_packages_by_patterns(&config.virtual_store_dir, &[], true);
        let virtual_hoisted_root = config.virtual_store_dir.join("node_modules");
        hoist_selected_packages(config.symlink, &virtual_hoisted_root, selected)?;
    }

    match config.node_linker {
        NodeLinker::Hoisted => {
            let selected = select_packages_by_patterns(&config.virtual_store_dir, &[], true);
            hoist_selected_packages(config.symlink, &shared_modules_dir, selected)?;
        }
        NodeLinker::Pnp => {}
        _ if config.hoist || config.shamefully_hoist => {
            let selected = select_packages_by_patterns(
                &config.virtual_store_dir,
                &config.public_hoist_pattern,
                config.shamefully_hoist,
            );
            if !selected.is_empty() {
                hoist_selected_packages(config.symlink, &config.modules_dir, selected)?;
            }
        }
        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use pacquet_fs::symlink_dir;
    use pacquet_testing_utils::fs::is_symlink_or_junction;
    use tempfile::tempdir;

    #[test]
    fn hoisted_node_linker_should_hoist_packages_to_project_node_modules() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let package_dir = virtual_store_dir.join("dep@1.0.0/node_modules/dep");
        fs::create_dir_all(&package_dir).expect("create virtual store package dir");
        fs::write(
            package_dir.join("package.json"),
            serde_json::json!({
                "name": "dep",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write package manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Hoisted;
        config.shamefully_hoist = false;

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        let link_path = modules_dir.join("dep");
        assert!(link_path.exists());
        assert!(is_symlink_or_junction(&link_path).expect("read link metadata"));
    }

    #[test]
    fn hoisted_node_linker_should_hoist_to_shared_root_modules_dir() {
        let dir = tempdir().expect("tempdir");
        let shared_modules_dir = dir.path().join("workspace/node_modules");
        let project_modules_dir = dir.path().join("workspace/packages/app/node_modules");
        let virtual_store_dir = shared_modules_dir.join(".pnpm");
        let package_dir = virtual_store_dir.join("dep@1.0.0/node_modules/dep");
        fs::create_dir_all(&package_dir).expect("create virtual store package dir");
        fs::write(
            package_dir.join("package.json"),
            serde_json::json!({
                "name": "dep",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write package manifest");

        let mut config = Npmrc::new();
        config.modules_dir = project_modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Hoisted;
        config.shamefully_hoist = false;

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        assert!(shared_modules_dir.join("dep").exists());
        assert!(!project_modules_dir.join("dep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn hoisted_node_linker_should_discover_symlinked_virtual_store_packages() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let package_root = virtual_store_dir.join("dep@1.0.0/node_modules");
        let real_package_dir = dir.path().join("store/dep");
        let linked_package_dir = package_root.join("dep");

        fs::create_dir_all(&package_root).expect("create package root");
        fs::create_dir_all(&real_package_dir).expect("create real package dir");
        fs::write(
            real_package_dir.join("package.json"),
            serde_json::json!({
                "name": "dep",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write package manifest");
        symlink_dir(&real_package_dir, &linked_package_dir).expect("create package symlink");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Hoisted;
        config.shamefully_hoist = false;

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        let link_path = modules_dir.join("dep");
        assert!(link_path.exists());
        assert!(is_symlink_or_junction(&link_path).expect("read link metadata"));
    }

    #[test]
    fn isolated_node_linker_without_shameful_hoist_should_not_create_root_hoists() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let package_dir = virtual_store_dir.join("dep@1.0.0/node_modules/dep");
        fs::create_dir_all(&package_dir).expect("create virtual store package dir");
        fs::write(
            package_dir.join("package.json"),
            serde_json::json!({
                "name": "dep",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write package manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Isolated;
        config.shamefully_hoist = false;

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        assert!(!modules_dir.join("dep").exists());
    }

    #[test]
    fn public_hoist_pattern_should_hoist_only_matching_packages_to_root_modules() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let eslint_dir = virtual_store_dir.join("eslint@1.0.0/node_modules/eslint");
        let lodash_dir = virtual_store_dir.join("lodash@1.0.0/node_modules/lodash");
        fs::create_dir_all(&eslint_dir).expect("create eslint package dir");
        fs::create_dir_all(&lodash_dir).expect("create lodash package dir");
        fs::write(
            eslint_dir.join("package.json"),
            serde_json::json!({
                "name": "eslint",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write eslint manifest");
        fs::write(
            lodash_dir.join("package.json"),
            serde_json::json!({
                "name": "lodash",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write lodash manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Isolated;
        config.hoist = true;
        config.shamefully_hoist = false;
        config.public_hoist_pattern = vec!["*eslint*".to_string()];

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        assert!(modules_dir.join("eslint").exists());
        assert!(is_symlink_or_junction(&modules_dir.join("eslint")).expect("eslint link"));
        assert!(!modules_dir.join("lodash").exists());
    }

    #[test]
    fn public_hoist_pattern_should_support_negative_patterns() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let eslint_dir = virtual_store_dir.join("eslint@1.0.0/node_modules/eslint");
        let lodash_dir = virtual_store_dir.join("lodash@1.0.0/node_modules/lodash");
        fs::create_dir_all(&eslint_dir).expect("create eslint package dir");
        fs::create_dir_all(&lodash_dir).expect("create lodash package dir");
        fs::write(
            eslint_dir.join("package.json"),
            serde_json::json!({
                "name": "eslint",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write eslint manifest");
        fs::write(
            lodash_dir.join("package.json"),
            serde_json::json!({
                "name": "lodash",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write lodash manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Isolated;
        config.hoist = true;
        config.shamefully_hoist = false;
        config.public_hoist_pattern = vec!["*".to_string(), "!eslint".to_string()];

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        assert!(!modules_dir.join("eslint").exists());
        assert!(modules_dir.join("lodash").exists());
        assert!(is_symlink_or_junction(&modules_dir.join("lodash")).expect("lodash link"));
    }

    #[test]
    fn wildcard_match_supports_leading_star() {
        assert!(wildcard_match("@pnpm.e2e/hello-world-js-bin", "*hello-world-js-bin"));
    }

    #[test]
    fn wildcard_match_empty_pattern_does_not_match_everything() {
        assert!(!wildcard_match("eslint", ""));
    }

    #[test]
    fn pnp_node_linker_should_not_hoist_packages_to_root_modules() {
        let dir = tempdir().expect("tempdir");
        let modules_dir = dir.path().join("project/node_modules");
        let virtual_store_dir = modules_dir.join(".pnpm");
        let pkg_dir = virtual_store_dir.join("dep@1.0.0/node_modules/dep");
        fs::create_dir_all(&pkg_dir).expect("create dep dir");
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({
                "name": "dep",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .expect("write dep manifest");

        let mut config = Npmrc::new();
        config.modules_dir = modules_dir.clone();
        config.virtual_store_dir = virtual_store_dir;
        config.node_linker = NodeLinker::Pnp;
        config.hoist = true;
        config.public_hoist_pattern = vec!["*".to_string()];
        config.shamefully_hoist = false;

        hoist_virtual_store_packages(&config).expect("hoist should succeed");

        assert!(!modules_dir.join("dep").exists());
    }
}
