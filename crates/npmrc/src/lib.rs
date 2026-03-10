mod custom_deserializer;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STD};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_store_dir::StoreDir;
use pipe_trait::Pipe;
use serde::Deserialize;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::{collections::HashMap, fs, path::PathBuf, process::Command};
use url::Url;

use crate::custom_deserializer::{
    bool_true, default_cache_dir, default_fetch_timeout, default_hoist_pattern,
    default_modules_cache_max_age, default_modules_dir, default_network_concurrency,
    default_peers_suffix_max_length, default_public_hoist_pattern, default_registry,
    default_store_dir, default_virtual_store_dir, deserialize_bool, deserialize_optional_pathbuf,
    deserialize_pathbuf, deserialize_registry, deserialize_store_dir, deserialize_string_vec,
    deserialize_u16, deserialize_u64,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistrySslConfig {
    pub ca: Option<String>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum NodeLinker {
    /// dependencies are symlinked from a virtual store at node_modules/.pnpm.
    #[default]
    Isolated,

    /// flat node_modules without symlinks is created. Same as the node_modules created by npm or
    /// Yarn Classic.
    Hoisted,

    /// no node_modules. Plug'n'Play is an innovative strategy for Node that is used by
    /// Yarn Berry. It is recommended to also set symlink setting to false when using pnp as
    /// your linker.
    Pnp,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageImportMethod {
    ///  try to clone packages from the store. If cloning is not supported then hardlink packages
    /// from the store. If neither cloning nor linking is possible, fall back to copying
    #[default]
    Auto,

    /// hard link packages from the store
    Hardlink,

    /// try to clone packages from the store. If cloning is not supported then fall back to copying
    Copy,

    /// copy packages from the store
    Clone,

    /// clone (AKA copy-on-write or reference link) packages from the store
    CloneOrCopy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Npmrc {
    /// When true, all dependencies are hoisted to node_modules/.pnpm/node_modules.
    /// This makes unlisted dependencies accessible to all packages inside node_modules.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub hoist: bool,

    /// Tells pnpm which packages should be hoisted to node_modules/.pnpm/node_modules.
    /// By default, all packages are hoisted - however, if you know that only some flawed packages
    /// have phantom dependencies, you can use this option to exclusively hoist the phantom
    /// dependencies (recommended).
    #[serde(default = "default_hoist_pattern", deserialize_with = "deserialize_string_vec")]
    pub hoist_pattern: Vec<String>,

    /// Unlike hoist-pattern, which hoists dependencies to a hidden modules directory inside the
    /// virtual store, public-hoist-pattern hoists dependencies matching the pattern to the root
    /// modules directory. Hoisting to the root modules directory means that application code will
    /// have access to phantom dependencies, even if they modify the resolution strategy improperly.
    #[serde(default = "default_public_hoist_pattern", deserialize_with = "deserialize_string_vec")]
    pub public_hoist_pattern: Vec<String>,

    /// By default, pnpm creates a semistrict node_modules, meaning dependencies have access to
    /// undeclared dependencies but modules outside of node_modules do not. With this layout,
    /// most of the packages in the ecosystem work with no issues. However, if some tooling only
    /// works when the hoisted dependencies are in the root of node_modules, you can set this to
    /// true to hoist them for you.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub shamefully_hoist: bool,

    /// The location where all the packages are saved on the disk.
    #[serde(default = "default_store_dir", deserialize_with = "deserialize_store_dir")]
    pub store_dir: StoreDir,

    /// Directory where package metadata cache is stored.
    #[serde(default = "default_cache_dir", deserialize_with = "deserialize_pathbuf")]
    pub cache_dir: PathBuf,

    /// The directory in which dependencies will be installed (instead of node_modules).
    #[serde(default = "default_modules_dir", deserialize_with = "deserialize_pathbuf")]
    pub modules_dir: PathBuf,

    /// Defines what linker should be used for installing Node packages.
    #[serde(default)]
    pub node_linker: NodeLinker,

    /// When symlink is set to false, pnpm creates a virtual store directory without any symlinks.
    /// It is a useful setting together with node-linker=pnp.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub symlink: bool,

    /// The directory with links to the store. All direct and indirect dependencies of the
    /// project are linked into this directory.
    #[serde(default = "default_virtual_store_dir", deserialize_with = "deserialize_pathbuf")]
    pub virtual_store_dir: PathBuf,

    /// Controls the way packages are imported from the store (if you want to disable symlinks
    /// inside node_modules, then you need to change the node-linker setting, not this one).
    #[serde(default)]
    pub package_import_method: PackageImportMethod,

    /// The time in minutes after which orphan packages from the modules directory should be
    /// removed. pnpm keeps a cache of packages in the modules directory. This boosts installation
    /// speed when switching branches or downgrading dependencies.
    ///
    /// Default value is 10080 (7 days in minutes)
    #[serde(default = "default_modules_cache_max_age", deserialize_with = "deserialize_u64")]
    pub modules_cache_max_age: u64,

    /// Maximum number of concurrent HTTP requests.
    #[serde(default = "default_network_concurrency", deserialize_with = "deserialize_u16")]
    pub network_concurrency: u16,

    /// HTTP request timeout in milliseconds.
    #[serde(default = "default_fetch_timeout", deserialize_with = "deserialize_u64")]
    pub fetch_timeout: u64,

    /// Controls whether SSL/TLS certificate validation is enforced for registry requests.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub strict_ssl: bool,

    /// Legacy catch-all proxy setting.
    #[serde(default)]
    pub proxy: Option<String>,

    /// HTTPS proxy URL.
    #[serde(default)]
    pub https_proxy: Option<String>,

    /// HTTP proxy URL.
    #[serde(default)]
    pub http_proxy: Option<String>,

    /// Comma-separated proxy bypass list.
    #[serde(default)]
    pub no_proxy: Option<String>,

    /// Legacy npm key normalized to `no_proxy`.
    #[serde(default)]
    noproxy: Option<String>,

    /// PEM-encoded CA certificates used for registry TLS validation.
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub ca: Vec<String>,

    /// Path to a PEM file with CA certificates.
    #[serde(default, deserialize_with = "deserialize_optional_pathbuf")]
    cafile: Option<PathBuf>,

    /// When set to false, pnpm won't read or generate a pnpm-lock.yaml file.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub lockfile: bool,

    /// When set to true and the available pnpm-lock.yaml satisfies the package.json dependencies
    /// directive, a headless installation is performed. A headless installation skips all
    /// dependency resolution as it does not need to modify the lockfile.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub prefer_frozen_lockfile: bool,

    /// Add the full URL to the package's tarball to every entry in pnpm-lock.yaml.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub lockfile_include_tarball_url: bool,

    /// Exclude dependencies that are linked from the lockfile.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub exclude_links_from_lockfile: bool,

    /// Controls whether workspace packages are injected by default.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub inject_workspace_packages: bool,

    /// When enabled, injected workspace dependencies may be deduplicated back to links when the
    /// target workspace project already provides a compatible dependency set.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub dedupe_injected_deps: bool,

    /// When enabled, local directory dependencies are not refreshed on reinstall if they are
    /// already present in node_modules.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub disable_relink_local_dir_deps: bool,

    /// Maximum length of peer suffixes persisted to the lockfile.
    #[serde(default = "default_peers_suffix_max_length", deserialize_with = "deserialize_u16")]
    pub peers_suffix_max_length: u16,

    /// The base URL of the npm package registry (trailing slash included).
    #[serde(default = "default_registry", deserialize_with = "deserialize_registry")]
    pub registry: String, // TODO: use Url type (compatible with reqwest)

    /// When true, any missing non-optional peer dependencies are automatically installed.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub auto_install_peers: bool,

    /// When this setting is set to true, packages with peer dependencies will be deduplicated after peers resolution.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub dedupe_peer_dependents: bool,

    /// If this is enabled, commands will fail if there is a missing or invalid peer dependency in the tree.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub strict_peer_dependencies: bool,

    /// When enabled, dependencies of the root workspace project are used to resolve peer
    /// dependencies of any projects in the workspace. It is a useful feature as you can install
    /// your peer dependencies only in the root of the workspace, and you can be sure that all
    /// projects in the workspace use the same versions of the peer dependencies.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub resolve_peers_from_workspace_root: bool,

    /// Controls whether pre- and post- scripts are executed when running a script explicitly.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub enable_pre_post_scripts: bool,

    /// Custom shell binary to run lifecycle scripts in.
    #[serde(default)]
    pub script_shell: Option<String>,

    /// When true, a shell emulator can be used (not yet implemented in pacquet).
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub shell_emulator: bool,

    /// Raw merged `.npmrc` key/value pairs used for auth resolution.
    #[serde(skip, default)]
    raw_settings: HashMap<String, String>,
    /// Raw user-level `.npmrc` key/value pairs used for token helper resolution.
    #[serde(skip, default)]
    raw_user_settings: HashMap<String, String>,
    /// Authorization header values keyed by nerfed URL (`//host/path/`).
    #[serde(skip, default)]
    auth_headers_by_uri: HashMap<String, String>,
    /// Largest number of slash-separated segments among auth header keys.
    #[serde(skip, default)]
    auth_header_max_parts: usize,
    /// Per-registry TLS settings keyed by nerfed URL (`//host/path/`).
    #[serde(skip, default)]
    pub ssl_configs: HashMap<String, RegistrySslConfig>,
}

#[cfg(test)]
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn env_lock() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Display, Error, Diagnostic)]
pub enum AuthConfigError {
    #[display("{setting_name} must be an absolute path, without arguments")]
    InvalidTokenHelperPath { setting_name: String },
    #[display(
        "Error running \"{helper_path}\" as a token helper, configured as {setting_name}. Exit code {exit_code}"
    )]
    TokenHelperExecFailed { setting_name: String, helper_path: String, exit_code: i32 },
    #[display("Failed to run token helper \"{helper_path}\" configured as {setting_name}: {error}")]
    TokenHelperExecIo {
        setting_name: String,
        helper_path: String,
        #[error(source)]
        error: std::io::Error,
    },
}

