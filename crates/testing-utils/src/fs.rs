use std::{fs, io, path::Path};
use walkdir::WalkDir;

pub fn get_filenames_in_folder(path: &Path) -> Vec<String> {
    let mut files = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    files.sort();
    files
}

fn normalized_suffix(path: &Path, prefix: &Path) -> String {
    path.strip_prefix(prefix)
        .expect("strip prefix from path")
        .to_str()
        .expect("convert suffix to UTF-8")
        .replace('\\', "/")
}

pub fn get_all_folders(root: &Path) -> Vec<String> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.expect("access entry"))
        .filter(|entry| entry.file_type().is_dir() || entry.file_type().is_symlink())
        .map(|entry| normalized_suffix(entry.path(), root))
        .filter(|suffix| !suffix.is_empty())
        .collect()
}

pub fn get_all_files(root: &Path) -> Vec<String> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.expect("access entry"))
        .filter(|entry| !entry.file_type().is_dir())
        .map(|entry| normalized_suffix(entry.path(), root))
        .filter(|suffix| !suffix.is_empty())
        .collect()
}

// Helper function to check if a path is a symlink or junction
pub fn is_symlink_or_junction(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        match junction::exists(path) {
            Ok(value) => Ok(value),
            Err(error) if error.raw_os_error() == Some(4390) => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(windows))]
    return Ok(path.is_symlink());
}

/// Check if a file is executable.
#[cfg(unix)]
pub fn is_path_executable(path: &Path) -> bool {
    use std::{fs::File, os::unix::prelude::*};
    let mode = File::open(path)
        .expect("open the file")
        .metadata()
        .expect("get metadata of the file")
        .mode();
    mode & 0b001_001_001 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn is_symlink_or_junction_returns_false_for_plain_directory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let plain = dir.path().join("plain");
        fs::create_dir(&plain).expect("create plain dir");

        assert!(!is_symlink_or_junction(&plain).expect("check plain dir"));
    }
}
