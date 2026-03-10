use derive_more::{Display, Error};
use flate2::read::GzDecoder;
use miette::Diagnostic;
use node_semver::{Range, Version};
use pacquet_fs::symlink_dir;
use pacquet_network::{RegistryTlsConfig, ThrottledClient, ThrottledClientOptions};
use pacquet_npmrc::Npmrc;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tar::Archive;
use zip::ZipArchive;

const NODEJS_CURRENT_DIRNAME: &str = "nodejs_current";
const STABLE_RELEASE_ERROR_HINT: &str =
    "The correct syntax for stable release is strictly X.Y.Z or release/X.Y.Z";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpecifier {
    pub release_channel: String,
    pub use_node_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSpecifier {
    pub release_channel: String,
    pub version_specifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveSummary {
    pub exit_code: i32,
    pub failures: Vec<String>,
}

#[derive(Debug, Display, Error, Diagnostic)]
pub enum EnvError {
    #[display(
        "\"pnpm env {command} <version>\" can only be used with the \"--global\" option currently"
    )]
    NotImplementedYet { command: &'static str },

    #[display(
        "Unable to manage Node.js because pnpm was not installed using the standalone installation script"
    )]
    CannotManageNode,

    #[display("Couldn't find Node.js version matching {specifier}")]
    CouldNotResolveNodeVersion { specifier: String },

    #[display("Couldn't find Node.js directory in {path:?}")]
    NoNodeDirectory { path: PathBuf },

    #[display("{message}")]
    Message { message: String },
}

#[derive(Debug)]
pub struct EnvManager {
    home_dir: PathBuf,
    bin_dir: Option<PathBuf>,
    raw_config: HashMap<String, String>,
    http_client: ThrottledClient,
}

impl EnvManager {
    pub fn from_system() -> Result<Self, EnvError> {
        let bin_dir =
            std::env::current_exe().ok().and_then(|path| path.parent().map(Path::to_path_buf));
        let npmrc =
            Npmrc::current(std::env::current_dir, home::home_dir, Npmrc::new).map_err(|error| {
                EnvError::Message { message: format!("Could not load .npmrc: {error}") }
            })?;
        let home_dir = std::env::var_os("PNPM_HOME")
            .or_else(|| std::env::var_os("PACQUET_HOME"))
            .map(PathBuf::from)
            .or_else(|| bin_dir.clone())
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| EnvError::Message {
                message: "could not detect home directory".to_string(),
            })?;
        let raw_config = read_raw_npmrc_config(
            std::env::current_dir().ok(),
            home::home_dir().map(|path| path.join(".npmrc")),
        );
        Ok(Self { home_dir, bin_dir, raw_config, http_client: new_http_client(&npmrc) })
    }

    pub async fn add_versions(
        &self,
        global: bool,
        specifiers: &[String],
    ) -> Result<String, EnvError> {
        self.ensure_global(global, "add")?;
        let mut failed = Vec::new();
        for specifier in specifiers {
            if self.download_node_version(specifier).await?.is_none() {
                failed.push(specifier.to_string());
            }
        }
        if !failed.is_empty() {
            return Err(EnvError::CouldNotResolveNodeVersion { specifier: failed.join(", ") });
        }
        Ok("All specified Node.js versions were installed".to_string())
    }

    pub async fn use_version(&self, global: bool, specifier: &str) -> Result<String, EnvError> {
        let bin_dir = self.ensure_global(global, "use")?;
        let Some((version, node_dir)) = self.download_node_version(specifier).await? else {
            return Err(EnvError::CouldNotResolveNodeVersion { specifier: specifier.to_string() });
        };
        self.activate(&node_dir, &bin_dir)?;
        let src = node_exec_path_in_node_dir(&node_dir);
        let dest = node_exec_path_in_bin_dir(&bin_dir);
        Ok(format!("Node.js {version} was activated\n{} -> {}", dest.display(), src.display()))
    }

    pub async fn remove_versions(
        &self,
        global: bool,
        specifiers: &[String],
    ) -> Result<RemoveSummary, EnvError> {
        let bin_dir = self.ensure_global(global, "remove")?;
        let mut failures = Vec::new();
        for specifier in specifiers {
            if let Err(error) = self.remove_single(&bin_dir, specifier).await {
                failures.push(error.to_string());
            }
        }
        Ok(RemoveSummary { exit_code: if failures.is_empty() { 0 } else { 1 }, failures })
    }

    pub async fn list_versions(
        &self,
        remote: bool,
        version_specifier: Option<&str>,
    ) -> Result<String, EnvError> {
        if remote {
            let specifier = parse_env_specifier(version_specifier.unwrap_or_default());
            let mirror = get_node_mirror(&self.raw_config, &specifier.release_channel);
            let mut versions = self.resolve_versions(&mirror, &specifier.version_specifier).await?;
            versions.reverse();
            return Ok(versions.join("\n"));
        }

        let local = self.list_local()?;
        Ok(local
            .into_iter()
            .map(|(version, current)| format!("{} {version}", if current { '*' } else { ' ' }))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn ensure_global(&self, global: bool, command: &'static str) -> Result<PathBuf, EnvError> {
        if !global {
            return Err(EnvError::NotImplementedYet { command });
        }
        self.bin_dir.clone().ok_or(EnvError::CannotManageNode)
    }

    fn node_versions_dir(&self) -> PathBuf {
        self.home_dir.join("nodejs")
    }
}

fn normalized_arch_for_node(platform: &str, arch: &str, version: &str) -> String {
    let major =
        version.split('.').next().and_then(|part| part.parse::<u64>().ok()).unwrap_or_default();
    if platform == "darwin" && arch == "arm64" && major < 16 {
        return "x64".to_string();
    }
    if platform == "win32" && arch == "x86" {
        return "x86".to_string();
    }
    if arch == "arm" {
        return "armv7l".to_string();
    }
    arch.to_string()
}

fn node_download_url(mirror: &str, version: &str) -> String {
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let raw_arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => other,
    };
    let arch = normalized_arch_for_node(platform, raw_arch, version);
    let platform = if platform == "win32" { "win" } else { platform };
    let ext = if cfg!(windows) { ".zip" } else { ".tar.gz" };
    format!("{mirror}v{version}/node-v{version}-{platform}-{arch}{ext}")
}