fn parse_raw_npmrc(content: &str) -> HashMap<String, String> {
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

fn read_npmrc_raw(dir: Option<PathBuf>) -> HashMap<String, String> {
    let Some(dir) = dir else {
        return HashMap::new();
    };
    let path = dir.join(".npmrc");
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    parse_raw_npmrc(&content)
}

fn normalize_registry_url(registry: &str) -> String {
    let trimmed = registry.trim();
    if trimmed.ends_with('/') { trimmed.to_string() } else { format!("{trimmed}/") }
}

fn package_scope(package_name: &str) -> Option<&str> {
    if !package_name.starts_with('@') {
        return None;
    }
    let separator = package_name.find('/')?;
    Some(&package_name[..separator])
}

fn normalize_auth_key_uri(uri: &str) -> Option<String> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("//") {
        return Some(if trimmed.ends_with('/') {
            trimmed.to_string()
        } else {
            format!("{trimmed}/")
        });
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return nerf_url(trimmed);
    }
    None
}

fn split_auth_setting_key(key: &str) -> Option<(&str, &str)> {
    let idx = key.rfind(':')?;
    if idx == 0 || idx + 1 >= key.len() {
        return None;
    }
    Some((&key[..idx], &key[idx + 1..]))
}

fn nerf_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let mut out = String::from("//");
    out.push_str(host);
    if let Some(port) = parsed.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path.is_empty() || path == "/" {
        out.push('/');
    } else {
        out.push_str(path);
        if !path.ends_with('/') {
            out.push('/');
        }
    }
    Some(out)
}

