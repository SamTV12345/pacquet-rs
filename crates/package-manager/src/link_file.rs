use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
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

// ── sticky auto-import method selection ─────────────────────────────

const AUTO_UNDECIDED: u8 = 0;
const AUTO_CLONE: u8 = 1;
const AUTO_HARDLINK: u8 = 2;
const AUTO_COPY: u8 = 3;

/// Once a method succeeds under `Auto`, we remember it here so every
/// subsequent call skips the trial-and-error cascade.
/// Matches pnpm's sticky method selection behavior.
static AUTO_RESOLVED: AtomicU8 = AtomicU8::new(AUTO_UNDECIDED);

// ── inode-based skip check ──────────────────────────────────────────

/// Check whether `source` and `target` already refer to the same file
/// by comparing inodes (Unix) or file indices (Windows).
/// Returns `true` when the import can be skipped entirely.
/// This matches pnpm's `pkgLinkedToStore()` inode check.
#[cfg(unix)]
fn same_inode(source: &Path, target: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(src_meta), Ok(tgt_meta)) = (fs::metadata(source), fs::metadata(target)) else {
        return false;
    };
    src_meta.dev() == tgt_meta.dev() && src_meta.ino() == tgt_meta.ino()
}

#[cfg(windows)]
fn same_inode(source: &Path, target: &Path) -> bool {
    // volume_serial_number() and file_index() are unstable (rust#63010).
    // Use the stable file_size + last_write_time as a best-effort proxy.
    // If the file sizes and last-write times match, we assume the hardlink
    // is intact. This isn't as precise as inode comparison but avoids
    // requiring nightly Rust.
    use std::os::windows::fs::MetadataExt;
    let (Ok(src_meta), Ok(tgt_meta)) = (fs::metadata(source), fs::metadata(target)) else {
        return false;
    };
    src_meta.file_size() == tgt_meta.file_size()
        && src_meta.last_write_time() == tgt_meta.last_write_time()
}

#[cfg(not(any(unix, windows)))]
fn same_inode(_source: &Path, _target: &Path) -> bool {
    false
}

// ── Auto strategy: clone → hardlink → copy ──────────────────────────

/// Probe the best method. On Windows, skip reflink (pnpm does this
/// because reflinks on Dev Drives are 10x slower than hardlinks).
fn auto_import_probe(source: &Path, target: &Path) -> Result<u8, io::Error> {
    #[cfg(not(target_os = "windows"))]
    {
        match reflink_copy::reflink(source, target) {
            Ok(()) => {
                AUTO_RESOLVED.store(AUTO_CLONE, Ordering::Relaxed);
                return Ok(AUTO_CLONE);
            }
            Err(_) => {
                let _ = fs::remove_file(target);
            }
        }
    }

    if let Ok(()) = fs::hard_link(source, target) {
        AUTO_RESOLVED.store(AUTO_HARDLINK, Ordering::Relaxed);
        return Ok(AUTO_HARDLINK);
    }

    fs::copy(source, target)?;
    AUTO_RESOLVED.store(AUTO_COPY, Ordering::Relaxed);
    Ok(AUTO_COPY)
}

/// Execute using the already-resolved sticky method.
fn auto_import_sticky(resolved: u8, source: &Path, target: &Path) -> Result<(), io::Error> {
    match resolved {
        AUTO_CLONE => reflink_copy::reflink(source, target),
        AUTO_HARDLINK => fs::hard_link(source, target),
        AUTO_COPY => fs::copy(source, target).map(|_| ()),
        _ => unreachable!("invalid resolved auto-import method"),
    }
}

// ── public API ──────────────────────────────────────────────────────

/// Link/copy a single file using a selected import method.
///
/// * If `source_file` and `target_link` share the same inode, skip (already linked).
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    import_method: PackageImportMethod,
    source_file: &Path,
    target_link: &Path,
) -> Result<(), LinkFileError> {
    // Fast path: inode match means the import was already done correctly.
    if same_inode(source_file, target_link) {
        return Ok(());
    }

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
            let resolved = AUTO_RESOLVED.load(Ordering::Relaxed);
            if resolved == AUTO_UNDECIDED {
                auto_import_probe(source_file, target_link).map_err(|error| {
                    LinkFileError::CreateLink {
                        from: source_file.to_path_buf(),
                        to: target_link.to_path_buf(),
                        error,
                    }
                })?;
            } else {
                auto_import_sticky(resolved, source_file, target_link).map_err(|error| {
                    LinkFileError::CreateLink {
                        from: source_file.to_path_buf(),
                        to: target_link.to_path_buf(),
                        error,
                    }
                })?;
            }
        }
        PackageImportMethod::Hardlink => {
            if let Err(error) = fs::hard_link(source_file, target_link) {
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