fn extract_tar_gz(bytes: &[u8], output_dir: &Path, version: &str) -> Result<(), EnvError> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    archive.unpack(output_dir).map_err(|error| EnvError::Message {
        message: format!("Could not extract archive for Node.js {version}: {error}"),
    })
}

fn extract_zip(bytes: &[u8], output_dir: &Path, version: &str) -> Result<(), EnvError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| EnvError::Message {
        message: format!("Could not extract archive for Node.js {version}: {error}"),
    })?;
    for idx in 0..archive.len() {
        let mut file = archive.by_index(idx).map_err(|error| EnvError::Message {
            message: format!("Could not extract archive for Node.js {version}: {error}"),
        })?;
        let Some(relative) = file.enclosed_name().map(|path| path.to_path_buf()) else {
            continue;
        };
        let target = output_dir.join(relative);
        if file.is_dir() {
            fs::create_dir_all(&target).map_err(|error| EnvError::Message {
                message: format!("Could not extract archive for Node.js {version}: {error}"),
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| EnvError::Message {
                message: format!("Could not extract archive for Node.js {version}: {error}"),
            })?;
        }
        let mut output = fs::File::create(&target).map_err(|error| EnvError::Message {
            message: format!("Could not extract archive for Node.js {version}: {error}"),
        })?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).map_err(|error| EnvError::Message {
            message: format!("Could not extract archive for Node.js {version}: {error}"),
        })?;
        output.write_all(&buffer).map_err(|error| EnvError::Message {
            message: format!("Could not extract archive for Node.js {version}: {error}"),
        })?;
    }
    Ok(())
}

fn find_first_dir(base: &Path) -> Option<PathBuf> {
    fs::read_dir(base).ok()?.flatten().map(|entry| entry.path()).find(|path| path.is_dir())
}

