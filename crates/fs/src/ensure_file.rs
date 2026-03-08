use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
fn to_windows_extended_path(path: &Path) -> PathBuf {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return path.to_path_buf();
    }

    let raw = path.as_os_str().to_string_lossy();
    if raw.starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    if let Some(stripped) = raw.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{stripped}"));
    }
    PathBuf::from(format!(r"\\?\{raw}"))
}

/// Error type of [`ensure_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum EnsureFileError {
    #[display("Failed to create the parent directory at {parent_dir:?}: {error}")]
    CreateDir {
        parent_dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to create file at {file_path:?}: {error}")]
    CreateFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to write to file at {file_path:?}: {error}")]
    WriteFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Write `content` to `file_path` unless it already exists.
///
/// Ancestor directories will be created if they don't already exist.
pub fn ensure_file(
    file_path: &Path,
    content: &[u8],
    #[cfg_attr(windows, allow(unused))] mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    if file_path.exists() {
        return Ok(());
    }

    let parent_dir = file_path.parent().unwrap();
    #[cfg(windows)]
    let create_dir_target = to_windows_extended_path(parent_dir);
    #[cfg(not(windows))]
    let create_dir_target = parent_dir.to_path_buf();
    fs::create_dir_all(&create_dir_target).map_err(|error| EnsureFileError::CreateDir {
        parent_dir: parent_dir.to_path_buf(),
        error,
    })?;

    let mut options = OpenOptions::new();
    options.write(true).create(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(mode) = mode {
            options.mode(mode);
        }
    }

    #[cfg(windows)]
    let open_target = to_windows_extended_path(file_path);
    #[cfg(not(windows))]
    let open_target = file_path.to_path_buf();

    options
        .open(&open_target)
        .map_err(|error| EnsureFileError::CreateFile { file_path: file_path.to_path_buf(), error })?
        .write_all(content)
        .map_err(|error| EnsureFileError::WriteFile { file_path: file_path.to_path_buf(), error })
}
