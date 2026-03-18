use clap::Args;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::PackageFilesIndex;
use std::fs::File;

#[derive(Debug, Args)]
pub struct FindHashArgs {
    /// Integrity hash to search for in store index files.
    hash: String,
}

impl FindHashArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let mut matches = Vec::<(String, String, String)>::new();
        let index_root = npmrc.store_dir.version_dir().join("index");

        for path in npmrc.store_dir.index_file_paths() {
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let Ok(index) = serde_json::from_reader::<_, PackageFilesIndex>(file) else {
                continue;
            };
            if !index_contains_hash(&index, &self.hash) {
                continue;
            }
            matches.push((
                index.name.unwrap_or_else(|| "unknown".to_string()),
                index.version.unwrap_or_else(|| "unknown".to_string()),
                path.strip_prefix(&index_root).unwrap_or(&path).display().to_string(),
            ));
        }

        if matches.is_empty() {
            miette::bail!("No package or index file matching this hash was found.");
        }

        for (name, version, path) in matches {
            println!("{name}@{version}  {path}");
        }
        Ok(())
    }
}

fn index_contains_hash(index: &PackageFilesIndex, hash: &str) -> bool {
    if index.files.values().any(|file| file.integrity == hash) {
        return true;
    }
    index.side_effects.as_ref().is_some_and(|side_effects| {
        side_effects.values().any(|diff| {
            diff.added
                .as_ref()
                .is_some_and(|added| added.values().any(|file| file.integrity == hash))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::index_contains_hash;
    use pacquet_store_dir::{PackageFileInfo, PackageFilesIndex, SideEffectsDiff};
    use std::collections::HashMap;

    #[test]
    fn index_contains_hash_checks_files_and_side_effects() {
        let index = PackageFilesIndex {
            name: Some("pkg".to_string()),
            version: Some("1.0.0".to_string()),
            requires_build: None,
            files: HashMap::from([(
                "index.js".to_string(),
                PackageFileInfo {
                    checked_at: None,
                    integrity: "sha512-main".to_string(),
                    mode: 0o644,
                    size: Some(1),
                },
            )]),
            side_effects: Some(HashMap::from([(
                "linux".to_string(),
                SideEffectsDiff {
                    deleted: None,
                    added: Some(HashMap::from([(
                        "addon.node".to_string(),
                        PackageFileInfo {
                            checked_at: None,
                            integrity: "sha512-side".to_string(),
                            mode: 0o755,
                            size: Some(1),
                        },
                    )])),
                },
            )])),
        };

        assert!(index_contains_hash(&index, "sha512-main"));
        assert!(index_contains_hash(&index, "sha512-side"));
        assert!(!index_contains_hash(&index, "sha512-missing"));
    }
}