fn basic_auth_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    if parsed.username().is_empty() && parsed.password().is_none() {
        return None;
    }
    let raw = format!("{}:{}", parsed.username(), parsed.password().unwrap_or_default());
    Some(format!("Basic {}", BASE64_STD.encode(raw)))
}

fn remove_default_port(url: &str) -> Option<String> {
    let mut parsed = Url::parse(url).ok()?;
    let has_default_port =
        matches!((parsed.scheme(), parsed.port()), ("https", Some(443)) | ("http", Some(80)));
    if !has_default_port {
        return None;
    }
    if parsed.set_port(None).is_err() {
        return None;
    }
    Some(parsed.to_string())
}

fn load_token_helper(helper_path: &str, setting_name: &str) -> Result<String, AuthConfigError> {
    let helper = PathBuf::from(helper_path);
    if !helper.is_absolute() || !helper.exists() {
        return Err(AuthConfigError::InvalidTokenHelperPath {
            setting_name: setting_name.to_string(),
        });
    }
    let output =
        Command::new(helper).output().map_err(|error| AuthConfigError::TokenHelperExecIo {
            setting_name: setting_name.to_string(),
            helper_path: helper_path.to_string(),
            error,
        })?;
    if !output.status.success() {
        return Err(AuthConfigError::TokenHelperExecFailed {
            setting_name: setting_name.to_string(),
            helper_path: helper_path.to_string(),
            exit_code: output.status.code().unwrap_or_default(),
        });
    }
    let token = String::from_utf8(output.stdout).unwrap_or_default();
    Ok(token.trim_end().to_string())
}

fn get_max_parts(keys: impl Iterator<Item = String>) -> usize {
    keys.map(|key| key.split('/').count()).max().unwrap_or(0)
}

fn unescape_npmrc_newlines(value: &str) -> String {
    value.replace("\\n", "\n")
}

fn get_process_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| std::env::var(name.to_uppercase()).ok())
        .or_else(|| std::env::var(name.to_lowercase()).ok())
}

fn ssl_configs_from_settings(
    settings: &HashMap<String, String>,
) -> HashMap<String, RegistrySslConfig> {
    let mut configs = HashMap::<String, RegistrySslConfig>::new();
    for (key, value) in settings {
        let Some((uri_key, config_type)) = split_auth_setting_key(key) else {
            continue;
        };
        let Some(uri) = normalize_auth_key_uri(uri_key) else {
            continue;
        };
        let entry = configs.entry(uri).or_default();
        match config_type {
            "ca" => entry.ca = Some(unescape_npmrc_newlines(value)),
            "cert" => entry.cert = Some(unescape_npmrc_newlines(value)),
            "key" => entry.key = Some(unescape_npmrc_newlines(value)),
            _ => {}
        }
    }
    configs
}

