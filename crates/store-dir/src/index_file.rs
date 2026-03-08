use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{EnsureFileError, ensure_file};
use serde::{Deserialize, Serialize};
use ssri::{Algorithm, Integrity};
use std::{collections::HashMap, fs::File, path::PathBuf};

impl StoreDir {
    /// Path to an index file of a tarball.
    pub fn index_file_path(&self, tarball_integrity: &Integrity, package_id: &str) -> PathBuf {
        let (algorithm, hex) = tarball_integrity.to_hex();
        assert!(
            matches!(algorithm, Algorithm::Sha512 | Algorithm::Sha1),
            "Only Sha1 and Sha512 are supported. {algorithm} isn't",
        ); // TODO: propagate this error
        let hex = &hex[..hex.len().min(64)];
        let sanitized_pkg_id = package_id
            .chars()
            .map(|ch| match ch {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '+',
                other => other,
            })
            .collect::<String>();
        let file_name = format!("{}-{sanitized_pkg_id}.json", &hex[2..]);
        self.version_dir().join("index").join(&hex[..2]).join(file_name)
    }
}

/// Content of an index file (`$STORE_DIR/v10/index/*/*.json`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilesIndex {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_build: Option<bool>,
    pub files: HashMap<String, PackageFileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<HashMap<String, SideEffectsDiff>>,
}

/// Value of the [`files`](PackageFilesIndex::files) map.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFileInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u128>,
    pub integrity: String,
    pub mode: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideEffectsDiff {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<HashMap<String, PackageFileInfo>>,
}

/// Error type of [`StoreDir::write_index_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteIndexFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a JSON file that indexes files in a tarball to the store directory.
    pub fn write_index_file(
        &self,
        integrity: &Integrity,
        package_id: &str,
        index_content: &PackageFilesIndex,
    ) -> Result<(), WriteIndexFileError> {
        let file_path = self.index_file_path(integrity, package_id);
        let index_content =
            serde_json::to_string(&index_content).expect("convert a TarballIndex to JSON");
        ensure_file(&file_path, index_content.as_bytes(), Some(0o666))
            .map_err(WriteIndexFileError::WriteFile)
    }

    /// Read a JSON index file of a tarball from the store directory.
    pub fn read_index_file(
        &self,
        integrity: &Integrity,
        package_id: &str,
    ) -> Option<PackageFilesIndex> {
        let path = self.index_file_path(integrity, package_id);
        let file = File::open(path).ok()?;
        serde_json::from_reader(file).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssri::IntegrityOpts;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn index_file_path() {
        let store_dir = StoreDir::new("STORE_DIR");
        let integrity =
            IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(b"TARBALL CONTENT").result();
        let received = store_dir.index_file_path(&integrity, "@scope/pkg@1.0.0");
        let expected = "STORE_DIR/v10/index/bc/d60799116ebef60071b9f2c7dafd7e2a4e1b366e341f750b2de52dd6995ab4-@scope+pkg@1.0.0.json";
        let expected: PathBuf = expected.split('/').collect();
        assert_eq!(&received, &expected);
    }

    #[test]
    fn serialize_side_effects_shape() {
        let value = PackageFilesIndex {
            name: Some("pkg".to_string()),
            version: Some("1.0.0".to_string()),
            requires_build: Some(true),
            files: HashMap::new(),
            side_effects: Some(HashMap::from([(
                "linux;x64;node20".to_string(),
                SideEffectsDiff {
                    deleted: Some(vec!["a.js".to_string()]),
                    added: Some(HashMap::from([(
                        "b.js".to_string(),
                        PackageFileInfo {
                            checked_at: Some(1),
                            integrity: "sha512-abc".to_string(),
                            mode: 0o644,
                            size: Some(1),
                        },
                    )])),
                },
            )])),
        };

        let json = serde_json::to_value(value).expect("serialize index");
        assert!(json.get("sideEffects").is_some());
        assert!(json["sideEffects"]["linux;x64;node20"].get("deleted").is_some());
        assert!(json["sideEffects"]["linux;x64;node20"].get("added").is_some());
    }

    #[test]
    fn write_and_read_index_roundtrip() {
        let dir = tempdir().expect("create tempdir");
        let store_dir = StoreDir::new(dir.path());
        let integrity = IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(b"hello").result();
        let package_id = "@scope/pkg@1.0.0";

        let expected = PackageFilesIndex {
            name: Some("@scope/pkg".to_string()),
            version: Some("1.0.0".to_string()),
            requires_build: Some(true),
            files: HashMap::new(),
            side_effects: None,
        };

        store_dir.write_index_file(&integrity, package_id, &expected).expect("write index");

        let received = store_dir.read_index_file(&integrity, package_id).expect("read index");
        assert_eq!(received.name, expected.name);
        assert_eq!(received.version, expected.version);
        assert_eq!(received.requires_build, expected.requires_build);
        assert_eq!(received.files.len(), expected.files.len());
    }
}