fn node_exec_path_in_bin_dir(bin_dir: &Path) -> PathBuf {
    if cfg!(windows) { bin_dir.join("node.exe") } else { bin_dir.join("node") }
}

fn node_exec_path_in_node_dir(node_dir: &Path) -> PathBuf {
    if cfg!(windows) { node_dir.join("node.exe") } else { node_dir.join("bin/node") }
}

fn npm_cli_paths(node_dir: &Path) -> (PathBuf, PathBuf) {
    if cfg!(windows) {
        (
            node_dir.join("node_modules/npm/bin/npm-cli.js"),
            node_dir.join("node_modules/npm/bin/npx-cli.js"),
        )
    } else {
        (
            node_dir.join("lib/node_modules/npm/bin/npm-cli.js"),
            node_dir.join("lib/node_modules/npm/bin/npx-cli.js"),
        )
    }
}

#[cfg(windows)]
fn write_windows_shims(node_dir: &Path, bin_dir: &Path, node_exec: &Path) -> Result<(), EnvError> {
    let (npm_cli, npx_cli) = npm_cli_paths(node_dir);
    if !npm_cli.exists() || !npx_cli.exists() {
        return Ok(());
    }
    fs::write(
        bin_dir.join("npm.cmd"),
        format!("@ECHO OFF\r\n\"{}\" \"{}\" %*\r\n", node_exec.display(), npm_cli.display()),
    )
    .map_err(|error| EnvError::Message {
        message: format!("Could not write {}: {error}", bin_dir.join("npm.cmd").display()),
    })?;
    fs::write(
        bin_dir.join("npx.cmd"),
        format!("@ECHO OFF\r\n\"{}\" \"{}\" %*\r\n", node_exec.display(), npx_cli.display()),
    )
    .map_err(|error| EnvError::Message {
        message: format!("Could not write {}: {error}", bin_dir.join("npx.cmd").display()),
    })?;
    Ok(())
}

