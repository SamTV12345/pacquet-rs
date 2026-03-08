use std::collections::HashMap;

use pacquet_network::ThrottledClient;
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};

use crate::{NetworkError, PackageTag, RegistryError, package_distribution::PackageDistribution};

#[derive(Serialize, Deserialize, Debug, Clone, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageVersion {
    pub name: String,
    pub version: node_semver::Version,
    pub dist: PackageDistribution,
    pub dependencies: Option<HashMap<String, String>>,
    pub optional_dependencies: Option<HashMap<String, String>>,
    pub dev_dependencies: Option<HashMap<String, String>>,
    pub peer_dependencies: Option<HashMap<String, String>>,
    #[serde(default)]
    pub bin: Option<serde_json::Value>,
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

        http_client
            .run_with_permit(|client| {
                let mut request = client.get(url()).header(
                    "accept",
                    "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*",
                );
                if let Some(auth_header) = auth_header {
                    request = request.header("authorization", auth_header);
                }
                request.send()
            })
            .await
            .map_err(network_error)?
            .json::<PackageVersion>()
            .await
            .map_err(network_error)?
            .pipe(Ok)
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
}
