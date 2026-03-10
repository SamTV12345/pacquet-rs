use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Error type for [`link_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkFileError {
    #[display("cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("unsupported clone operation from {from:?} to {to:?}: {error}")]
    CloneUnsupported {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Link/copy a single file using a selected import method.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    import_method: PackageImportMethod,
    source_file: &Path,
    target_link: &Path,
) -> Result<(), LinkFileError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| LinkFileError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    match import_method {
        PackageImportMethod::Auto => {
            reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
                LinkFileError::CreateLink {
                    from: source_file.to_path_buf(),
                    to: target_link.to_path_buf(),
                    error,
                }
            })?;
        }
        PackageImportMethod::Hardlink => {
            if let Err(error) = fs::hard_link(source_file, target_link) {
                // pnpm still proceeds across devices/filesystems where hardlinks are unavailable.
                fs::copy(source_file, target_link).map_err(|copy_error| {
                    LinkFileError::CreateLink {
                        from: source_file.to_path_buf(),
                        to: target_link.to_path_buf(),
                        error: io::Error::new(
                            error.kind(),
                            format!("{error}; fallback copy failed: {copy_error}"),
                        ),
                    }
                })?;
            }
        }
        PackageImportMethod::Copy => {
            fs::copy(source_file, target_link).map_err(|error| LinkFileError::CreateLink {
                from: source_file.to_path_buf(),
                to: target_link.to_path_buf(),
                error,
            })?;
        }
        PackageImportMethod::Clone => {
            reflink_copy::reflink(source_file, target_link).map_err(|error| {
                LinkFileError::CloneUnsupported {
                    from: source_file.to_path_buf(),
                    to: target_link.to_path_buf(),
                    error,
                }
            })?;
        }
        PackageImportMethod::CloneOrCopy => {
            reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
                LinkFileError::CreateLink {
                    from: source_file.to_path_buf(),
                    to: target_link.to_path_buf(),
                    error,
                }
            })?;
        }
    }

    Ok(())
}