#[cfg(unix)]
fn write_unix_shims(node_dir: &Path, bin_dir: &Path, node_exec: &Path) -> Result<(), EnvError> {
    use std::os::unix::fs::PermissionsExt;
    let (npm_cli, npx_cli) = npm_cli_paths(node_dir);
    if !npm_cli.exists() || !npx_cli.exists() {
        return Ok(());
    }
    fs::write(
        bin_dir.join("npm"),
        format!("#!/bin/sh\n\"{}\" \"{}\" \"$@\"\n", node_exec.display(), npm_cli.display()),
    )
    .map_err(|error| EnvError::Message {
        message: format!("Could not write {}: {error}", bin_dir.join("npm").display()),
    })?;
    fs::write(
        bin_dir.join("npx"),
        format!("#!/bin/sh\n\"{}\" \"{}\" \"$@\"\n", node_exec.display(), npx_cli.display()),
    )
    .map_err(|error| EnvError::Message {
        message: format!("Could not write {}: {error}", bin_dir.join("npx").display()),
    })?;
    for shim in ["npm", "npx"] {
        let path = bin_dir.join(shim);
        let mut permissions = fs::metadata(&path)
            .map_err(|error| EnvError::Message {
                message: format!("Could not read {}: {error}", path.display()),
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).map_err(|error| EnvError::Message {
            message: format!("Could not set permissions on {}: {error}", path.display()),
        })?;
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<(), EnvError> {
    if !path.exists() {
        return Ok(());
    }
    let attempts = if cfg!(windows) { 8 } else { 1 };
    for attempt in 0..attempts {
        match remove_existing_path_once(path) {
            Ok(()) => return Ok(()),
            Err(error)
                if cfg!(windows)
                    && matches!(
                        &error,
                        EnvError::Message { message }
                            if message.contains("Permission denied")
                                || message.contains("Zugriff verweigert")
                                || message.contains("Access is denied")
                    )
                    && attempt + 1 < attempts =>
            {
                std::thread::sleep(std::time::Duration::from_millis(((attempt + 1) * 40) as u64));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn remove_existing_path_once(path: &Path) -> Result<(), EnvError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| EnvError::Message {
        message: format!("Could not read metadata {}: {error}", path.display()),
    })?;
    if metadata.file_type().is_symlink() {
        return fs::remove_file(path).map_err(|error| EnvError::Message {
            message: format!("Could not remove {}: {error}", path.display()),
        });
    }
    if metadata.is_dir() {
        return fs::remove_dir_all(path).map_err(|error| EnvError::Message {
            message: format!("Could not remove {}: {error}", path.display()),
        });
    }
    fs::remove_file(path).map_err(|error| EnvError::Message {
        message: format!("Could not remove {}: {error}", path.display()),
    })
}

fn resolve_one(versions: &[NodeRelease], selector: &str) -> Option<String> {
    if selector == "latest" {
        return versions.first().map(|release| release.version.clone());
    }
    let (candidates, range) = filter_versions(versions, selector);
    if range == "*" {
        return max_semver(&candidates);
    }
    let Ok(range) = range.parse::<Range>() else {
        return None;
    };
    max_semver(
        &candidates
            .into_iter()
            .filter(|version| {
                version.parse::<Version>().ok().is_some_and(|parsed| parsed.satisfies(&range))
            })
            .collect::<Vec<_>>(),
    )
}

fn resolve_many(versions: &[NodeRelease], selector: &str) -> Vec<String> {
    if selector.is_empty() {
        return versions.iter().map(|release| release.version.clone()).collect();
    }
    if selector == "latest" {
        return versions.first().map(|release| vec![release.version.clone()]).unwrap_or_default();
    }
    let (candidates, range) = filter_versions(versions, selector);
    if range == "*" {
        return candidates;
    }
    let Ok(range) = range.parse::<Range>() else {
        return vec![];
    };
    candidates
        .into_iter()
        .filter(|version| {
            version.parse::<Version>().ok().is_some_and(|parsed| parsed.satisfies(&range))
        })
        .collect()
}

fn filter_versions(versions: &[NodeRelease], selector: &str) -> (Vec<String>, String) {
    if selector == "lts" {
        return (
            versions
                .iter()
                .filter(|release| release.lts.is_some())
                .map(|release| release.version.clone())
                .collect(),
            "*".to_string(),
        );
    }
    if is_lts_tag(selector) {
        let wanted = selector.to_ascii_lowercase();
        return (
            versions
                .iter()
                .filter(|release| {
                    release.lts.as_ref().is_some_and(|lts| lts.to_ascii_lowercase() == wanted)
                })
                .map(|release| release.version.clone())
                .collect(),
            "*".to_string(),
        );
    }
    (versions.iter().map(|release| release.version.clone()).collect(), selector.to_string())
}

fn is_lts_tag(selector: &str) -> bool {
    if selector.is_empty() || selector == "latest" || selector == "lts" {
        return false;
    }
    selector.parse::<Range>().is_err()
}

fn max_semver(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .filter_map(|version| version.parse::<Version>().ok().map(|parsed| (version, parsed)))
        .max_by(|left, right| left.1.cmp(&right.1))
        .map(|(version, _)| version.to_string())
}

fn version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<Version>().ok(), right.parse::<Version>().ok()) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_npmrc::RegistrySslConfig;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_env_specifier_matches_pnpm_rules() {
        assert_eq!(
            parse_env_specifier("rc/18"),
            EnvSpecifier { release_channel: "rc".to_string(), version_specifier: "18".to_string() }
        );
        assert_eq!(
            parse_env_specifier("16.0.0-rc.0"),
            EnvSpecifier {
                release_channel: "rc".to_string(),
                version_specifier: "16.0.0-rc.0".to_string()
            }
        );
        assert_eq!(
            parse_env_specifier("latest"),
            EnvSpecifier {
                release_channel: "release".to_string(),
                version_specifier: "latest".to_string()
            }
        );
    }

    #[test]
    fn parse_node_specifier_matches_pnpm_rules() {
        assert!(is_valid_node_version("16.4.0"));
        assert!(is_valid_node_version("16.0.0-rc.0"));
        assert!(!is_valid_node_version("16.4"));
        let parsed = parse_node_specifier("release/16.4.0").expect("parse release selector");
        assert_eq!(parsed.release_channel, "release".to_string());
        assert_eq!(parsed.use_node_version, "16.4.0".to_string());
    }

    #[test]
    fn resolve_versions_by_lts_and_range() {
        let versions = vec![
            NodeRelease { version: "20.11.0".to_string(), lts: Some("Iron".to_string()) },
            NodeRelease { version: "20.10.0".to_string(), lts: Some("Iron".to_string()) },
            NodeRelease { version: "19.9.0".to_string(), lts: None },
            NodeRelease { version: "18.19.1".to_string(), lts: Some("Hydrogen".to_string()) },
        ];

        assert_eq!(resolve_one(&versions, "latest"), Some("20.11.0".to_string()));
        assert_eq!(resolve_one(&versions, "lts"), Some("20.11.0".to_string()));
        assert_eq!(resolve_one(&versions, "iron"), Some("20.11.0".to_string()));
        assert_eq!(
            resolve_many(&versions, "20"),
            vec!["20.11.0".to_string(), "20.10.0".to_string()]
        );
    }

    #[test]
    fn normalize_arch_matches_pnpm_rules() {
        assert_eq!(normalized_arch_for_node("darwin", "arm64", "15.9.0"), "x64");
        assert_eq!(normalized_arch_for_node("darwin", "arm64", "16.0.0"), "arm64");
        assert_eq!(normalized_arch_for_node("linux", "arm", "20.11.0"), "armv7l");
        assert_eq!(normalized_arch_for_node("win32", "x86", "20.11.0"), "x86");
    }

    #[test]
    fn new_http_client_uses_npmrc_network_settings() {
        let mut config = Npmrc::new();
        config.network_concurrency = 3;
        config.fetch_timeout = 1_234;
        config.strict_ssl = false;
        config.ca =
            vec!["-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----".to_string()];
        config.https_proxy = Some("http://secure-proxy.example".to_string());
        config.http_proxy = Some("http://plain-proxy.example".to_string());
        config.no_proxy = Some("localhost,127.0.0.1".to_string());
        config.ssl_configs.insert(
            "//nodejs.org/download/release/".to_string(),
            RegistrySslConfig {
                ca: Some("ca".to_string()),
                cert: Some("cert".to_string()),
                key: Some("key".to_string()),
            },
        );

        let client = new_http_client(&config);

        assert_eq!(client.concurrency_limit(), 3);
        assert_eq!(client.request_timeout_ms(), Some(1_234));
        assert!(!client.strict_ssl());
        assert_eq!(client.ca_cert_count(), 1);
        assert_eq!(client.registry_tls_config_count(), 1);
        assert_eq!(client.https_proxy(), Some("http://secure-proxy.example"));
        assert_eq!(client.http_proxy(), Some("http://plain-proxy.example"));
        assert_eq!(client.no_proxy(), Some("localhost,127.0.0.1"));
    }
}

#[derive(Debug, Deserialize)]
struct NodeReleaseJson {
    version: String,
    #[serde(default)]
    lts: Value,
}

#[derive(Debug, Clone)]
struct NodeRelease {
    version: String,
    lts: Option<String>,
}

fn read_raw_npmrc_config(
    current_dir: Option<PathBuf>,
    home_npmrc: Option<PathBuf>,
) -> HashMap<String, String> {
    let mut raw_config = HashMap::new();
    if let Some(home_npmrc) = home_npmrc {
        raw_config.extend(read_npmrc_file(&home_npmrc));
    }
    if let Some(current_dir) = current_dir {
        raw_config.extend(read_npmrc_file(&current_dir.join(".npmrc")));
    }
    raw_config
}

fn read_npmrc_file(path: &Path) -> HashMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

pub fn parse_env_specifier(specifier: &str) -> EnvSpecifier {
    if let Some((release_channel, version_specifier)) = specifier.split_once('/') {
        return EnvSpecifier {
            release_channel: release_channel.to_string(),
            version_specifier: version_specifier.to_string(),
        };
    }
    for channel in ["nightly", "rc", "test", "v8-canary"] {
        if specifier.contains(&format!("-{channel}")) {
            return EnvSpecifier {
                release_channel: channel.to_string(),
                version_specifier: specifier.to_string(),
            };
        }
    }
    if ["nightly", "rc", "test", "release", "v8-canary"].contains(&specifier) {
        return EnvSpecifier {
            release_channel: specifier.to_string(),
            version_specifier: "latest".to_string(),
        };
    }
    EnvSpecifier {
        release_channel: "release".to_string(),
        version_specifier: specifier.to_string(),
    }
}

pub fn is_valid_node_version(specifier: &str) -> bool {
    if let Some((release_channel, version)) = specifier.split_once('/') {
        if release_channel == "release" {
            return is_stable(version);
        }
        return version.contains(release_channel);
    }
    is_stable(specifier) || prerelease_channel(specifier).is_some()
}

pub fn parse_node_specifier(specifier: &str) -> Result<NodeSpecifier, EnvError> {
    if let Some((release_channel, use_node_version)) = specifier.split_once('/') {
        if release_channel == "release" && !is_stable(use_node_version) {
            return Err(EnvError::Message {
                message: format!(
                    "\"{specifier}\" is not a valid Node.js version\n{STABLE_RELEASE_ERROR_HINT}"
                ),
            });
        }
        if release_channel != "release" && !use_node_version.contains(release_channel) {
            return Err(EnvError::Message {
                message: format!(
                    "Node.js version ({use_node_version}) must contain the release channel ({release_channel})"
                ),
            });
        }
        return Ok(NodeSpecifier {
            release_channel: release_channel.to_string(),
            use_node_version: use_node_version.to_string(),
        });
    }
    if let Some(channel) = prerelease_channel(specifier) {
        return Ok(NodeSpecifier {
            release_channel: channel.to_string(),
            use_node_version: specifier.to_string(),
        });
    }
    if is_stable(specifier) {
        return Ok(NodeSpecifier {
            release_channel: "release".to_string(),
            use_node_version: specifier.to_string(),
        });
    }
    Err(EnvError::Message { message: format!("\"{specifier}\" is not a valid Node.js version") })
}

fn is_stable(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3 && parts.iter().all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
}

fn prerelease_channel(version: &str) -> Option<&'static str> {
    let (base, suffix) = version.split_once('-')?;
    if !is_stable(base) {
        return None;
    }
    if suffix.starts_with("rc.") {
        return Some("rc");
    }
    if suffix.starts_with("test") {
        return Some("test");
    }
    if suffix.starts_with("v8-canary") {
        return Some("v8-canary");
    }
    if suffix.starts_with("nightly") {
        return Some("nightly");
    }
    None
}

fn get_node_mirror(raw_config: &HashMap<String, String>, release_channel: &str) -> String {
    let key = format!("node-mirror:{release_channel}");
    let mirror = raw_config
        .get(&key)
        .cloned()
        .unwrap_or_else(|| format!("https://nodejs.org/download/{release_channel}/"));
    if mirror.ends_with('/') { mirror } else { format!("{mirror}/") }
}

fn network_tls_configs(config: &Npmrc) -> HashMap<String, RegistryTlsConfig> {
    config
        .ssl_configs
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                RegistryTlsConfig {
                    ca: value.ca.clone(),
                    cert: value.cert.clone(),
                    key: value.key.clone(),
                },
            )
        })
        .collect()
}