fn auth_headers_from_settings(
    settings: &HashMap<String, String>,
    user_settings: &HashMap<String, String>,
    registry_url: &str,
) -> Result<(HashMap<String, String>, usize), AuthConfigError> {
    let mut auth_by_uri = HashMap::<String, HashMap<String, String>>::new();

    for (key, value) in settings {
        let Some((uri_key, auth_type)) = split_auth_setting_key(key) else {
            continue;
        };
        let Some(uri) = normalize_auth_key_uri(uri_key) else {
            continue;
        };
        auth_by_uri.entry(uri).or_default().insert(auth_type.to_string(), value.to_string());
    }

    let mut headers = HashMap::<String, String>::new();
    for (uri, auth_fields) in &auth_by_uri {
        if let Some(token_helper) = user_settings.get(&format!("{uri}:tokenHelper")) {
            let setting_name = format!("{uri}:tokenHelper");
            let token = load_token_helper(token_helper, &setting_name)?;
            headers.insert(uri.clone(), token);
            continue;
        }
        if let Some(token) = auth_fields.get("_authToken") {
            headers.insert(uri.clone(), format!("Bearer {token}"));
            continue;
        }
        if let Some(auth) = auth_fields.get("_auth") {
            headers.insert(uri.clone(), format!("Basic {auth}"));
            continue;
        }
        if let (Some(username), Some(password_b64)) =
            (auth_fields.get("username"), auth_fields.get("_password"))
            && let Ok(decoded) = BASE64_STD.decode(password_b64)
            && let Ok(password) = String::from_utf8(decoded)
        {
            let raw = format!("{username}:{password}");
            headers.insert(uri.clone(), format!("Basic {}", BASE64_STD.encode(raw)));
        }
    }

    let registry_key = normalize_auth_key_uri(registry_url)
        .or_else(|| nerf_url(registry_url))
        .unwrap_or_else(|| "//registry.npmjs.org/".to_string());
    if !headers.contains_key(&registry_key) {
        if let Some(token_helper) = user_settings.get("tokenHelper") {
            let token = load_token_helper(token_helper, "tokenHelper")?;
            headers.insert(registry_key.clone(), token);
        } else if let Some(token) = settings.get("_authToken") {
            headers.insert(registry_key.clone(), format!("Bearer {token}"));
        } else if let Some(auth) = settings.get("_auth") {
            headers.insert(registry_key.clone(), format!("Basic {auth}"));
        } else if let (Some(username), Some(password)) =
            (settings.get("username"), settings.get("_password"))
        {
            let raw = format!("{username}:{password}");
            headers.insert(registry_key, format!("Basic {}", BASE64_STD.encode(raw)));
        }
    }

    let max_parts = get_max_parts(headers.keys().cloned());
    Ok((headers, max_parts))
}

impl Npmrc {
    pub fn new() -> Self {
        let mut config: Npmrc = serde_ini::from_str("").unwrap(); // TODO: derive `SmartDefault` for `Npmrc and call `Npmrc::default()`
        config.apply_derived_settings();
        config.recompute_tls_settings();
        config.recompute_request_settings();
        config.recompute_auth_headers().expect("default npmrc auth config should be valid");
        config
    }

    /// Try loading `.npmrc` in the current directory.
    /// If fails, try in the home directory.
    /// If fails again, return the default.
    pub fn current<Error, CurrentDir, HomeDir, Default>(
        current_dir: CurrentDir,
        home_dir: HomeDir,
        default: Default,
    ) -> Result<Self, AuthConfigError>
    where
        CurrentDir: FnOnce() -> Result<PathBuf, Error>,
        HomeDir: FnOnce() -> Option<PathBuf>,
        Default: FnOnce() -> Npmrc,
    {
        let current_dir = current_dir().ok();
        let home_dir = home_dir();

        let load_content =
            |dir: &PathBuf| -> Option<String> { dir.join(".npmrc").pipe(fs::read_to_string).ok() };
        let parse = |content: &str| serde_ini::from_str(content).ok();

        let home_content = home_dir.as_ref().and_then(load_content);
        let current_content = current_dir.as_ref().and_then(load_content);

        let mut merged_raw = read_npmrc_raw(home_dir.clone());
        merged_raw.extend(read_npmrc_raw(current_dir.clone()));
        let user_raw = read_npmrc_raw(current_dir.clone());

        let mut config = match (&home_content, &current_content) {
            (Some(home), Some(current)) => parse(&format!("{home}\n{current}"))
                .or_else(|| parse(current))
                .or_else(|| parse(home)),
            (None, Some(current)) => parse(current),
            (Some(home), None) => parse(home),
            (None, None) => None,
        }
        .unwrap_or_else(default);
        config.apply_derived_settings();
        config.raw_settings = merged_raw;
        config.raw_user_settings = user_raw;
        if !config.raw_settings.contains_key("registry") {
            config.raw_settings.insert("registry".to_string(), config.registry.clone());
        }
        config.recompute_tls_settings();
        config.recompute_request_settings();
        config.recompute_auth_headers()?;
        Ok(config)
    }

    pub fn apply_derived_settings(&mut self) {
        if !self.hoist {
            self.hoist_pattern.clear();
        }

        if self.shamefully_hoist {
            self.public_hoist_pattern = vec!["*".to_string()];
        } else if self.public_hoist_pattern.len() == 1 && self.public_hoist_pattern[0].is_empty() {
            self.public_hoist_pattern.clear();
        }

        if !self.symlink {
            self.hoist_pattern.clear();
            self.public_hoist_pattern.clear();
        }
    }

    fn recompute_auth_headers(&mut self) -> Result<(), AuthConfigError> {
        let (headers, max_parts) = auth_headers_from_settings(
            &self.raw_settings,
            &self.raw_user_settings,
            &self.registry,
        )?;
        self.auth_headers_by_uri = headers;
        self.auth_header_max_parts = max_parts;
        Ok(())
    }

    fn recompute_tls_settings(&mut self) {
        self.ca = self.ca.iter().map(|entry| unescape_npmrc_newlines(entry)).collect();
        if let Some(cafile) = &self.cafile
            && let Ok(content) = fs::read_to_string(cafile)
        {
            self.ca = vec![content];
        }
        self.ssl_configs = ssl_configs_from_settings(&self.raw_settings);
    }

