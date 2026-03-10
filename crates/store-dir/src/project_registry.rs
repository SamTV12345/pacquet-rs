use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Error type of [`StoreDir::register_project`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum RegisterProjectError {
    #[display("Failed to create projects registry directory at {path}: {error}")]
    CreateRegistryDir {
        path: String,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to register project with a symlink from {link} to {target}: {error}")]
    CreateRegistryLink {
        link: String,
        target: String,
        #[error(source)]
        error: io::Error,
    },
}

impl StoreDir {
    /// Register a project as using this store.
    ///
    /// pnpm keeps symlinks in `{store}/v10/projects/{hash}` that point to project roots.
    pub fn register_project(&self, project_dir: &Path) -> Result<(), RegisterProjectError> {
        let normalized_store_root = normalize_path(self.root_dir());
        let normalized_project_dir = normalize_path(project_dir);
        if normalized_store_root.starts_with(&normalized_project_dir) {
            return Ok(());
        }

        let registry_dir = self.version_dir().join("projects");
        fs::create_dir_all(&registry_dir).map_err(|error| {
            RegisterProjectError::CreateRegistryDir {
                path: registry_dir.display().to_string(),
                error,
            }
        })?;

        let hash = create_short_hash(&normalized_project_dir);
        let link_path = registry_dir.join(hash);
        if link_path.exists() {
            return Ok(());
        }

        symlink_dir(&normalized_project_dir, &link_path).map_err(|error| {
            RegisterProjectError::CreateRegistryLink {
                link: link_path.display().to_string(),
                target: normalized_project_dir.display().to_string(),
                error,
            }
        })
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn create_short_hash(project_dir: &Path) -> String {
    let digest = Sha256::digest(project_dir.to_string_lossy().as_bytes());
    format!("{digest:x}")[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn register_project_creates_project_entry() {
        let dir = tempdir().expect("tempdir");
        let project_dir = dir.path().join("workspace");
        let store_dir = dir.path().join("store");
        fs::create_dir_all(&project_dir).expect("create project dir");
        let store = StoreDir::new(&store_dir);

        store.register_project(&project_dir).expect("register project");

        let entries = fs::read_dir(store_dir.join("v10/projects"))
            .expect("read projects dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect entries");
        assert_eq!(entries.len(), 1);

        let link_path = entries[0].path();
        assert_eq!(link_path.file_name().expect("file name").to_string_lossy().len(), 32);
        assert!(link_path.exists());
    }

    #[test]
    fn register_project_handles_store_path_with_parent_segments() {
        let dir = tempdir().expect("tempdir");
        let project_dir = dir.path().join("workspace");
        let store_dir_with_parent = project_dir.join("../store");
        fs::create_dir_all(&project_dir).expect("create project dir");
        let store = StoreDir::new(&store_dir_with_parent);

        store.register_project(&project_dir).expect("register project");

        let entries = fs::read_dir(dir.path().join("store").join("v10/projects"))
            .expect("read projects dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect entries");
        assert_eq!(entries.len(), 1);
    }
}
