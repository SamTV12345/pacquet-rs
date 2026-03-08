use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

#[cfg(unix)]
fn relative_path(target: &Path, from_dir: &Path) -> PathBuf {
    use std::path::Component;

    let mut from_components = from_dir.components().peekable();
    let mut target_components = target.components().peekable();

    while from_components.peek() == target_components.peek() {
        from_components.next();
        target_components.next();
    }

    // If roots differ, fallback to absolute path.
    if matches!(from_components.peek(), Some(Component::Prefix(_) | Component::RootDir))
        || matches!(target_components.peek(), Some(Component::Prefix(_) | Component::RootDir))
    {
        return target.to_path_buf();
    }

    let mut relative = PathBuf::new();
    for _ in from_components {
        relative.push("..");
    }
    for component in target_components {
        relative.push(component.as_os_str());
    }
    relative
}

/// Error type for [`symlink_package`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum SymlinkPackageError {
    #[display("Failed to create directory at {dir:?}: {error}")]
    CreateParentDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to create symlink at {symlink_path:?} to {symlink_target:?}: {error}")]
    SymlinkDir {
        symlink_target: PathBuf,
        symlink_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Create symlink for a package.
///
/// * If ancestors of `symlink_path` don't exist, they will be created recursively.
/// * If `symlink_path` already exists, skip.
/// * If `symlink_path` doesn't exist, a symlink pointing to `symlink_target` will be created.
pub fn symlink_package(
    symlink_target: &Path,
    symlink_path: &Path,
) -> Result<(), SymlinkPackageError> {
    if let Some(parent) = symlink_path.parent() {
        fs::create_dir_all(parent).map_err(|error| SymlinkPackageError::CreateParentDir {
            dir: parent.to_path_buf(),
            error,
        })?;
    }
    #[cfg(unix)]
    let symlink_target = symlink_path.parent().map_or_else(
        || symlink_target.to_path_buf(),
        |parent| relative_path(symlink_target, parent),
    );
    #[cfg(windows)]
    let symlink_target = symlink_target.to_path_buf();

    if let Err(error) = symlink_dir(&symlink_target, symlink_path) {
        match error.kind() {
            ErrorKind::AlreadyExists => {}
            _ => {
                return Err(SymlinkPackageError::SymlinkDir {
                    symlink_target,
                    symlink_path: symlink_path.to_path_buf(),
                    error,
                });
            }
        }
    }
    Ok(())
}
