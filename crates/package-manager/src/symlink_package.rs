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

    #[display("Failed to remove existing path at {path:?}: {error}")]
    RemoveExistingPath {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to rename existing path at {path:?} to {rename_to:?}: {error}")]
    RenameExistingPath {
        path: PathBuf,
        rename_to: PathBuf,
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

    force_symlink(&symlink_target, symlink_path, false)
}

fn force_symlink(
    symlink_target: &Path,
    symlink_path: &Path,
    rename_tried: bool,
) -> Result<(), SymlinkPackageError> {
    let initial_error = match symlink_dir(symlink_target, symlink_path) {
        Ok(()) => return Ok(()),
        Err(error)
            if matches!(error.kind(), ErrorKind::AlreadyExists | ErrorKind::IsADirectory) =>
        {
            error
        }
        Err(error) => {
            return Err(SymlinkPackageError::SymlinkDir {
                symlink_target: symlink_target.to_path_buf(),
                symlink_path: symlink_path.to_path_buf(),
                error,
            });
        }
    };

    if existing_points_to_target(symlink_target, symlink_path) {
        return Ok(());
    }

    if rename_tried {
        remove_existing_path(symlink_path)?;
    } else {
        rename_existing_path(symlink_path).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                SymlinkPackageError::SymlinkDir {
                    symlink_target: symlink_target.to_path_buf(),
                    symlink_path: symlink_path.to_path_buf(),
                    error: initial_error,
                }
            } else {
                SymlinkPackageError::RenameExistingPath {
                    path: symlink_path.to_path_buf(),
                    rename_to: ignored_path(symlink_path),
                    error,
                }
            }
        })?;
    }

    force_symlink(symlink_target, symlink_path, true)
}

fn existing_points_to_target(symlink_target: &Path, symlink_path: &Path) -> bool {
    let compare_target = if symlink_target.is_absolute() {
        symlink_target.to_path_buf()
    } else {
        symlink_path
            .parent()
            .map_or_else(|| symlink_target.to_path_buf(), |parent| parent.join(symlink_target))
    };
    fs::canonicalize(symlink_path)
        .ok()
        .zip(fs::canonicalize(compare_target).ok())
        .is_some_and(|(existing, wanted)| existing == wanted)
}

fn ignored_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map_or_else(|| "unknown".to_string(), |name| name.to_string_lossy().into_owned());
    parent.join(format!(".ignored_{file_name}"))
}

fn remove_existing_path(path: &Path) -> Result<(), SymlinkPackageError> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        SymlinkPackageError::RemoveExistingPath { path: path.to_path_buf(), error }
    })?;

    if metadata.file_type().is_symlink() {
        fs::remove_file(path).map_err(|error| SymlinkPackageError::RemoveExistingPath {
            path: path.to_path_buf(),
            error,
        })?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(|error| SymlinkPackageError::RemoveExistingPath {
            path: path.to_path_buf(),
            error,
        })?;
        return Ok(());
    }

    fs::remove_file(path).map_err(|error| SymlinkPackageError::RemoveExistingPath {
        path: path.to_path_buf(),
        error,
    })?;
    Ok(())
}

fn rename_existing_path(path: &Path) -> Result<(), io::Error> {
    let rename_to = ignored_path(path);
    if rename_to.exists() {
        match fs::symlink_metadata(&rename_to) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&rename_to)?,
            Ok(_) => fs::remove_file(&rename_to)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::rename(path, &rename_to)
}
