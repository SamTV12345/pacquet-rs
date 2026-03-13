use std::{
    io,
    path::{Path, PathBuf},
};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_dir(original, link) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(1314) // ERROR_PRIVILEGE_NOT_HELD
                        | Some(5) // ERROR_ACCESS_DENIED
                ) =>
            {
                junction::create(original, link)
            }
            Err(error) => Err(error),
        }
    }
}

pub fn is_symlink_or_junction(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        if std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Ok(true);
        }
        match junction::exists(path) {
            Ok(value) => Ok(value),
            Err(error) if error.raw_os_error() == Some(4390) => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(windows))]
    {
        Ok(path.is_symlink())
    }
}

pub fn symlink_or_junction_target(path: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        if std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            std::fs::read_link(path)
        } else {
            junction::get_target(path)
        }
    }

    #[cfg(not(windows))]
    {
        std::fs::read_link(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(windows)]
    #[test]
    fn is_symlink_or_junction_returns_false_for_plain_directory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let plain = dir.path().join("plain");
        fs::create_dir(&plain).expect("create plain dir");

        assert!(!is_symlink_or_junction(&plain).expect("check plain dir"));
    }

    #[test]
    fn symlink_or_junction_target_returns_created_link_target() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::create_dir(&target).expect("create target dir");
        symlink_dir(&target, &link).expect("create link");

        let resolved = symlink_or_junction_target(&link).expect("read link target");
        #[cfg(windows)]
        assert_eq!(resolved, target);
        #[cfg(not(windows))]
        assert!(resolved.to_string_lossy().contains("target"));
    }
}
