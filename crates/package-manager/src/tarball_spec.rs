use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry::{PackageDistribution, PackageVersion};
use ssri::{Algorithm, IntegrityOpts};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use tar::Archive;
use zune_inflate::{DeflateDecoder, DeflateOptions};

pub(crate) fn is_tarball_spec(spec: &str) -> bool {
    (spec.starts_with("http://") || spec.starts_with("https://"))
        && (spec.ends_with(".tgz") || spec.ends_with(".tar.gz"))
}

fn value_to_map(value: Option<&serde_json::Value>) -> Option<HashMap<String, String>> {
    let obj = value?.as_object()?;
    let map = obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|value| (k.to_string(), value.to_string())))
        .collect::<HashMap<_, _>>();
    (!map.is_empty()).then_some(map)
}

pub(crate) async fn resolve_package_version_from_tarball_spec(
    config: &Npmrc,
    http_client: &ThrottledClient,
    tarball_url: &str,
) -> Result<PackageVersion, String> {
    let auth_header = config.auth_header_for_url(tarball_url);
    let response = http_client
        .run_with_permit(|client| {
            let mut request = client.get(tarball_url);
            if let Some(auth_header) = auth_header.as_deref() {
                request = request.header("authorization", auth_header);
            }
            request.send()
        })
        .await
        .map_err(|error| format!("request tarball: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download tarball: {error}"))?
        .bytes()
        .await
        .map_err(|error| format!("read tarball bytes: {error}"))?
        .to_vec();

    let integrity = IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(&response).result();
    let tar_bytes = DeflateDecoder::new_with_options(&response, DeflateOptions::default())
        .decode_gzip()
        .map_err(|error| format!("decode tarball gzip: {error}"))?;
    let mut archive = Archive::new(Cursor::new(tar_bytes));

    let mut manifest: Option<serde_json::Value> = None;
    for entry in archive.entries().map_err(|error| format!("read tarball entries: {error}"))? {
        let mut entry = entry.map_err(|error| format!("read tarball entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read tarball entry path: {error}"))?
            .to_string_lossy()
            .to_string();
        if !path.ends_with("/package.json") {
            continue;
        }
        let mut buffer = Vec::new();
        entry.read_to_end(&mut buffer).map_err(|error| format!("read package.json: {error}"))?;
        manifest = Some(
            serde_json::from_slice(&buffer)
                .map_err(|error| format!("parse package.json: {error}"))?,
        );
        break;
    }

    let manifest = manifest.ok_or_else(|| "package.json missing from tarball".to_string())?;
    let name = manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "package.json missing name".to_string())?
        .to_string();
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "package.json missing version".to_string())?
        .parse::<node_semver::Version>()
        .map_err(|error| format!("parse version: {error}"))?;

    Ok(PackageVersion {
        name,
        version,
        dist: PackageDistribution {
            integrity: Some(integrity),
            shasum: None,
            tarball: tarball_url.to_string(),
            file_count: None,
            unpacked_size: None,
        },
        dependencies: value_to_map(manifest.get("dependencies")),
        optional_dependencies: value_to_map(manifest.get("optionalDependencies")),
        dev_dependencies: value_to_map(manifest.get("devDependencies")),
        peer_dependencies: value_to_map(manifest.get("peerDependencies")),
        bin: manifest.get("bin").cloned(),
    })
}
