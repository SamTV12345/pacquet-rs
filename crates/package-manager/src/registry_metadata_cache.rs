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
    package_cache_file(&config.cache_dir, &config.registry, package_name)
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
    let auth_header = config.auth_header_for_url(&format!("{}{}", &config.registry, package_name));
    let package = Package::fetch_from_registry(
        package_name,
        http_client,
        &config.registry,
        auth_header.as_deref(),
    )
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
                tracing::warn!(
                    target: "pacquet::install",
                    package = package_name,
                    "Failed to fetch package metadata from registry, using cached metadata: {error}"
                );
                return cached;
            }
            panic!("fetch package metadata from registry: {error}");
        }
    }
}
