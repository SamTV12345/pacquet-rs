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
            let Ok(store_file_type) = store_entry.file_type() else {
                continue;
            };
            if !store_file_type.is_dir() {
                continue;
            }

            let name = store_entry.file_name().to_string_lossy().to_string();
            if name.starts_with('@') {
                let Ok(scoped) = fs::read_dir(store_path) else {
                    continue;
                };
                for scoped_entry in scoped.flatten() {
                    let scoped_path = scoped_entry.path();
                    let Ok(scoped_type) = scoped_entry.file_type() else {
                        continue;
                    };
                    if scoped_type.is_dir() && scoped_path.join("package.json").is_file() {
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

    let mut remainder = value;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }
        if first {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            first = false;
            continue;
        }
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }

    if !pattern.ends_with('*')
        && let Some(last) = pattern.split('*').next_back()
    {
        return value.ends_with(last);
    }

    true
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

    if config.shamefully_hoist {
        let selected = select_packages_by_patterns(&config.virtual_store_dir, &[], true);
        let virtual_hoisted_root = config.virtual_store_dir.join("node_modules");
        hoist_selected_packages(config.symlink, &virtual_hoisted_root, selected)?;
    }

    match config.node_linker {
        NodeLinker::Hoisted => {
            let selected = select_packages_by_patterns(&config.virtual_store_dir, &[], true);
            hoist_selected_packages(config.symlink, &config.modules_dir, selected)?;
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
