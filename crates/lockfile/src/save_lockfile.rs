use crate::{Lockfile, lockfile_file::render_lockfile_content};
use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Error when writing lockfile to filesystem.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum SaveLockfileError {
    #[display("Failed to serialize lockfile as YAML: {_0}")]
    #[diagnostic(code(pacquet_lockfile::serialize_yaml))]
    SerializeYaml(serde_yaml::Error),

    #[display("Failed to write lockfile to {path:?}: {error}")]
    #[diagnostic(code(pacquet_lockfile::write_file))]
    WriteFile {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

impl Lockfile {
    /// Path to `pnpm-lock.yaml` inside `dir`.
    pub fn path_in_dir(dir: &Path) -> PathBuf {
        dir.join(Self::FILE_NAME)
    }

    /// Save lockfile to `pnpm-lock.yaml` under `dir`.
    pub fn save_to_dir(&self, dir: &Path) -> Result<(), SaveLockfileError> {
        self.save_to_path(&Self::path_in_dir(dir))
    }

    /// Save lockfile to a specific `path`.
    pub fn save_to_path(&self, path: &Path) -> Result<(), SaveLockfileError> {
        let yaml = render_lockfile_content(self).map_err(SaveLockfileError::SerializeYaml)?;
        fs::write(path, yaml)
            .map_err(|error| SaveLockfileError::WriteFile { path: path.to_path_buf(), error })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComVer, ProjectSnapshot, RootProjectSnapshot};
    use tempfile::tempdir;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let lockfile = Lockfile {
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
            project_snapshot: RootProjectSnapshot::Single(ProjectSnapshot::default()),
            packages: None,
        };

        lockfile.save_to_dir(dir.path()).unwrap();

        let loaded = Lockfile::load_from_dir(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.lockfile_version, lockfile.lockfile_version);
        assert_eq!(loaded.project_snapshot, lockfile.project_snapshot);
    }
}
