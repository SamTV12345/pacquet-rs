use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry::{Package, RegistryError};
use std::path::{Path, PathBuf};

fn registry_namespace(registry: &str) -> String {
    let trimmed = registry.trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme.replace(':', "+")
}

fn package_cache_file(cache_dir: &Path, registry: &str, package_name: &str) -> PathBuf {
    let encoded_name = package_name.replace('/', "%2f");
    cache_dir
        .join("metadata-v1.3")
        .join(registry_namespace(registry))
        .join(format!("{encoded_name}.json"))
}

pub(crate) fn metadata_cache_file(config: &Npmrc, package_name: &str) -> PathBuf {
    let registry = config.registry_for_package_name(package_name);
    package_cache_file(&config.cache_dir, &registry, package_name)
}

fn read_cached_package(cache_file: &Path) -> Option<Package> {
    let text = std::fs::read_to_string(cache_file).ok()?;
    serde_json::from_str::<Package>(&text).ok()
}

pub(crate) fn read_cached_package_from_config(
    config: &Npmrc,
    package_name: &str,
) -> Option<Package> {
    read_cached_package(&metadata_cache_file(config, package_name))
}

fn write_cached_package(cache_file: &Path, package: &Package) {
    if let Some(parent) = cache_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(package) {
        let _ = std::fs::write(cache_file, text);
    }
}

pub(crate) async fn fetch_package_from_registry_and_cache(
    config: &Npmrc,
    http_client: &ThrottledClient,
    package_name: &str,
) -> Result<Package, RegistryError> {
    let registry = config.registry_for_package_name(package_name);
    let auth_header = config.auth_header_for_url(&format!("{registry}{package_name}"));
    let package =
        Package::fetch_from_registry(package_name, http_client, &registry, auth_header.as_deref())
            .await?;
    write_cached_package(&metadata_cache_file(config, package_name), &package);
    Ok(package)
}

pub(crate) async fn fetch_package_with_metadata_cache(
    config: &Npmrc,
    http_client: &ThrottledClient,
    package_name: &str,
    prefer_offline: bool,
    offline: bool,
) -> Package {
    let cache_file = metadata_cache_file(config, package_name);
    let cached = read_cached_package(&cache_file);
    if (prefer_offline || offline)
        && let Some(cached) = cached.clone()
    {
        return cached;
    }
    if offline {
        panic!("Failed to resolve {package_name} in package mirror {}", cache_file.display());
    }

    match fetch_package_from_registry_and_cache(config, http_client, package_name).await {
        Ok(package) => package,
        Err(error) => {
            if let Some(cached) = cached {
                crate::progress_reporter::warn(&format!(
                    "Failed to fetch package metadata from registry for {package_name}, using cached metadata: {error}"
                ));
                return cached;
            }
            panic!("fetch package metadata from registry: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use pacquet_network::ThrottledClient;
    use pacquet_npmrc::Npmrc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn falls_back_to_cached_metadata_when_registry_fetch_fails() {
        let dir = tempdir().expect("tempdir");
        let mut server = Server::new_async().await;
        let _mock = server.mock("GET", "/pkg").with_status(500).create_async().await;

        let mut config = Npmrc::new();
        config.registry = format!("{}/", server.url());
        config.cache_dir = dir.path().join("cache");

        let cached = serde_json::from_value::<Package>(serde_json::json!({
            "name": "pkg",
            "dist-tags": { "latest": "1.0.0" },
            "versions": {
                "1.0.0": {
                    "name": "pkg",
                    "version": "1.0.0",
                    "dist": {
                        "tarball": "https://registry.example/pkg/-/pkg-1.0.0.tgz"
                    }
                }
            }
        }))
        .expect("deserialize cached package");
        let cache_file = metadata_cache_file(&config, "pkg");
        std::fs::create_dir_all(cache_file.parent().expect("cache dir")).expect("create cache dir");
        std::fs::write(&cache_file, serde_json::to_string(&cached).expect("serialize cached"))
            .expect("write cache");

        let package = fetch_package_with_metadata_cache(
            &config,
            &ThrottledClient::new_from_cpu_count(),
            "pkg",
            false,
            false,
        )
        .await;

        assert_eq!(package.name, "pkg");
        assert_eq!(package.latest().version.to_string(), "1.0.0");
    }
}