fn new_http_client(config: &Npmrc) -> ThrottledClient {
    ThrottledClient::new_with_options(
        config.network_concurrency as usize,
        ThrottledClientOptions {
            request_timeout_ms: Some(config.fetch_timeout),
            strict_ssl: config.strict_ssl,
            ca_certs: config.ca.clone(),
            registry_tls_configs: network_tls_configs(config),
            https_proxy: config.https_proxy.clone(),
            http_proxy: config.http_proxy.clone(),
            no_proxy: config.no_proxy.clone(),
        },
    )
}

impl EnvManager {
    async fn download_node_version(
        &self,
        env_specifier: &str,
    ) -> Result<Option<(String, PathBuf)>, EnvError> {
        let parsed = parse_env_specifier(env_specifier);
        let mirror = get_node_mirror(&self.raw_config, &parsed.release_channel);
        let Some(version) = self.resolve_version(&mirror, &parsed.version_specifier).await? else {
            return Ok(None);
        };

        let version_dir = self.node_versions_dir().join(&version);
        if node_exec_path_in_node_dir(&version_dir).is_file() {
            return Ok(Some((version, version_dir)));
        }

        fs::create_dir_all(self.node_versions_dir()).map_err(|error| EnvError::Message {
            message: format!("Could not create {}: {error}", self.node_versions_dir().display()),
        })?;

        let url = node_download_url(&mirror, &version);
        let bytes = self.fetch_bytes(&url).await?;
        let temp_dir = self.node_versions_dir().join(format!(
            ".tmp-{}-{}-{}",
            version,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        ));
        if temp_dir.exists() {
            remove_existing_path(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir).map_err(|error| EnvError::Message {
            message: format!("Could not create {}: {error}", temp_dir.display()),
        })?;

        if cfg!(windows) {
            extract_zip(&bytes, &temp_dir, &version)?;
        } else {
            extract_tar_gz(&bytes, &temp_dir, &version)?;
        }

        let extracted = find_first_dir(&temp_dir).ok_or_else(|| EnvError::Message {
            message: format!("Could not find extracted directory for Node.js {version}"),
        })?;
        if version_dir.exists() {
            remove_existing_path(&version_dir)?;
        }
        fs::rename(&extracted, &version_dir).map_err(|error| EnvError::Message {
            message: format!(
                "Could not move {} to {}: {error}",
                extracted.display(),
                version_dir.display()
            ),
        })?;
        let _ = remove_existing_path(&temp_dir);

        Ok(Some((version, version_dir)))
    }

    fn activate(&self, node_dir: &Path, bin_dir: &Path) -> Result<(), EnvError> {
        let current_link = self.home_dir.join(NODEJS_CURRENT_DIRNAME);
        if current_link.exists() {
            remove_existing_path(&current_link)?;
        }
        symlink_dir(node_dir, &current_link).map_err(|error| EnvError::Message {
            message: format!(
                "Could not link {} -> {}: {error}",
                current_link.display(),
                node_dir.display()
            ),
        })?;

        fs::create_dir_all(bin_dir).map_err(|error| EnvError::Message {
            message: format!("Could not create {}: {error}", bin_dir.display()),
        })?;
        let src = node_exec_path_in_node_dir(node_dir);
        let dest = node_exec_path_in_bin_dir(bin_dir);
        if dest.exists() {
            remove_existing_path(&dest)?;
        }
        #[cfg(windows)]
        {
            if let Err(link_error) = fs::hard_link(&src, &dest) {
                fs::copy(&src, &dest).map_err(|copy_error| EnvError::Message {
                    message: format!(
                        "Could not create node link {} -> {} ({link_error}): {copy_error}",
                        dest.display(),
                        src.display()
                    ),
                })?;
            }
            write_windows_shims(node_dir, bin_dir, &dest)?;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &dest).map_err(|error| EnvError::Message {
                message: format!(
                    "Could not create node symlink {} -> {}: {error}",
                    dest.display(),
                    src.display()
                ),
            })?;
            write_unix_shims(node_dir, bin_dir, &dest)?;
        }
        Ok(())
    }

