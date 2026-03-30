use crate::{Lockfile, lockfile_file::LockfileFileError, lockfile_file::parse_lockfile_content};
use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use pipe_trait::Pipe;
use std::{
    env, fs,
    io::{self, ErrorKind},
    path::Path,
};

/// Error when reading lockfile the filesystem.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum LoadLockfileError {
    #[display("Failed to get current_dir: {_0}")]
    #[diagnostic(code(pacquet_lockfile::current_dir))]
    CurrentDir(io::Error),

    #[display("Failed to read lockfile content: {_0}")]
    #[diagnostic(code(pacquet_lockfile::read_file))]
    ReadFile(io::Error),

    #[display("Failed to parse lockfile content as YAML: {_0}")]
    #[diagnostic(code(pacquet_lockfile::parse_yaml))]
    ParseYaml(serde_yaml::Error),

    #[display("Failed to parse lockfile format: {_0}")]
    #[diagnostic(code(pacquet_lockfile::parse_lockfile))]
    ParseLockfileFormat(#[error(not(source))] String),
}

impl Lockfile {
    /// Load lockfile from an exact path.
    pub fn load_from_path(file_path: &Path) -> Result<Option<Self>, LoadLockfileError> {
        let content = match fs::read_to_string(file_path) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return error.pipe(LoadLockfileError::ReadFile).pipe(Err),
        };
        parse_lockfile_content(&content).map(Some).map_err(|error| match error {
            LockfileFileError::ParseHeader(parse_error)
            | LockfileFileError::ParseV6(parse_error)
            | LockfileFileError::ParseV9(parse_error) => LoadLockfileError::ParseYaml(parse_error),
            other => LoadLockfileError::ParseLockfileFormat(other.to_string()),
        })
    }

    /// Load lockfile from a specific directory.
    pub fn load_from_dir(dir: &Path) -> Result<Option<Self>, LoadLockfileError> {
        Self::load_from_path(&dir.join(Lockfile::FILE_NAME))
    }

    /// Load lockfile from the current directory.
    pub fn load_from_current_dir() -> Result<Option<Self>, LoadLockfileError> {
        let current_dir = env::current_dir().map_err(LoadLockfileError::CurrentDir)?;
        Self::load_from_dir(&current_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::Lockfile;
    use std::fs;

    #[test]
    fn load_from_path_reads_lockfile_from_exact_location() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lock.yaml");
        fs::write(&path, "lockfileVersion: '9.0'\nimporters:\n  .: {}\n").expect("write lockfile");

        let lockfile =
            Lockfile::load_from_path(&path).expect("load lockfile").expect("lockfile should exist");

        assert_eq!(lockfile.lockfile_version.major, 9);
    }

    #[test]
    fn load_from_dir_returns_none_when_file_does_not_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = Lockfile::load_from_dir(dir.path()).expect("load should not fail");
        assert!(result.is_none(), "expected None when no lockfile exists in dir");
    }

    #[test]
    fn load_from_dir_returns_some_when_file_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lockfile_path = dir.path().join("pnpm-lock.yaml");
        fs::write(
            &lockfile_path,
            "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies: {}\n",
        )
        .expect("write lockfile");

        let lockfile = Lockfile::load_from_dir(dir.path())
            .expect("load should not fail")
            .expect("lockfile should be Some");

        assert_eq!(lockfile.lockfile_version.major, 9);
        assert_eq!(lockfile.lockfile_version.minor, 0);
    }

    #[test]
    fn load_from_path_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nonexistent = dir.path().join("does-not-exist.yaml");
        let result = Lockfile::load_from_path(&nonexistent).expect("load should not fail");
        assert!(result.is_none(), "expected None for nonexistent path");
    }

    #[test]
    fn load_from_path_returns_error_for_invalid_yaml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad-lock.yaml");
        fs::write(&path, "{{{{not valid yaml at all!!!!").expect("write garbage");

        let result = Lockfile::load_from_path(&path);
        assert!(result.is_err(), "expected error when loading invalid YAML");
    }
}
