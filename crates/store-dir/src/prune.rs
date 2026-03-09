use crate::PackageFilesIndex;
use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

/// Error type of [`StoreDir::prune`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum PruneError {
    #[display("Failed to read directory {path}: {error}")]
    ReadDir {
        path: String,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to remove file {path}: {error}")]
    RemoveFile {
        path: String,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to parse store index file {path}: {error}")]
    ParseIndex {
        path: String,
        #[error(source)]
        error: serde_json::Error,
    },
}

#[derive(Debug)]
struct IndexEntry {
    path: PathBuf,
    package_id: String,
    cas_paths: HashSet<PathBuf>,
}

impl StoreDir {
    /// Remove all files in the store that don't have reference elsewhere.
    pub fn prune(&self) -> Result<(), PruneError> {
        let index_entries = self.collect_index_entries()?;
        if index_entries.is_empty() {
            return Ok(());
        }

        let referenced_package_ids = self.collect_referenced_package_ids()?;
        let mut referenced_cas_paths = HashSet::<PathBuf>::new();
        for entry in &index_entries {
            if referenced_package_ids.contains(&entry.package_id) {
                referenced_cas_paths.extend(entry.cas_paths.iter().cloned());
            }
        }

        let mut removable_cas_paths = HashSet::<PathBuf>::new();
        for entry in index_entries {
            if referenced_package_ids.contains(&entry.package_id) {
                continue;
            }

            fs::remove_file(&entry.path).map_err(|error| PruneError::RemoveFile {
                path: entry.path.display().to_string(),
                error,
            })?;
            remove_empty_parent_dirs(&entry.path, &self.version_dir().join("index"))?;
            removable_cas_paths.extend(entry.cas_paths);
        }

        for cas_path in removable_cas_paths {
            if referenced_cas_paths.contains(&cas_path) {
                continue;
            }
            match fs::remove_file(&cas_path) {
                Ok(()) => remove_empty_parent_dirs(&cas_path, &self.version_dir().join("files"))?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(PruneError::RemoveFile {
                        path: cas_path.display().to_string(),
                        error,
                    });
                }
            }
        }

        Ok(())
    }

    fn collect_index_entries(&self) -> Result<Vec<IndexEntry>, PruneError> {
        let mut result = Vec::new();
        for path in self.index_file_paths() {
            let file = fs::File::open(&path)
                .map_err(|error| PruneError::ReadDir { path: path.display().to_string(), error })?;
            let index = serde_json::from_reader::<_, PackageFilesIndex>(file).map_err(|error| {
                PruneError::ParseIndex { path: path.display().to_string(), error }
            })?;
            let Some(package_id) = package_id_from_index_path_and_payload(&path, &index) else {
                continue;
            };
            let cas_paths = index
                .files
                .values()
                .filter_map(|file_info| {
                    let executable = (file_info.mode & 0o111) != 0;
                    self.cas_file_path_by_integrity(&file_info.integrity, executable)
                })
                .collect::<HashSet<_>>();
            result.push(IndexEntry { path, package_id, cas_paths });
        }
        Ok(result)
    }

    fn collect_referenced_package_ids(&self) -> Result<HashSet<String>, PruneError> {
        let projects_dir = self.version_dir().join("projects");
        if !projects_dir.is_dir() {
            return Ok(HashSet::new());
        }

        let mut package_ids = HashSet::new();
        let entries = fs::read_dir(&projects_dir).map_err(|error| PruneError::ReadDir {
            path: projects_dir.display().to_string(),
            error,
        })?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let project_root = match fs::canonicalize(entry.path()) {
                Ok(path) => path,
                Err(_) => continue,
            };
            package_ids.extend(scan_project_virtual_store(&project_root)?);
        }
        Ok(package_ids)
    }
}

fn package_id_from_index_path_and_payload(
    path: &Path,
    index: &PackageFilesIndex,
) -> Option<String> {
    if let (Some(name), Some(version)) = (index.name.as_deref(), index.version.as_deref()) {
        return Some(format!("{name}@{version}"));
    }

    let file_stem = path.file_stem()?.to_str()?;
    let (_, encoded_package_id) = file_stem.split_once('-')?;
    Some(encoded_package_id.replace('+', "/"))
}

fn scan_project_virtual_store(project_root: &Path) -> Result<HashSet<String>, PruneError> {
    let mut package_ids = HashSet::new();
    for virtual_store in
        [".pnpm", ".pacquet"].iter().map(|name| project_root.join("node_modules").join(name))
    {
        if !virtual_store.is_dir() {
            continue;
        }

        let entries = fs::read_dir(&virtual_store).map_err(|error| PruneError::ReadDir {
            path: virtual_store.display().to_string(),
            error,
        })?;
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let node_modules = entry.path().join("node_modules");
            if !node_modules.is_dir() {
                continue;
            }
            package_ids.extend(scan_node_modules_dir(&node_modules)?);
        }
    }
    Ok(package_ids)
}

