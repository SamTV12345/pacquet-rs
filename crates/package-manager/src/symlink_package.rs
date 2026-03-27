use crate::{LinkFileError, link_file};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{is_symlink_or_junction, symlink_dir, symlink_or_junction_target};
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashSet,
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

    #[display("Failed to import local file from {from:?} to {to:?}: {error}")]
    ImportLocalFile {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: Box<LinkFileError>,
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

pub fn import_local_package_dir(
    import_method: PackageImportMethod,
    source: &Path,
    destination: &Path,
) -> Result<(), SymlinkPackageError> {
    if let Some(parent) = destination.parent() {
        ensure_parent_dir(parent)?;
    }

    if destination.exists() {
        remove_existing_path(destination)?;
    }

    let source = fs::canonicalize(source).map_err(|error| {
        SymlinkPackageError::CanonicalizePath { path: source.to_path_buf(), error }
    })?;
    import_dir_recursive(import_method, &source, destination)
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
        ensure_parent_dir(parent)?;
    }
    #[cfg(unix)]
    let symlink_target = symlink_path.parent().map_or_else(
        || symlink_target.to_path_buf(),
        |parent| {
            let relative_base = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            let relative_target = if symlink_target.is_absolute() {
                symlink_target.to_path_buf()
            } else {
                parent.join(symlink_target)
            };
            relative_path(&relative_target, &relative_base)
        },
    );
    #[cfg(windows)]
    let symlink_target = {
        // Windows junctions require absolute, normalized target paths.
        // Resolve relative targets against the symlink's parent directory
        // and canonicalize to remove `..` components.
        let absolute = if symlink_target.is_absolute() {
            symlink_target.to_path_buf()
        } else {
            symlink_path.parent().unwrap_or_else(|| Path::new(".")).join(symlink_target)
        };
        fs::canonicalize(&absolute).unwrap_or(absolute)
    };

    force_symlink(&symlink_target, symlink_path)
}

fn ensure_parent_dir(parent: &Path) -> Result<(), SymlinkPackageError> {
    match fs::create_dir_all(parent) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == ErrorKind::AlreadyExists
                && fs::symlink_metadata(parent)
                    .map(|metadata| metadata.is_dir() || metadata.file_type().is_symlink())
                    .unwrap_or(false) =>
        {
            Ok(())
        }
        Err(error) => {
            Err(SymlinkPackageError::CreateParentDir { dir: parent.to_path_buf(), error })
        }
    }
}

fn force_symlink(symlink_target: &Path, symlink_path: &Path) -> Result<(), SymlinkPackageError> {
    let mut replaced_existing = false;

    loop {
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

        if replaced_existing {
            return Err(SymlinkPackageError::SymlinkDir {
                symlink_target: symlink_target.to_path_buf(),
                symlink_path: symlink_path.to_path_buf(),
                error: initial_error,
            });
        }

        if let Err(error) = rename_existing_path(symlink_path) {
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

        replaced_existing = true;
    }
}

fn existing_points_to_target(symlink_target: &Path, symlink_path: &Path) -> bool {
    let compare_target = if symlink_target.is_absolute() {
        symlink_target.to_path_buf()
    } else {
        symlink_path
            .parent()
            .map_or_else(|| symlink_target.to_path_buf(), |parent| parent.join(symlink_target))
    };
    if !is_symlink_or_junction(symlink_path).unwrap_or(false) {
        return false;
    }

    let existing_target = symlink_or_junction_target(symlink_path).ok().map(|target| {
        if target.is_absolute() {
            target
        } else {
            symlink_path.parent().map_or(target.clone(), |parent| parent.join(target))
        }
    });

    existing_target
        .and_then(|existing| fs::canonicalize(existing).ok())
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
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SymlinkPackageError::RemoveExistingPath {
                path: path.to_path_buf(),
                error,
            });
        }
    };

    if metadata.file_type().is_symlink() || is_symlink_or_junction(path).unwrap_or(false) {
        #[cfg(windows)]
        fs::remove_dir(path).map_err(|error| SymlinkPackageError::RemoveExistingPath {
            path: path.to_path_buf(),
            error,
        })?;
        #[cfg(not(windows))]
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
    import_local_package_dir(PackageImportMethod::Copy, source, destination)
}