    async fn remove_single(&self, bin_dir: &Path, specifier: &str) -> Result<(), EnvError> {
        let parsed = parse_env_specifier(specifier);
        let mirror = get_node_mirror(&self.raw_config, &parsed.release_channel);
        let Some(version) = self.resolve_version(&mirror, &parsed.version_specifier).await? else {
            return Err(EnvError::CouldNotResolveNodeVersion { specifier: specifier.to_string() });
        };
        let version_dir = self.node_versions_dir().join(&version);
        if !version_dir.exists() {
            return Err(EnvError::NoNodeDirectory { path: version_dir });
        }

        let current_link = self.home_dir.join(NODEJS_CURRENT_DIRNAME);
        if current_link.exists() {
            let current = fs::canonicalize(&current_link).map_err(|error| EnvError::Message {
                message: format!("Could not resolve {}: {error}", current_link.display()),
            })?;
            let target = fs::canonicalize(&version_dir).map_err(|error| EnvError::Message {
                message: format!("Could not resolve {}: {error}", version_dir.display()),
            })?;
            if current == target {
                for name in ["node", "node.exe", "npm", "npx", "npm.cmd", "npx.cmd"] {
                    let candidate = bin_dir.join(name);
                    if candidate.exists() {
                        let _ = remove_existing_path(&candidate);
                    }
                }
                remove_existing_path(&current_link)?;
            }
        }

        remove_existing_path(&version_dir)?;
        Ok(())
    }

