use derive_more::From;
use serde::{Deserialize, Serialize};
use sha2::{Sha512, digest};
use ssri::Integrity;
use std::path::{self, Path, PathBuf};

/// Content hash of a file.
pub type FileHash = digest::Output<Sha512>;

/// Represent a store directory.
///
/// * The store directory stores all files that were acquired by installing packages with pacquet or pnpm.
/// * The files in `node_modules` directories are hardlinks or reflinks to the files in the store directory.
/// * The store directory can and often act as a global shared cache of all installation of different workspaces.
/// * The location of the store directory can be customized by `store-dir` field.
#[derive(Debug, Clone, PartialEq, Eq, From, Deserialize, Serialize)]
#[serde(transparent)]
pub struct StoreDir {
    /// Path to the root of the store directory from which all sub-paths are derived.
    ///
    /// Consumer of this struct should interact with the sub-paths instead of this path.
    root: PathBuf,
}

impl StoreDir {
    const STORE_LAYOUT_VERSION: &'static str = "v10";

    /// Construct an instance of [`StoreDir`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        root.into().into()
    }

    /// Create an object that [displays](std::fmt::Display) the root of the store directory.
    pub fn display(&self) -> path::Display<'_> {
        self.root.display()
    }

    /// Get `{store}/v10`.
    fn v10(&self) -> PathBuf {
        self.root.join(Self::STORE_LAYOUT_VERSION)
    }

    pub fn version_dir(&self) -> PathBuf {
        self.v10()
    }

    pub(crate) fn root_dir(&self) -> &PathBuf {
        &self.root
    }

    /// The directory that contains all files from the once-installed packages.
    fn files(&self) -> PathBuf {
        self.v10().join("files")
    }

    /// Path to a file in the store directory.
    ///
    /// **Parameters:**
    /// * `head` is the first 2 hexadecimal digit of the file address.
    /// * `tail` is the rest of the address and an optional suffix.
    fn file_path_by_head_tail(&self, head: &str, tail: &str) -> PathBuf {
        self.files().join(head).join(tail)
    }

    /// Path to a file in the store directory.
    pub(crate) fn file_path_by_hex_str(&self, hex: &str, suffix: &'static str) -> PathBuf {
        let head = &hex[..2];
        let middle = &hex[2..];
        let tail = format!("{middle}{suffix}");
        self.file_path_by_head_tail(head, &tail)
    }

    /// Path to the temporary directory inside the store.
    pub fn tmp(&self) -> PathBuf {
        self.v10().join("tmp")
    }

    /// Resolve a CAS file path from an integrity string (`sha512-...`).
    pub fn cas_file_path_by_integrity(&self, integrity: &str, executable: bool) -> Option<PathBuf> {
        let integrity = integrity.parse::<Integrity>().ok()?;
        let (_, hex) = integrity.to_hex();
        let suffix = if executable { "-exec" } else { "" };
        Some(self.file_path_by_hex_str(&hex, suffix))
    }

    /// Iterate all index JSON files under `{store}/v10/index`.
    pub fn index_file_paths(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, acc: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    walk(&path, acc);
                    continue;
                }
                if file_type.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("json")
                {
                    acc.push(path);
                }
            }
        }

        let mut paths = Vec::new();
        walk(&self.version_dir().join("index"), &mut paths);
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use ssri::{Algorithm, IntegrityOpts};
    use tempfile::tempdir;

    #[test]
    fn file_path_by_head_tail() {
        let received = "/home/user/.local/share/pnpm/store"
            .pipe(StoreDir::new)
            .file_path_by_head_tail("3e", "f722d37b016c63ac0126cfdcec");
        let expected = PathBuf::from(
            "/home/user/.local/share/pnpm/store/v10/files/3e/f722d37b016c63ac0126cfdcec",
        );
        assert_eq!(&received, &expected);
    }

    #[test]
    fn tmp() {
        let received = StoreDir::new("/home/user/.local/share/pnpm/store").tmp();
        let expected = PathBuf::from("/home/user/.local/share/pnpm/store/v10/tmp");
        assert_eq!(&received, &expected);
    }

    #[test]
    fn cas_file_path_by_integrity() {
        let store = StoreDir::new("/tmp/store");
        let integrity =
            IntegrityOpts::new().algorithm(Algorithm::Sha512).chain("hello").result().to_string();
        let path =
            store.cas_file_path_by_integrity(&integrity, false).expect("resolve path by integrity");
        let normalized = path.to_string_lossy().replace('\\', "/");
        assert!(normalized.contains("/v10/files/"));
    }

    #[test]
    fn index_file_paths_discovers_nested_json_files() {
        let dir = tempdir().expect("create tempdir");
        let store = StoreDir::new(dir.path());
        let index_dir = store.version_dir().join("index").join("ab");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        std::fs::write(index_dir.join("one.json"), "{}").expect("write json file");
        std::fs::write(index_dir.join("two.txt"), "x").expect("write text file");

        let mut paths = store.index_file_paths();
        paths.sort();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("one.json"));
    }

    #[test]
    fn index_file_paths_does_not_infinite_loop_on_symlink_cycle() {
        let dir = tempdir().expect("create tempdir");
        let store = StoreDir::new(dir.path());
        let index_dir = store.version_dir().join("index");
        let dir_a = index_dir.join("aa");
        let dir_b = index_dir.join("bb");
        std::fs::create_dir_all(&dir_a).expect("create dir_a");
        // Create a symlink cycle: bb -> aa, then aa/loop -> bb
        // On Windows, symlinks require admin privileges, so use junctions instead.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir_a, &dir_b).expect("symlink bb -> aa");
            std::os::unix::fs::symlink(&dir_b, dir_a.join("loop")).expect("symlink aa/loop -> bb");
        }
        #[cfg(windows)]
        {
            junction::create(&dir_a, &dir_b).expect("junction bb -> aa");
            junction::create(&dir_b, &dir_a.join("loop")).expect("junction aa/loop -> bb");
        }
        // Write a real json file so the result is not empty
        std::fs::write(dir_a.join("real.json"), "{}").expect("write json");

        // The walk function uses `entry.file_type()` which reports symlinks as
        // symlinks (not dirs), so it naturally skips them. This test verifies
        // the function terminates and finds only the real file.
        let paths = store.index_file_paths();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("real.json"));
    }

    #[test]
    fn store_path_construction() {
        let store = StoreDir::new("/custom/store/path");
        assert_eq!(store.version_dir(), PathBuf::from("/custom/store/path/v10"));
        assert_eq!(store.tmp(), PathBuf::from("/custom/store/path/v10/tmp"));

        // Verify files sub-path via file_path_by_head_tail
        let file_path = store.file_path_by_head_tail("ab", "cdef0123");
        assert_eq!(file_path, PathBuf::from("/custom/store/path/v10/files/ab/cdef0123"));

        // Verify file_path_by_hex_str with suffix
        let hex_path = store.file_path_by_hex_str("abcdef0123456789", "-exec");
        assert_eq!(hex_path, PathBuf::from("/custom/store/path/v10/files/ab/cdef0123456789-exec"));
    }
}