fn import_dir_recursive(
    import_method: PackageImportMethod,
    source: &Path,
    destination: &Path,
) -> Result<(), SymlinkPackageError> {
    let canonical_source = fs::canonicalize(source).map_err(|error| {
        SymlinkPackageError::CanonicalizePath { path: source.to_path_buf(), error }
    })?;
    let mut pending = vec![(
        canonical_source.clone(),
        destination.to_path_buf(),
        HashSet::from([canonical_source]),
    )];

    while let Some((current_source, current_destination, ancestors)) = pending.pop() {
        fs::create_dir_all(&current_destination).map_err(|error| {
            SymlinkPackageError::CreateDestinationDir { path: current_destination.clone(), error }
        })?;

        let entries = fs::read_dir(&current_source).map_err(|error| {
            SymlinkPackageError::ReadSourceDir { path: current_source.clone(), error }
        })?;

        for entry in entries {
            let entry = entry.map_err(|error| SymlinkPackageError::ReadSourceDir {
                path: current_source.clone(),
                error,
            })?;
            let from = entry.path();
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            let to = current_destination.join(&file_name);
            let file_type = entry.file_type().map_err(|error| {
                SymlinkPackageError::ReadSourceEntryType { path: from.clone(), error }
            })?;
            if file_type.is_dir() && matches!(file_name_str.as_ref(), "node_modules" | ".git") {
                continue;
            }
            let canonical_from =
                if file_type.is_symlink() { fs::canonicalize(&from).ok() } else { None };
            let dir_source = canonical_from
                .as_ref()
                .filter(|target| target.is_dir())
                .or_else(|| file_type.is_dir().then_some(&from));

            if let Some(dir_source) = dir_source {
                let canonical_dir = fs::canonicalize(dir_source).map_err(|error| {
                    SymlinkPackageError::CanonicalizePath { path: dir_source.to_path_buf(), error }
                })?;
                if ancestors.contains(&canonical_dir) {
                    continue;
                }
                let mut child_ancestors = ancestors.clone();
                child_ancestors.insert(canonical_dir.clone());
                pending.push((canonical_dir, to, child_ancestors));
                continue;
            }

            let import_from = canonical_from.as_deref().unwrap_or(&from);
            link_file(import_method, import_from, &to).map_err(|error| {
                SymlinkPackageError::ImportLocalFile {
                    from: import_from.to_path_buf(),
                    to: to.clone(),
                    error: Box::new(error),
                }
            })?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_fs::symlink_dir;
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

    #[test]
    fn import_local_package_dir_with_hardlink_links_files() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let destination = dir.path().join("node_modules/pkg");
        fs::create_dir_all(&source).expect("create source dir");
        fs::write(source.join("index.js"), "module.exports = 1;").expect("write source file");

        import_local_package_dir(PackageImportMethod::Hardlink, &source, &destination)
            .expect("import local package");

        assert!(
            same_file::is_same_file(source.join("index.js"), destination.join("index.js"))
                .expect("compare physical files")
        );
    }

    #[test]
    fn import_local_package_dir_skips_source_node_modules() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let source_dep = source.join("node_modules/dep");
        let destination = dir.path().join("node_modules/pkg");
        fs::create_dir_all(&source_dep).expect("create source node_modules dir");
        fs::write(source.join("index.js"), "module.exports = 1;").expect("write source file");
        fs::write(source_dep.join("index.js"), "module.exports = 'dep';")
            .expect("write nested dep");

        import_local_package_dir(PackageImportMethod::Copy, &source, &destination)
            .expect("import local package");

        assert!(destination.join("index.js").exists());
        assert!(!destination.join("node_modules").exists());
    }

    #[cfg(unix)]
    #[test]
    fn import_local_package_dir_skips_symlink_cycles_without_stack_overflow() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let nested = source.join("nested");
        let destination = dir.path().join("node_modules/pkg");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(nested.join("index.js"), "module.exports = 1;").expect("write file");
        symlink_dir(&source, &nested.join("loop")).expect("create loop");

        import_local_package_dir(PackageImportMethod::Copy, &source, &destination)
            .expect("import local package");

        assert!(destination.join("nested/index.js").exists());
        assert!(!destination.join("nested/loop").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_package_uses_real_parent_when_destination_parent_is_symlink() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("local-pkg");
        let real_parent = dir.path().join("shared-node_modules");
        let symlinked_parent = dir.path().join("app/node_modules");
        let link_path = symlinked_parent.join("local-pkg");

        fs::create_dir_all(&source).expect("create source dir");
        fs::write(source.join("index.js"), "module.exports = 'linked';\n")
            .expect("write source file");
        fs::create_dir_all(real_parent.parent().expect("shared parent")).expect("create parent");
        fs::create_dir_all(&real_parent).expect("create real node_modules dir");
        fs::create_dir_all(symlinked_parent.parent().expect("app parent")).expect("create app dir");
        symlink_dir(&real_parent, &symlinked_parent).expect("symlink node_modules parent");

        symlink_package(&source, &link_path).expect("create package symlink");

        assert_eq!(
            fs::read_to_string(link_path.join("index.js")).expect("read through symlink"),
            "module.exports = 'linked';\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_package_preserves_symlink_identity_of_target() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("local-pkg");
        let source_symlink = dir.path().join("symlink");
        let parent = dir.path().join("app/node_modules");
        let link_path = parent.join("local-pkg");

        fs::create_dir_all(&source).expect("create source dir");
        fs::write(source.join("index.js"), "module.exports = 'linked';\n")
            .expect("write source file");
        fs::create_dir_all(&parent).expect("create node_modules dir");
        symlink_dir(&source, &source_symlink).expect("create source symlink");

        symlink_package(&source_symlink, &link_path).expect("create package symlink");

        let link_target = fs::read_link(&link_path).expect("read symlink target");
        assert!(link_target.to_string_lossy().contains("symlink"));
        assert!(!link_target.to_string_lossy().contains("local-pkg"));
    }

    #[test]
    fn symlink_package_replaces_existing_directory_link() {
        let dir = tempdir().expect("tempdir");
        let source_a = dir.path().join("pkg-a");
        let source_b = dir.path().join("pkg-b");
        let link_path = dir.path().join("app/node_modules/pkg");

        fs::create_dir_all(&source_a).expect("create source a");
        fs::create_dir_all(&source_b).expect("create source b");
        fs::write(source_a.join("index.js"), "module.exports = 'a';\n").expect("write source a");
        fs::write(source_b.join("index.js"), "module.exports = 'b';\n").expect("write source b");
        fs::create_dir_all(link_path.parent().expect("link parent")).expect("create link parent");
        symlink_dir(&source_a, &link_path).expect("create initial package link");

        symlink_package(&source_b, &link_path).expect("replace package link");

        assert_eq!(
            fs::read_to_string(link_path.join("index.js")).expect("read replaced link"),
            "module.exports = 'b';\n"
        );
    }

    #[test]
    fn existing_points_to_target_recognizes_existing_directory_link() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("pkg");
        let link_path = dir.path().join("app/node_modules/pkg");

        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(link_path.parent().expect("parent")).expect("create parent");
        symlink_dir(&source, &link_path).expect("create link");

        assert!(existing_points_to_target(&source, &link_path));
    }
}