    fn recompute_request_settings(&mut self) {
        if self.https_proxy.is_none() {
            self.https_proxy = self.proxy.clone().or_else(|| get_process_env("https_proxy"));
        }
        if self.http_proxy.is_none() {
            self.http_proxy = self
                .https_proxy
                .clone()
                .or_else(|| get_process_env("http_proxy"))
                .or_else(|| get_process_env("proxy"));
        }
        if self.no_proxy.is_none() {
            self.no_proxy = self.noproxy.clone().or_else(|| get_process_env("no_proxy"));
        }
    }

    pub fn auth_header_for_url(&self, url: &str) -> Option<String> {
        if let Some(basic) = basic_auth_from_url(url) {
            return Some(basic);
        }
        if self.auth_headers_by_uri.is_empty() {
            return None;
        }
        let try_match = |candidate_url: &str| -> Option<String> {
            let nerfed = nerf_url(candidate_url)?;
            let parts = nerfed.split('/').collect::<Vec<_>>();
            let max_parts = self.auth_header_max_parts.min(parts.len());
            for idx in (3..max_parts).rev() {
                let key = format!("{}/", parts[..idx].join("/"));
                if let Some(value) = self.auth_headers_by_uri.get(&key) {
                    return Some(value.clone());
                }
            }
            None
        };
        try_match(url)
            .or_else(|| remove_default_port(url).and_then(|without_port| try_match(&without_port)))
    }

    pub fn registry_for_package_name(&self, package_name: &str) -> String {
        let Some(scope) = package_scope(package_name) else {
            return self.registry.clone();
        };
        let key = format!("{scope}:registry");
        self.raw_settings
            .get(&key)
            .map(|registry| normalize_registry_url(registry))
            .unwrap_or_else(|| self.registry.clone())
    }

    /// Persist the config data until the program terminates.
    pub fn leak(self) -> &'static mut Self {
        self.pipe(Box::new).pipe(Box::leak)
    }
}

