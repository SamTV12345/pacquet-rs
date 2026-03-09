use crate::symlink_package;
use pacquet_npmrc::Npmrc;
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

pub(crate) fn hoist_virtual_store_packages(config: &Npmrc) -> miette::Result<()> {
    if !config.shamefully_hoist {
        return Ok(());
    }

    let hoisted_root = config.virtual_store_dir.join("node_modules");
    fs::create_dir_all(&hoisted_root)
        .map_err(|error| miette::miette!("create hoisted virtual-store root: {error}"))?;

    let mut selected = BTreeMap::<String, (node_semver::Version, PathBuf)>::new();
    for package_dir in discover_package_dirs(&config.virtual_store_dir) {
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

    for (name, (_, target)) in selected {
        let link_path = hoisted_root.join(name);
        symlink_package(&target, &link_path)
            .map_err(|error| miette::miette!("create hoisted package symlink: {error}"))?;
    }

    Ok(())
}