    fn list_local(&self) -> Result<Vec<(String, bool)>, EnvError> {
        if !self.node_versions_dir().exists() {
            return Err(EnvError::NoNodeDirectory { path: self.node_versions_dir() });
        }
        let current = fs::canonicalize(self.home_dir.join(NODEJS_CURRENT_DIRNAME)).ok();

        let mut versions = fs::read_dir(self.node_versions_dir())
            .map_err(|error| EnvError::Message {
                message: format!("Could not read {}: {error}", self.node_versions_dir().display()),
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter_map(|path| {
                let version = path.file_name()?.to_str()?.to_string();
                if version.parse::<Version>().is_err() {
                    return None;
                }
                if !node_exec_path_in_node_dir(&path).is_file() {
                    return None;
                }
                let normalized = fs::canonicalize(&path).ok();
                let is_current = match (current.as_ref(), normalized.as_ref()) {
                    (Some(current), Some(normalized)) => current == normalized,
                    _ => false,
                };
                Some((version, is_current))
            })
            .collect::<Vec<_>>();
        versions.sort_by(|left, right| version_cmp(&left.0, &right.0));
        Ok(versions)
    }

    async fn resolve_version(
        &self,
        mirror: &str,
        version_specifier: &str,
    ) -> Result<Option<String>, EnvError> {
        let versions = self.fetch_versions(mirror).await?;
        Ok(resolve_one(&versions, version_specifier))
    }

    async fn resolve_versions(
        &self,
        mirror: &str,
        version_specifier: &str,
    ) -> Result<Vec<String>, EnvError> {
        let versions = self.fetch_versions(mirror).await?;
        Ok(resolve_many(&versions, version_specifier))
    }

    async fn fetch_versions(&self, mirror: &str) -> Result<Vec<NodeRelease>, EnvError> {
        let url = format!("{mirror}index.json");
        let value = self.fetch_json(&url).await?;
        let parsed: Vec<NodeReleaseJson> =
            serde_json::from_value(value).map_err(|error| EnvError::Message {
                message: format!("Could not parse Node.js index from {url}: {error}"),
            })?;
        Ok(parsed
            .into_iter()
            .map(|release| NodeRelease {
                version: release.version.trim_start_matches('v').to_string(),
                lts: release.lts.as_str().map(ToString::to_string),
            })
            .collect())
    }

    async fn fetch_json(&self, url: &str) -> Result<Value, EnvError> {
        let url_string = url.to_string();
        let response = self
            .http_client
            .run_with_permit_for_url(url, |client| client.get(url).send())
            .await
            .map_err(|error| EnvError::Message {
                message: format!("Could not fetch {}: {error}", url_string),
            })?
            .error_for_status()
            .map_err(|error| EnvError::Message {
                message: format!("Could not fetch {}: {error}", url_string),
            })?;
        response.json::<Value>().await.map_err(|error| EnvError::Message {
            message: format!("Could not parse {}: {error}", url_string),
        })
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, EnvError> {
        let url_string = url.to_string();
        let response = self
            .http_client
            .run_with_permit_for_url(url, |client| client.get(url).send())
            .await
            .map_err(|error| EnvError::Message {
                message: format!("Could not fetch {}: {error}", url_string),
            })?
            .error_for_status()
            .map_err(|error| EnvError::Message {
                message: format!("Could not fetch {}: {error}", url_string),
            })?;
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| EnvError::Message {
            message: format!("Could not download {}: {error}", url_string),
        })
    }
}
