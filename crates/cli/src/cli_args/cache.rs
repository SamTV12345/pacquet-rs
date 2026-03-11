use clap::Args;
use glob::Pattern;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Cache subcommand.
    pub command: String,

    /// Optional package name or glob pattern.
    pub args: Vec<String>,
}

impl CacheArgs {
    pub fn run(self, config: &Npmrc) -> miette::Result<()> {
        let cache_dir = config.cache_dir.join("metadata-v1.3");
        match self.command.as_str() {
            "list" => {
                let filter = self.args.first().map(String::as_str);
                if self.args.len() > 1 {
                    miette::bail!("`pacquet cache list` accepts at most one pattern");
                }
                let output = cache_list(&cache_dir, filter)?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            "list-registries" => {
                let output = cache_list_registries(&cache_dir)?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            "delete" => {
                let output = cache_delete(&cache_dir, &self.args)?;
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            "view" => {
                let package_name = match self.args.as_slice() {
                    [package_name] => package_name,
                    [] => miette::bail!("`pacquet cache view` requires the package name"),
                    _ => miette::bail!("`pacquet cache view` only accepts one package name"),
                };
                let output =
                    cache_view(&cache_dir, &config.store_dir.display().to_string(), package_name)?;
                println!("{output}");
            }
            _ => {
                miette::bail!("Unsupported cache command `{}`", self.command);
            }
        }
        Ok(())
    }
}

fn cache_list(cache_dir: &Path, filter: Option<&str>) -> miette::Result<String> {
    let matcher = filter.map(Pattern::new).transpose().into_diagnostic()?;
    let mut entries = Vec::new();
    for registry_dir in registry_dirs(cache_dir)? {
        let registry_name = registry_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| miette::miette!("invalid registry cache directory"))?;
        let files = fs::read_dir(&registry_dir).into_diagnostic()?;
        for file in files.flatten() {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(package_name) = decode_cache_file_name(file_name) else {
                continue;
            };
            if matcher.as_ref().is_some_and(|matcher| !matcher.matches(&package_name)) {
                continue;
            }
            entries.push(format!("{registry_name}/{file_name}"));
        }
    }
    entries.sort();
    Ok(entries.join("\n"))
}

fn cache_list_registries(cache_dir: &Path) -> miette::Result<String> {
    let mut registries = registry_dirs(cache_dir)?
        .into_iter()
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()).map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    registries.sort();
    Ok(registries.join("\n"))
}

fn cache_delete(cache_dir: &Path, patterns: &[String]) -> miette::Result<String> {
    if patterns.is_empty() {
        miette::bail!("`pacquet cache delete` requires at least one pattern");
    }
    let matchers = patterns
        .iter()
        .map(|pattern| Pattern::new(pattern))
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    let mut deleted = Vec::new();
    for registry_dir in registry_dirs(cache_dir)? {
        let registry_name = registry_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| miette::miette!("invalid registry cache directory"))?
            .to_string();
        let files = fs::read_dir(&registry_dir).into_diagnostic()?;
        for file in files.flatten() {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(package_name) = decode_cache_file_name(file_name) else {
                continue;
            };
            if !matchers.iter().any(|matcher| matcher.matches(&package_name)) {
                continue;
            }
            fs::remove_file(&path)
                .into_diagnostic()
                .wrap_err_with(|| format!("delete cache file {}", path.display()))?;
            deleted.push(format!("{registry_name}/{file_name}"));
        }
    }
    deleted.sort();
    Ok(deleted.join("\n"))
}

#[derive(Debug, Serialize)]
struct CacheViewRegistryEntry {
    #[serde(rename = "cachedVersions")]
    cached_versions: Vec<String>,
    #[serde(rename = "nonCachedVersions")]
    non_cached_versions: Vec<String>,
}

fn cache_view(cache_dir: &Path, store_dir: &str, package_name: &str) -> miette::Result<String> {
    let mut result = BTreeMap::<String, CacheViewRegistryEntry>::new();
    let cached_versions = cached_versions_for_package(Path::new(store_dir), package_name)?;
    let encoded_name = encode_package_name(package_name);
    for registry_dir in registry_dirs(cache_dir)? {
        let registry_name = registry_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| miette::miette!("invalid registry cache directory"))?
            .to_string();
        let cache_file = registry_dir.join(format!("{encoded_name}.json"));
        if !cache_file.is_file() {
            continue;
        }
        let package: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&cache_file)
                .into_diagnostic()
                .wrap_err_with(|| format!("read cache file {}", cache_file.display()))?,
        )
        .into_diagnostic()
        .wrap_err_with(|| format!("parse cache file {}", cache_file.display()))?;
        let mut all_versions = package
            .get("versions")
            .and_then(serde_json::Value::as_object)
            .map(|versions| versions.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        all_versions.sort();
        let cached = all_versions
            .iter()
            .filter(|version| cached_versions.contains(version.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let non_cached = all_versions
            .into_iter()
            .filter(|version| !cached_versions.contains(version.as_str()))
            .collect::<Vec<_>>();
        result.insert(
            registry_name.replace('+', ":"),
            CacheViewRegistryEntry { cached_versions: cached, non_cached_versions: non_cached },
        );
    }
    serde_json::to_string_pretty(&result).into_diagnostic().wrap_err("serialize cache view")
}

fn cached_versions_for_package(
    store_dir: &Path,
    package_name: &str,
) -> miette::Result<BTreeSet<String>> {
    let mut versions = BTreeSet::new();
    for index_path in walk_index_files(store_dir)? {
        let package_files: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&index_path)
                .into_diagnostic()
                .wrap_err_with(|| format!("read store index {}", index_path.display()))?,
        )
        .into_diagnostic()
        .wrap_err_with(|| format!("parse store index {}", index_path.display()))?;
        let Some(name) = package_files.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(version) = package_files.get("version").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if name == package_name {
            versions.insert(version.to_string());
        }
    }
    Ok(versions)
}

fn walk_index_files(store_dir: &Path) -> miette::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(store_dir, &mut files);
    Ok(files)
}

fn registry_dirs(cache_dir: &Path) -> miette::Result<Vec<PathBuf>> {
    if !cache_dir.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_dir(cache_dir)
        .into_diagnostic()?
        .flatten()
        .filter_map(|entry| {
            entry.file_type().ok().filter(|file_type| file_type.is_dir()).map(|_| entry.path())
        })
        .collect())
}

fn encode_package_name(package_name: &str) -> String {
    package_name.replace('/', "%2f")
}

fn decode_cache_file_name(file_name: &str) -> Option<String> {
    file_name.strip_suffix(".json").map(|name| name.replace("%2f", "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn cache_list_filters_by_glob() {
        let dir = tempdir().expect("tempdir");
        let registry_dir = dir.path().join("metadata-v1.3/registry.npmjs.org");
        fs::create_dir_all(&registry_dir).expect("create registry dir");
        fs::write(registry_dir.join("is-negative.json"), "{}").expect("write cache file");
        fs::write(registry_dir.join("is-positive.json"), "{}").expect("write cache file");

        let listed =
            cache_list(&dir.path().join("metadata-v1.3"), Some("*-positive")).expect("list cache");
        assert_eq!(listed, "registry.npmjs.org/is-positive.json");
    }

    #[test]
    fn decode_cache_file_name_restores_scopes() {
        assert_eq!(decode_cache_file_name("@scope%2fpkg.json"), Some("@scope/pkg".to_string()));
    }
}
