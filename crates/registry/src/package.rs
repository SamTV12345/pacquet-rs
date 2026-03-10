use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use pacquet_network::ThrottledClient;
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};

use crate::{NetworkError, RegistryError, package_version::PackageVersion};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Package {
    pub name: String,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    pub versions: HashMap<String, PackageVersion>,

    #[serde(skip_serializing, skip_deserializing)]
    pub mutex: Arc<Mutex<u8>>,
}

impl PartialEq for Package {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Package {
    pub async fn fetch_from_registry(
        name: &str,
        http_client: &ThrottledClient,
        registry: &str,
        auth_header: Option<&str>,
    ) -> Result<Self, RegistryError> {
        let url = || format!("{registry}{name}"); // TODO: use reqwest URL directly
        let network_error = |error| NetworkError { error, url: url() };
        http_client
            .run_with_permit_for_url(&url(), |client| {
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
            .json::<Package>()
            .await
            .map_err(network_error)?
            .pipe(Ok)
    }

    pub fn pinned_version(&self, version_range: &str) -> Option<&PackageVersion> {
        let range: node_semver::Range = version_range.parse().unwrap(); // TODO: this step should have happened in PackageManifest
        let mut satisfied_versions = self
            .versions
            .values()
            .filter(|v| v.version.satisfies(&range))
            .collect::<Vec<&PackageVersion>>();

        satisfied_versions.sort_by(|a, b| a.version.partial_cmp(&b.version).unwrap());

        // Optimization opportunity:
        // We can store this in a cache to remove filter operation and make this a O(1) operation.
        satisfied_versions.last().copied()
    }

    pub fn latest(&self) -> &PackageVersion {
        let version =
            self.dist_tags.get("latest").expect("latest tag is expected but not found for package");
        self.versions.get(version).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mockito::Server;
    use node_semver::Version;
    use pacquet_network::ThrottledClient;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::package_distribution::PackageDistribution;

    #[test]
    pub fn package_version_should_include_peers() {
        let mut dependencies = HashMap::<String, String>::new();
        dependencies.insert("fastify".to_string(), "1.0.0".to_string());
        let mut peer_dependencies = HashMap::<String, String>::new();
        peer_dependencies.insert("fast-querystring".to_string(), "1.0.0".to_string());
        let version = PackageVersion {
            name: "".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            dist: PackageDistribution::default(),
            dependencies: Some(dependencies),
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: Some(peer_dependencies),
            bin: None,
        };

        let dependencies = |peer| version.dependencies(peer).collect::<HashMap<_, _>>();
        assert!(dependencies(false).contains_key("fastify"));
        assert!(!dependencies(false).contains_key("fast-querystring"));
        assert!(dependencies(true).contains_key("fastify"));
        assert!(dependencies(true).contains_key("fast-querystring"));
        assert!(!dependencies(true).contains_key("hello-world"));
    }

    #[test]
    pub fn serialized_according_to_params() {
        let version = PackageVersion {
            name: "".to_string(),
            version: Version { major: 3, minor: 2, patch: 1, build: vec![], pre_release: vec![] },
            dist: PackageDistribution::default(),
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            bin: None,
        };

        assert_eq!(version.serialize(true), "3.2.1");
        assert_eq!(version.serialize(false), "^3.2.1");
    }

    #[tokio::test]
    async fn fetch_from_registry_sends_authorization_header() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
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
        });
        let _mock = server
            .mock("GET", "/pkg")
            .match_header("authorization", "Bearer secret-token")
            .with_status(200)
            .with_body(body.to_string())
            .create_async()
            .await;
        let registry = format!("{}/", server.url());

        let value = Package::fetch_from_registry(
            "pkg",
            &ThrottledClient::new_from_cpu_count(),
            &registry,
            Some("Bearer secret-token"),
        )
        .await
        .expect("fetch package with auth header");

        assert_eq!(value.name, "pkg");
    }
}