impl Default for Npmrc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, env};

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    fn display_store_dir(store_dir: &StoreDir) -> String {
        store_dir.display().to_string().replace('\\', "/")
    }

    #[test]
    pub fn have_default_values() {
        let _env_guard = crate::env_lock().lock().expect("lock env mutex");
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("PNPM_HOME") };
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("XDG_DATA_HOME") };
        let value = Npmrc::new();
        assert_eq!(value.node_linker, NodeLinker::default());
        assert_eq!(value.package_import_method, PackageImportMethod::default());
        assert_eq!(value.network_concurrency, 16);
        assert_eq!(value.fetch_timeout, 60000);
        assert!(value.strict_ssl);
        assert_eq!(value.proxy, None);
        assert_eq!(value.https_proxy, None);
        assert_eq!(value.http_proxy, None);
        assert_eq!(value.no_proxy, None);
        assert!(value.ca.is_empty());
        assert!(value.lockfile);
        assert!(value.prefer_frozen_lockfile);
        assert!(!value.exclude_links_from_lockfile);
        assert!(!value.inject_workspace_packages);
        assert!(value.dedupe_injected_deps);
        assert!(!value.disable_relink_local_dir_deps);
        assert_eq!(value.peers_suffix_max_length, 1000);
        assert!(value.symlink);
        assert!(value.hoist);
        assert!(!value.enable_pre_post_scripts);
        assert_eq!(value.script_shell, None);
        assert!(!value.shell_emulator);
        assert_eq!(value.store_dir, default_store_dir());
        assert_eq!(value.registry, "https://registry.npmjs.org/");
    }

    #[test]
    pub fn parse_package_import_method() {
        let value: Npmrc = serde_ini::from_str("package-import-method=hardlink").unwrap();
        assert_eq!(value.package_import_method, PackageImportMethod::Hardlink);
    }

    #[test]
    pub fn parse_node_linker() {
        let value: Npmrc = serde_ini::from_str("node-linker=hoisted").unwrap();
        assert_eq!(value.node_linker, NodeLinker::Hoisted);
    }

    #[test]
    pub fn parse_single_string_hoist_patterns() {
        let value: Npmrc =
            serde_ini::from_str("hoist-pattern=*\npublic-hoist-pattern=*hello*").unwrap();
        assert_eq!(value.hoist_pattern, vec!["*".to_string()]);
        assert_eq!(value.public_hoist_pattern, vec!["*hello*".to_string()]);
    }

    #[test]
    pub fn parse_bool() {
        let value: Npmrc = serde_ini::from_str("prefer-frozen-lockfile=false").unwrap();
        assert!(!value.prefer_frozen_lockfile);
    }

    #[test]
    pub fn parse_lockfile_related_settings() {
        let value: Npmrc = serde_ini::from_str(
            "exclude-links-from-lockfile=true\ninject-workspace-packages=true\ndedupe-injected-deps=false\ndisable-relink-local-dir-deps=true\npeers-suffix-max-length=77",
        )
        .unwrap();
        assert!(value.exclude_links_from_lockfile);
        assert!(value.inject_workspace_packages);
        assert!(!value.dedupe_injected_deps);
        assert!(value.disable_relink_local_dir_deps);
        assert_eq!(value.peers_suffix_max_length, 77);
    }

    #[test]
    pub fn parse_script_runner_settings() {
        let value: Npmrc = serde_ini::from_str(
            "enable-pre-post-scripts=true\nscript-shell=/bin/bash\nshell-emulator=true",
        )
        .unwrap();
        assert!(value.enable_pre_post_scripts);
        assert_eq!(value.script_shell.as_deref(), Some("/bin/bash"));
        assert!(value.shell_emulator);
    }

    #[test]
    pub fn parse_u64() {
        let value: Npmrc = serde_ini::from_str("modules-cache-max-age=1000").unwrap();
        assert_eq!(value.modules_cache_max_age, 1000);
    }

    #[test]
    pub fn derived_settings_clear_hoist_pattern_when_hoist_is_false() {
        let mut value: Npmrc = serde_ini::from_str("hoist=false").unwrap();
        value.apply_derived_settings();
        assert!(!value.hoist);
        assert!(value.hoist_pattern.is_empty());
    }

    #[test]
    pub fn derived_settings_force_public_hoist_all_when_shamefully_hoist_is_true() {
        let mut value = Npmrc::new();
        value.public_hoist_pattern = vec!["*eslint*".to_string()];
        value.shamefully_hoist = true;
        value.apply_derived_settings();
        assert!(value.shamefully_hoist);
        assert_eq!(value.public_hoist_pattern, vec!["*".to_string()]);
    }

    #[test]
    pub fn derived_settings_clear_hoist_patterns_when_symlink_is_false() {
        let mut value = Npmrc::new();
        value.hoist_pattern = vec!["*".to_string()];
        value.public_hoist_pattern = vec!["*hello*".to_string()];
        value.symlink = false;
        value.apply_derived_settings();
        assert!(!value.symlink);
        assert!(value.hoist_pattern.is_empty());
        assert!(value.public_hoist_pattern.is_empty());
    }

    #[test]
    pub fn parse_network_concurrency() {
        let value: Npmrc = serde_ini::from_str("network-concurrency=8").unwrap();
        assert_eq!(value.network_concurrency, 8);
    }

    #[test]
    pub fn parse_fetch_timeout() {
        let value: Npmrc = serde_ini::from_str("fetch-timeout=45000").unwrap();
        assert_eq!(value.fetch_timeout, 45000);
    }

    #[test]
    pub fn parse_strict_ssl() {
        let value: Npmrc = serde_ini::from_str("strict-ssl=false").unwrap();
        assert!(!value.strict_ssl);
    }

    #[test]
    pub fn current_reads_cafile_into_ca() {
        let project = tempdir().unwrap();
        let cafile = project.path().join("cafile.pem");
        fs::write(&cafile, "xxx\n-----END CERTIFICATE-----").expect("write cafile");
        fs::write(project.path().join(".npmrc"), format!("cafile={}", cafile.display()))
            .expect("write npmrc");

        let config =
            Npmrc::current(|| Ok::<_, ()>(project.path().to_path_buf()), || None, Npmrc::new)
                .expect("load npmrc");
        assert_eq!(config.ca, vec!["xxx\n-----END CERTIFICATE-----".to_string()]);
    }

    #[test]
    pub fn current_reads_inline_ssl_certificates_from_npmrc() {
        let project = tempdir().unwrap();
        let inline_ca = "-----BEGIN CERTIFICATE-----\\nMII-CA\\n-----END CERTIFICATE-----";
        let inline_cert = "-----BEGIN CERTIFICATE-----\\nMII-CERT\\n-----END CERTIFICATE-----";
        let inline_key = "-----BEGIN PRIVATE KEY-----\\nMII-KEY\\n-----END PRIVATE KEY-----";
        fs::write(
            project.path().join(".npmrc"),
            format!(
                "//registry.example.com/:ca={inline_ca}\n//registry.example.com/:cert={inline_cert}\n//registry.example.com/:key={inline_key}\n"
            ),
        )
        .expect("write npmrc");

        let config =
            Npmrc::current(|| Ok::<_, ()>(project.path().to_path_buf()), || None, Npmrc::new)
                .expect("load npmrc");
        assert_eq!(
            config.ssl_configs.get("//registry.example.com/"),
            Some(&RegistrySslConfig {
                ca: Some(
                    "-----BEGIN CERTIFICATE-----\nMII-CA\n-----END CERTIFICATE-----".to_string()
                ),
                cert: Some(
                    "-----BEGIN CERTIFICATE-----\nMII-CERT\n-----END CERTIFICATE-----".to_string()
                ),
                key: Some(
                    "-----BEGIN PRIVATE KEY-----\nMII-KEY\n-----END PRIVATE KEY-----".to_string()
                ),
            })
        );
    }

    #[test]
    pub fn derived_request_settings_follow_pnpm_proxy_priority() {
        let _env_guard = crate::env_lock().lock().expect("lock env mutex");
        unsafe {
            env::remove_var("HTTPS_PROXY");
            env::remove_var("https_proxy");
            env::remove_var("HTTP_PROXY");
            env::remove_var("http_proxy");
            env::remove_var("NO_PROXY");
            env::remove_var("no_proxy");
            env::remove_var("PROXY");
            env::remove_var("proxy");
        }
        let mut config: Npmrc =
            serde_ini::from_str("proxy=http://proxy.example\nnoproxy=localhost").unwrap();
        config.recompute_request_settings();
        assert_eq!(config.https_proxy.as_deref(), Some("http://proxy.example"));
        assert_eq!(config.http_proxy.as_deref(), Some("http://proxy.example"));
        assert_eq!(config.no_proxy.as_deref(), Some("localhost"));
    }

    #[test]
    pub fn derived_request_settings_fallback_to_env_vars() {
        let _env_guard = crate::env_lock().lock().expect("lock env mutex");
        unsafe {
            env::set_var("HTTPS_PROXY", "http://secure-proxy.example");
            env::set_var("HTTP_PROXY", "http://plain-proxy.example");
            env::set_var("NO_PROXY", "localhost,127.0.0.1");
            env::remove_var("PROXY");
            env::remove_var("proxy");
        }
        let mut config: Npmrc = serde_ini::from_str("").unwrap();
        config.https_proxy = None;
        config.http_proxy = None;
        config.no_proxy = None;
        config.recompute_request_settings();
        assert_eq!(config.https_proxy.as_deref(), Some("http://secure-proxy.example"));
        assert_eq!(config.http_proxy.as_deref(), Some("http://secure-proxy.example"));
        assert_eq!(config.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
        unsafe {
            env::remove_var("HTTPS_PROXY");
            env::remove_var("HTTP_PROXY");
            env::remove_var("NO_PROXY");
        }
    }

    #[test]
    pub fn should_use_pnpm_home_env_var() {
        let _env_guard = crate::env_lock().lock().expect("lock env mutex");
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("XDG_DATA_HOME") };
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::set_var("PNPM_HOME", "/hello") }; // TODO: change this to dependency injection
        let value: Npmrc = serde_ini::from_str("").unwrap();
        assert_eq!(display_store_dir(&value.store_dir), "/hello/store");
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("PNPM_HOME") };
    }

    #[test]
    pub fn should_use_xdg_data_home_env_var() {
        let _env_guard = crate::env_lock().lock().expect("lock env mutex");
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("PNPM_HOME") };
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::set_var("XDG_DATA_HOME", "/hello") }; // TODO: change this to dependency injection
        let value: Npmrc = serde_ini::from_str("").unwrap();
        assert_eq!(display_store_dir(&value.store_dir), "/hello/pnpm/store");
        // Safe in this test context: mutate process env in a controlled scope.
        unsafe { env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    pub fn should_use_relative_virtual_store_dir() {
        let value: Npmrc = serde_ini::from_str("virtual-store-dir=node_modules/.pacquet").unwrap();
        assert_eq!(
            value.virtual_store_dir,
            env::current_dir().unwrap().join("node_modules/.pacquet")
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    pub fn should_use_absolute_virtual_store_dir() {
        let value: Npmrc = serde_ini::from_str("virtual-store-dir=/node_modules/.pacquet").unwrap();
        assert_eq!(value.virtual_store_dir, PathBuf::from("/node_modules/.pacquet"));
    }

    #[test]
    pub fn add_slash_to_registry_end() {
        let without_slash: Npmrc = serde_ini::from_str("registry=https://yagiz.co").unwrap();
        assert_eq!(without_slash.registry, "https://yagiz.co/");

        let without_slash: Npmrc = serde_ini::from_str("registry=https://yagiz.co/").unwrap();
        assert_eq!(without_slash.registry, "https://yagiz.co/");
    }

    #[test]
    pub fn registry_for_package_name_uses_scoped_registry_from_raw_settings() {
        let mut config = Npmrc::new();
        config.registry = "https://default.example/".to_string();
        config.raw_settings = HashMap::from([
            ("registry".to_string(), "https://default.example/".to_string()),
            ("@foo:registry".to_string(), "https://foo.example".to_string()),
        ]);

        assert_eq!(config.registry_for_package_name("@foo/pkg"), "https://foo.example/");
        assert_eq!(config.registry_for_package_name("@bar/pkg"), "https://default.example/");
        assert_eq!(config.registry_for_package_name("is-number"), "https://default.example/");
    }

    #[test]
    pub fn current_merges_scoped_registries_from_home_and_project_npmrc() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();

        fs::write(
            home.path().join(".npmrc"),
            "registry=https://default.example/\n@foo:registry=https://foo.example\n",
        )
        .expect("write home npmrc");
        fs::write(project.path().join(".npmrc"), "@bar:registry=https://bar.example/\n")
            .expect("write project npmrc");

        let config = Npmrc::current(
            || Ok::<_, ()>(project.path().to_path_buf()),
            || Some(home.path().to_path_buf()),
            Npmrc::new,
        )
        .expect("load npmrc");

        assert_eq!(config.registry_for_package_name("@foo/pkg"), "https://foo.example/");
        assert_eq!(config.registry_for_package_name("@bar/pkg"), "https://bar.example/");
        assert_eq!(config.registry_for_package_name("is-number"), "https://default.example/");
    }

    #[test]
    pub fn test_current_folder_for_npmrc() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".npmrc"), "symlink=false").expect("write to .npmrc");
        let config = Npmrc::current(
            || tmp.path().to_path_buf().pipe(Ok::<_, ()>),
            || None,
            || unreachable!("shouldn't reach default"),
        )
        .expect("load npmrc");
        assert!(!config.symlink);
    }

    #[test]
    pub fn test_current_folder_for_invalid_npmrc() {
        let tmp = tempdir().unwrap();
        // write invalid utf-8 value to npmrc
        fs::write(tmp.path().join(".npmrc"), b"Hello \xff World").expect("write to .npmrc");
        let config =
            Npmrc::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Npmrc::new)
                .expect("load npmrc");
        assert!(config.symlink); // TODO: what the hell? why succeed?
    }

    #[test]
    pub fn test_current_folder_fallback_to_home() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        dbg!(&current_dir, &home_dir);
        fs::write(home_dir.path().join(".npmrc"), "symlink=false").expect("write to .npmrc");
        let config = Npmrc::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            || unreachable!("shouldn't reach home dir"),
        )
        .expect("load npmrc");
        assert!(!config.symlink);
    }

    #[test]
    pub fn test_current_folder_fallback_to_default() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        let config = Npmrc::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            || serde_ini::from_str("symlink=false").unwrap(),
        )
        .expect("load npmrc");
        assert!(!config.symlink);
    }

    #[test]
    pub fn auth_header_uses_longest_matching_prefix() {
        let mut config = Npmrc::new();
        config.registry = "https://reg.example/".to_string();
        config.raw_settings = HashMap::from([
            ("registry".to_string(), "https://reg.example/".to_string()),
            ("//reg.example/:_authToken".to_string(), "outer".to_string()),
            ("//reg.example/tarballs/:_authToken".to_string(), "inner".to_string()),
        ]);
        config.recompute_auth_headers().expect("recompute auth headers");

        assert_eq!(
            config.auth_header_for_url("https://reg.example/tarballs/pkg/-/pkg-1.0.0.tgz"),
            Some("Bearer inner".to_string())
        );
        assert_eq!(
            config.auth_header_for_url("https://reg.example/@scope/pkg"),
            Some("Bearer outer".to_string())
        );
    }

    #[test]
    pub fn auth_header_supports_global_auth_token_and_default_port() {
        let mut config = Npmrc::new();
        config.registry = "https://reg.example/".to_string();
        config.raw_settings = HashMap::from([
            ("registry".to_string(), "https://reg.example/".to_string()),
            ("_authToken".to_string(), "abc123".to_string()),
        ]);
        config.recompute_auth_headers().expect("recompute auth headers");

        assert_eq!(
            config.auth_header_for_url("https://reg.example:443/pkg"),
            Some("Bearer abc123".to_string())
        );
    }

    #[test]
    pub fn auth_header_prefers_basic_auth_from_url_credentials() {
        let config = Npmrc::new();
        let expected = format!("Basic {}", BASE64_STD.encode("foo:bar"));
        assert_eq!(config.auth_header_for_url("https://foo:bar@reg.example/pkg"), Some(expected));
    }

    #[test]
    pub fn current_merges_raw_auth_settings_from_home_and_project_npmrc() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();

        fs::write(
            home.path().join(".npmrc"),
            "_authToken=from-home\nregistry=https://reg.example/\n",
        )
        .expect("write home npmrc");
        fs::write(project.path().join(".npmrc"), "//reg.example/:_authToken=from-project\n")
            .expect("write project npmrc");

        let config = Npmrc::current(
            || Ok::<_, ()>(project.path().to_path_buf()),
            || Some(home.path().to_path_buf()),
            Npmrc::new,
        )
        .expect("load npmrc");

        assert_eq!(
            config.auth_header_for_url("https://reg.example/foo"),
            Some("Bearer from-project".to_string())
        );
    }

    #[test]
    pub fn current_fails_for_non_absolute_token_helper() {
        let project = tempdir().expect("tempdir");
        fs::write(project.path().join(".npmrc"), "tokenHelper=./token-helper")
            .expect("write npmrc");
        let error =
            Npmrc::current(|| Ok::<_, ()>(project.path().to_path_buf()), || None, Npmrc::new)
                .expect_err("non-absolute tokenHelper should fail");
        assert!(error.to_string().contains("tokenHelper must be an absolute path"));
    }

    #[test]
    pub fn current_fails_for_non_existent_scoped_token_helper() {
        let project = tempdir().expect("tempdir");
        fs::write(
            project.path().join(".npmrc"),
            "//reg.example/:tokenHelper=/does/not/exist\nregistry=https://reg.example/\n",
        )
        .expect("write npmrc");
        let error =
            Npmrc::current(|| Ok::<_, ()>(project.path().to_path_buf()), || None, Npmrc::new)
                .expect_err("missing scoped tokenHelper should fail");
        assert!(error.to_string().contains("//reg.example/:tokenHelper"));
    }

    #[test]
    pub fn current_ignores_token_helper_from_home_config() {
        let home = tempdir().expect("home tempdir");
        let project = tempdir().expect("project tempdir");
        fs::write(
            home.path().join(".npmrc"),
            "tokenHelper=/does/not/exist\nregistry=https://reg.example/\n",
        )
        .expect("write home npmrc");
        fs::write(project.path().join(".npmrc"), "registry=https://reg.example/\n")
            .expect("write project npmrc");

        let config = Npmrc::current(
            || Ok::<_, ()>(project.path().to_path_buf()),
            || Some(home.path().to_path_buf()),
            Npmrc::new,
        );
        assert!(config.is_ok());
    }
}
