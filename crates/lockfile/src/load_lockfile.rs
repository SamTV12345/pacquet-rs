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
    /// Load lockfile from a specific directory.
    pub fn load_from_dir(dir: &Path) -> Result<Option<Self>, LoadLockfileError> {
        let file_path = dir.join(Lockfile::FILE_NAME);
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

    /// Load lockfile from the current directory.
    pub fn load_from_current_dir() -> Result<Option<Self>, LoadLockfileError> {
        let current_dir = env::current_dir().map_err(LoadLockfileError::CurrentDir)?;
        Self::load_from_dir(&current_dir)
    }
}