fn scan_node_modules_dir(node_modules_dir: &Path) -> Result<HashSet<String>, PruneError> {
    let entries = fs::read_dir(node_modules_dir).map_err(|error| PruneError::ReadDir {
        path: node_modules_dir.display().to_string(),
        error,
    })?;
    let mut package_ids = HashSet::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('@') {
            let scope_entries = fs::read_dir(&path)
                .map_err(|error| PruneError::ReadDir { path: path.display().to_string(), error })?;
            for scope_entry in scope_entries {
                let Ok(scope_entry) = scope_entry else {
                    continue;
                };
                let scope_path = scope_entry.path();
                if scope_entry.file_type().ok().is_some_and(|file_type| file_type.is_dir()) {
                    maybe_read_package_id(&scope_path)?.into_iter().for_each(|id| {
                        package_ids.insert(id);
                    });
                }
            }
            continue;
        }

        maybe_read_package_id(&path)?.into_iter().for_each(|id| {
            package_ids.insert(id);
        });
    }
    Ok(package_ids)
}

fn maybe_read_package_id(package_dir: &Path) -> Result<Option<String>, PruneError> {
    let package_json_path = package_dir.join("package.json");
    if !package_json_path.is_file() {
        return Ok(None);
    }

    let file = fs::File::open(&package_json_path).map_err(|error| PruneError::ReadDir {
        path: package_json_path.display().to_string(),
        error,
    })?;
    let package_json =
        serde_json::from_reader::<_, HashMap<String, serde_json::Value>>(file).map_err(
            |error| PruneError::ParseIndex { path: package_json_path.display().to_string(), error },
        )?;

    let Some(name) = package_json.get("name").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let Some(version) = package_json.get("version").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    Ok(Some(format!("{name}@{version}")))
}

fn remove_empty_parent_dirs(path: &Path, stop_dir: &Path) -> Result<(), PruneError> {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == stop_dir {
            break;
        }
        match fs::remove_dir(dir) {
            Ok(()) => {
                current = dir.parent();
            }
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(PruneError::ReadDir { path: dir.display().to_string(), error });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageFileInfo;
    use ssri::{Algorithm, IntegrityOpts};
    use tempfile::tempdir;

    fn write_index_for_package(
        store: &StoreDir,
        package_id: &str,
        package_name: &str,
        package_version: &str,
        integrity: &str,
        mode: u32,
    ) {
        let tarball_integrity = IntegrityOpts::new()
            .algorithm(Algorithm::Sha512)
            .chain(format!("{package_id}-tarball").as_bytes())
            .result();
        let index = PackageFilesIndex {
            name: Some(package_name.to_string()),
            version: Some(package_version.to_string()),
            requires_build: None,
            files: HashMap::from([(
                "package.json".to_string(),
                PackageFileInfo {
                    checked_at: None,
                    integrity: integrity.to_string(),
                    mode,
                    size: None,
                },
            )]),
            side_effects: None,
        };
        store.write_index_file(&tarball_integrity, package_id, &index).expect("write index");
    }

    fn add_virtual_store_package(project_root: &Path, package_name: &str, version: &str) {
        let virtual_store_entry = project_root
            .join("node_modules/.pnpm")
            .join(format!("{}@{version}", package_name.replace('/', "+")))
            .join("node_modules");
        let package_dir = if let Some((scope, name)) = package_name.split_once('/') {
            virtual_store_entry.join(scope).join(name)
        } else {
            virtual_store_entry.join(package_name)
        };
        fs::create_dir_all(&package_dir).expect("create package dir");
        fs::write(
            package_dir.join("package.json"),
            serde_json::json!({
                "name": package_name,
                "version": version
            })
            .to_string(),
        )
        .expect("write package.json");
    }

    #[test]
    fn prune_removes_only_unreferenced_packages() {
        let dir = tempdir().expect("tempdir");
        let store = StoreDir::new(dir.path().join("store"));
        let project = dir.path().join("project");
        fs::create_dir_all(&project).expect("create project");
        store.register_project(&project).expect("register project");

        let kept_content = b"keep";
        let pruned_content = b"prune";
        let (kept_cas_path, _) = store.write_cas_file(kept_content, false).expect("write cas");
        let (pruned_cas_path, _) = store.write_cas_file(pruned_content, false).expect("write cas");
        let kept_integrity = IntegrityOpts::new()
            .algorithm(Algorithm::Sha512)
            .chain(kept_content)
            .result()
            .to_string();
        let pruned_integrity = IntegrityOpts::new()
            .algorithm(Algorithm::Sha512)
            .chain(pruned_content)
            .result()
            .to_string();

        write_index_for_package(
            &store,
            "@pnpm.e2e/hello-world-js-bin@1.0.0",
            "@pnpm.e2e/hello-world-js-bin",
            "1.0.0",
            &kept_integrity,
            0o644,
        );
        write_index_for_package(
            &store,
            "@pnpm/xyz@1.0.0",
            "@pnpm/xyz",
            "1.0.0",
            &pruned_integrity,
            0o644,
        );

        add_virtual_store_package(&project, "@pnpm.e2e/hello-world-js-bin", "1.0.0");

        store.prune().expect("prune store");

        let mut remaining = store
            .index_file_paths()
            .into_iter()
            .filter_map(|path| {
                let file = fs::File::open(path).ok()?;
                let index = serde_json::from_reader::<_, PackageFilesIndex>(file).ok()?;
                Some(format!("{}@{}", index.name?, index.version?))
            })
            .collect::<Vec<_>>();
        remaining.sort();
        assert_eq!(remaining, vec!["@pnpm.e2e/hello-world-js-bin@1.0.0".to_string()]);
        assert!(kept_cas_path.is_file());
        assert!(!pruned_cas_path.exists());
    }
}
