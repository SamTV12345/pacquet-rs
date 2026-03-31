use std::collections::HashMap;

use pacquet_network::ThrottledClient;
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{NetworkError, PackageTag, RegistryError, package_distribution::PackageDistribution};

/// Metadata about a single peer dependency (e.g. whether it is optional).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeerDependencyMeta {
    #[serde(default)]
    pub optional: bool,
}

/// Deserialize a field that should be a map but may be an array or other type
/// in legacy npm packages. Returns None for non-map values.
fn deserialize_optional_map<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        Some(Value::Object(map)) => {
            let result = map
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect();
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageVersion {
    pub name: String,
    pub version: node_semver::Version,
    pub dist: PackageDistribution,
    #[serde(default, deserialize_with = "deserialize_optional_map")]
    pub dependencies: Option<HashMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_optional_map")]
    pub optional_dependencies: Option<HashMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_optional_map")]
    pub dev_dependencies: Option<HashMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_optional_map")]
    pub peer_dependencies: Option<HashMap<String, String>>,
    #[serde(default)]
    pub peer_dependencies_meta: Option<HashMap<String, PeerDependencyMeta>>,
    #[serde(default, deserialize_with = "deserialize_optional_map")]
    pub engines: Option<HashMap<String, String>>,
    #[serde(default)]
    pub cpu: Option<Vec<String>>,
    #[serde(default)]
    pub os: Option<Vec<String>>,
    #[serde(default)]
    pub libc: Option<Vec<String>>,
    #[serde(default)]
    pub deprecated: Option<String>,
    #[serde(default)]
    pub bin: Option<serde_json::Value>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<Value>,
}

impl PartialEq for PackageVersion {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist
    }
}

impl PackageVersion {
    pub async fn fetch_from_registry(
        name: &str,
        tag: PackageTag,
        http_client: &ThrottledClient,
        registry: &str,
        auth_header: Option<&str>,
    ) -> Result<Self, RegistryError> {
        let url = || format!("{registry}{name}/{tag}");
        let network_error = |error| NetworkError { error, url: url() };

        let body = http_client
            .run_with_permit_for_url(&url(), |client| {
                let mut request = client.get(url()).header(
                    "accept",
                    "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*",
                );
                request = request.header("accept-encoding", "identity");
                if let Some(auth_header) = auth_header {
                    request = request.header("authorization", auth_header);
                }
                async {
                    let response = request.send().await?.error_for_status()?;
                    response.bytes().await
                }
            })
            .await
            .map_err(network_error)?;
        body.pipe(|body| serde_json::from_slice::<PackageVersion>(&body))
            .map_err(|error| RegistryError::Serialization(error.to_string()))
    }

    pub fn to_virtual_store_name(&self) -> String {
        format!("{0}@{1}", self.name.replace('/', "+"), self.version)
    }

    pub fn as_tarball_url(&self) -> &str {
        self.dist.tarball.as_str()
    }

    pub fn dependencies(
        &self,
        with_peer_dependencies: bool,
    ) -> impl Iterator<Item = (&'_ str, &'_ str)> {
        let dependencies = self.dependencies.iter().flatten();
        let optional_dependencies = self.optional_dependencies.iter().flatten();

        let peer_dependencies = with_peer_dependencies
            .then_some(&self.peer_dependencies)
            .into_iter()
            .flatten()
            .flatten();

        dependencies
            .chain(optional_dependencies)
            .chain(peer_dependencies)
            .map(|(name, version)| (name.as_str(), version.as_str()))
    }

    pub fn regular_dependencies(&self) -> impl Iterator<Item = (&'_ str, &'_ str)> {
        self.dependencies.iter().flatten().map(|(name, version)| (name.as_str(), version.as_str()))
    }

    pub fn optional_dependencies_iter(&self) -> impl Iterator<Item = (&'_ str, &'_ str)> {
        self.optional_dependencies
            .iter()
            .flatten()
            .map(|(name, version)| (name.as_str(), version.as_str()))
    }

    pub fn peer_dependencies_iter(&self) -> impl Iterator<Item = (&'_ str, &'_ str)> {
        self.peer_dependencies
            .iter()
            .flatten()
            .map(|(name, version)| (name.as_str(), version.as_str()))
    }

    /// Returns true if the given peer dependency is marked as optional in `peerDependenciesMeta`.
    pub fn is_peer_optional(&self, name: &str) -> bool {
        self.peer_dependencies_meta
            .as_ref()
            .and_then(|meta| meta.get(name))
            .is_some_and(|meta| meta.optional)
    }

    pub fn serialize(&self, save_exact: bool) -> String {
        let prefix = if save_exact { "" } else { "^" };
        format!("{0}{1}", prefix, self.version)
    }

    pub fn has_bin(&self) -> bool {
        self.bin.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use pacquet_network::ThrottledClient;

    #[tokio::test]
    async fn fetch_from_registry_sends_authorization_header() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "dist": {
                "tarball": "https://registry.example/pkg/-/pkg-1.0.0.tgz"
            }
        });
        let _mock = server
            .mock("GET", "/pkg/latest")
            .match_header("authorization", "Bearer top-secret")
            .match_header("accept-encoding", "identity")
            .with_status(200)
            .with_body(body.to_string())
            .create_async()
            .await;
        let registry = format!("{}/", server.url());

        let value = PackageVersion::fetch_from_registry(
            "pkg",
            PackageTag::Latest,
            &ThrottledClient::new_from_cpu_count(),
            &registry,
            Some("Bearer top-secret"),
        )
        .await
        .expect("fetch package version with auth header");

        assert_eq!(value.name, "pkg");
    }

    #[test]
    fn deserializes_deprecated_field() {
        let value = serde_json::from_value::<PackageVersion>(serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "deprecated": "use something else",
            "dist": {
                "tarball": "https://registry.example/pkg/-/pkg-1.0.0.tgz"
            }
        }))
        .expect("deserialize package version");

        assert_eq!(value.deprecated.as_deref(), Some("use something else"));
    }

    #[test]
    fn deserializes_homepage_and_repository_fields() {
        let value = serde_json::from_value::<PackageVersion>(serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "homepage": "https://example.com/pkg",
            "repository": {
                "type": "git",
                "url": "git+https://github.com/example/pkg.git"
            },
            "dist": {
                "tarball": "https://registry.example/pkg/-/pkg-1.0.0.tgz"
            }
        }))
        .expect("deserialize package version");

        assert_eq!(value.homepage.as_deref(), Some("https://example.com/pkg"));
        assert_eq!(
            value
                .repository
                .as_ref()
                .and_then(|repository| repository.get("url"))
                .and_then(Value::as_str),
            Some("git+https://github.com/example/pkg.git")
        );
    }
}
