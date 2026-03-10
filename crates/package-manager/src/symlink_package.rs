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

    #[display("Failed to read source directory at {path:?}: {error}")]
    ReadSourceDir {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to read source entry type at {path:?}: {error}")]
    ReadSourceEntryType {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to create destination directory at {path:?}: {error}")]
    CreateDestinationDir {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to copy file from {from:?} to {to:?}: {error}")]
    CopyFile {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to canonicalize path at {path:?}: {error}")]
    CanonicalizePath {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Link package from `symlink_target` to `symlink_path`.
///
/// If `symlink` is false, this creates a physical directory copy at `symlink_path`.
pub fn link_package(
    symlink: bool,
    symlink_target: &Path,
    symlink_path: &Path,
) -> Result<(), SymlinkPackageError> {
    if symlink {
        return symlink_package(symlink_target, symlink_path);
    }

    copy_package_dir(symlink_target, symlink_path)
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
    } else if let Err(error) = rename_existing_path(symlink_path) {
        if error.kind() == ErrorKind::NotFound {
            return Err(SymlinkPackageError::SymlinkDir {
                symlink_target: symlink_target.to_path_buf(),
                symlink_path: symlink_path.to_path_buf(),
                error: initial_error,
            });
        }

        // Windows often returns EPERM/PermissionDenied when renaming existing junctions.
        // Fall back to direct deletion before retrying symlink creation.
        if let Err(remove_error) = remove_existing_path(symlink_path) {
            return Err(SymlinkPackageError::RenameExistingPath {
                path: symlink_path.to_path_buf(),
                rename_to: ignored_path(symlink_path),
                error: io::Error::new(
                    error.kind(),
                    format!("{error}; fallback remove failed: {remove_error}"),
                ),
            });
        }
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

fn copy_package_dir(source: &Path, destination: &Path) -> Result<(), SymlinkPackageError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| SymlinkPackageError::CreateParentDir {
            dir: parent.to_path_buf(),
            error,
        })?;
    }

    if destination.exists() {
        remove_existing_path(destination)?;
    }

    let source = fs::canonicalize(source).map_err(|error| {
        SymlinkPackageError::CanonicalizePath { path: source.to_path_buf(), error }
    })?;
    copy_dir_recursive(&source, destination)
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), SymlinkPackageError> {
    fs::create_dir_all(destination).map_err(|error| SymlinkPackageError::CreateDestinationDir {
        path: destination.to_path_buf(),
        error,
    })?;

    let entries = fs::read_dir(source).map_err(|error| SymlinkPackageError::ReadSourceDir {
        path: source.to_path_buf(),
        error,
    })?;

    for entry in entries {
        let entry = entry.map_err(|error| SymlinkPackageError::ReadSourceDir {
            path: source.to_path_buf(),
            error,
        })?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            SymlinkPackageError::ReadSourceEntryType { path: from.clone(), error }
        })?;
        let canonical_from =
            if file_type.is_symlink() { fs::canonicalize(&from).ok() } else { None };
        if file_type.is_dir() || canonical_from.as_ref().is_some_and(|target| target.is_dir()) {
            copy_dir_recursive(canonical_from.as_deref().unwrap_or(&from), &to)?;
            continue;
        }
        let copy_from = canonical_from.as_deref().unwrap_or(&from);
        fs::copy(copy_from, &to).map_err(|error| SymlinkPackageError::CopyFile {
            from: copy_from.to_path_buf(),
            to: to.clone(),
            error,
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn link_package_without_symlink_copies_directory() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let destination = dir.path().join("node_modules/pkg");
        fs::create_dir_all(source.join("lib")).expect("create source dir");
        fs::write(source.join("package.json"), "{\"name\":\"pkg\",\"version\":\"1.0.0\"}")
            .expect("write package.json");
        fs::write(source.join("lib/index.js"), "module.exports = 1;").expect("write nested file");

        link_package(false, &source, &destination).expect("copy package");

        assert!(destination.join("package.json").exists());
        assert!(destination.join("lib/index.js").exists());
        let metadata = fs::symlink_metadata(&destination).expect("read destination metadata");
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
    }
}
