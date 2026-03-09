use derive_more::{Display, Error};
use glob::Pattern;
use miette::Diagnostic;
use pacquet_lockfile::{LoadLockfileError, Lockfile};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::{ResolvedPackages, WorkspacePackageInfo, WorkspacePackages};
use pacquet_package_manifest::{PackageManifest, PackageManifestError};
use pacquet_tarball::MemCache;
use pipe_trait::Pipe;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Application state when running `pacquet run` or `pacquet install`.
pub struct State {
    /// Shared cache that store downloaded tarballs.
    pub tarball_mem_cache: MemCache,
    /// HTTP client to make HTTP requests.
    pub http_client: ThrottledClient,
    /// Configuration read from `.npmrc`
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub manifest: PackageManifest,
    /// Data from the `pnpm-lock.yaml` file.
    pub lockfile: Option<Lockfile>,
    /// Directory where `pnpm-lock.yaml` is loaded from and saved to.
    pub lockfile_dir: PathBuf,
    /// Importer key used inside lockfile's `importers` map.
    pub lockfile_importer_id: String,
    /// Workspace package map used for resolving `workspace:` dependencies.
    pub workspace_packages: WorkspacePackages,
    /// In-memory cache for packages that have started resolving dependencies.
    pub resolved_packages: ResolvedPackages,
}

/// Error type of [`State::init`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum InitStateError {
    #[diagnostic(transparent)]
    LoadManifest(#[error(source)] PackageManifestError),

    #[diagnostic(transparent)]
    LoadLockfile(#[error(source)] LoadLockfileError),
}

impl State {
    /// Initialize the application state.
    pub fn init(manifest_path: PathBuf, config: &'static Npmrc) -> Result<Self, InitStateError> {
        let project_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let workspace_root = find_workspace_root(&project_dir);
        let lockfile_dir = workspace_root.clone().unwrap_or_else(|| project_dir.clone());
        let lockfile_importer_id = to_lockfile_importer_id(workspace_root.as_deref(), &project_dir);
        let workspace_packages =
            workspace_root.as_deref().map(collect_workspace_packages).unwrap_or_default();

        Ok(State {
            config,
            manifest: manifest_path
                .pipe(PackageManifest::create_if_needed)
                .map_err(InitStateError::LoadManifest)?,
            lockfile: call_load_lockfile(config.lockfile, || {
                Lockfile::load_from_dir(&lockfile_dir)
            })
            .map_err(InitStateError::LoadLockfile)?,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            http_client: ThrottledClient::new_with_limit(config.network_concurrency as usize),
            tarball_mem_cache: MemCache::new(),
            resolved_packages: ResolvedPackages::new(),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceManifest {
    packages: Option<Vec<String>>,
}

fn collect_workspace_packages(workspace_root: &Path) -> WorkspacePackages {
    let patterns = read_workspace_package_patterns(workspace_root);
    let package_json_paths = collect_package_json_paths(workspace_root);

    package_json_paths
        .into_iter()
        .filter_map(|manifest_path| {
            let root_dir = manifest_path.parent()?.to_path_buf();
            if root_dir != workspace_root
                && !workspace_path_matches_patterns(workspace_root, &root_dir, patterns.as_deref())
            {
                return None;
            }

            let manifest = PackageManifest::from_path(manifest_path).ok()?;
            let name = manifest.value().get("name").and_then(|name| name.as_str())?.to_string();
            let version = manifest
                .value()
                .get("version")
                .and_then(|version| version.as_str())
                .unwrap_or("0.0.0")
                .to_string();
            Some((name, WorkspacePackageInfo { root_dir, version }))
        })
        .collect()
}

fn read_workspace_package_patterns(workspace_root: &Path) -> Option<Vec<String>> {
    let manifest_path = workspace_root.join("pnpm-workspace.yaml");
    let manifest_text = fs::read_to_string(manifest_path).ok()?;
    let manifest = serde_yaml::from_str::<WorkspaceManifest>(&manifest_text).ok()?;
    manifest.packages
}

fn collect_package_json_paths(workspace_root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, result: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };

            if file_type.is_file() && file_name == "package.json" {
                result.push(path);
                continue;
            }

            if !file_type.is_dir() {
                continue;
            }

            if matches!(file_name.as_ref(), "node_modules" | ".git" | "target") {
                continue;
            }

            walk(&path, result);
        }
    }

    let mut result = Vec::new();
    walk(workspace_root, &mut result);
    result
}

fn workspace_path_matches_patterns(
    workspace_root: &Path,
    package_dir: &Path,
    patterns: Option<&[String]>,
) -> bool {
    let Some(patterns) = patterns else {
        return true;
    };

    let relative = package_dir.strip_prefix(workspace_root).unwrap_or(package_dir);
    let relative = relative.to_string_lossy().replace('\\', "/");

    let mut included = false;
    for pattern in patterns {
        if let Some(pattern) = pattern.strip_prefix('!') {
            if workspace_pattern_matches(pattern, &relative) {
                included = false;
            }
        } else if workspace_pattern_matches(pattern, &relative) {
            included = true;
        }
    }

    included
}

fn workspace_pattern_matches(pattern: &str, path: &str) -> bool {
    Pattern::new(pattern).map(|pattern| pattern.matches(path)).unwrap_or(false)
}

pub(crate) fn find_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join("pnpm-workspace.yaml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn to_lockfile_importer_id(workspace_root: Option<&Path>, project_dir: &Path) -> String {
    let Some(workspace_root) = workspace_root else {
        return ".".to_string();
    };

    let Ok(relative) = project_dir.strip_prefix(workspace_root) else {
        return ".".to_string();
    };
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }

    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

/// Private function to load lockfile from current directory should `config.lockfile` is `true`.
///
/// This function was extracted to be tested independently.
fn call_load_lockfile<LoadLockfile, Lockfile, Error>(
    config_lockfile: bool,
    load_lockfile: LoadLockfile,
) -> Result<Option<Lockfile>, Error>
where
    LoadLockfile: FnOnce() -> Result<Option<Lockfile>, Error>,
{
    config_lockfile.then(load_lockfile).transpose().map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn test_call_load_lockfile() {
        macro_rules! case {
            ($config_lockfile:expr, $load_lockfile:expr => $output:expr) => {{
                let config_lockfile = $config_lockfile;
                let load_lockfile = $load_lockfile;
                let output: Result<Option<&str>, &str> = $output;
                eprintln!(
                    "CASE: {config_lockfile:?}, {load_lockfile} => {output:?}",
                    load_lockfile = stringify!($load_lockfile),
                );
                assert_eq!(call_load_lockfile(config_lockfile, load_lockfile), output);
            }};
        }

        case!(false, || unreachable!() => Ok(None));
        case!(true, || Err("error") => Err("error"));
        case!(true, || Ok(None) => Ok(None));
        case!(true, || Ok(Some("value")) => Ok(Some("value")));
    }

    #[test]
    fn importer_id_for_non_workspace() {
        assert_eq!(to_lockfile_importer_id(None, Path::new("/project")), ".".to_string());
    }

    #[test]
    fn importer_id_for_workspace_root() {
        assert_eq!(
            to_lockfile_importer_id(Some(Path::new("/repo")), Path::new("/repo")),
            ".".to_string()
        );
    }

    #[test]
    fn importer_id_for_workspace_child() {
        assert_eq!(
            to_lockfile_importer_id(Some(Path::new("/repo")), Path::new("/repo/packages/app")),
            "packages/app".to_string()
        );
    }

    #[test]
    fn detect_workspace_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        let nested = root.join("packages").join("app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("pnpm-workspace.yaml"), "packages:\n  - packages/*\n").unwrap();
        assert_eq!(find_workspace_root(&nested).as_deref(), Some(root.as_path()));
    }

    #[test]
    fn workspace_path_patterns() {
        let root = Path::new("/repo");
        assert!(workspace_path_matches_patterns(
            root,
            Path::new("/repo/packages/a"),
            Some(&["packages/*".to_string()])
        ));
        assert!(!workspace_path_matches_patterns(
            root,
            Path::new("/repo/examples/a"),
            Some(&["packages/*".to_string(), "!packages/private/*".to_string()])
        ));
    }

    #[test]
    fn init_uses_network_concurrency_from_npmrc() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("package.json");
        let mut config = Npmrc::new();
        config.network_concurrency = 3;
        let config = config.leak();

        let state = State::init(manifest_path, config).expect("initialize state");
        assert_eq!(state.http_client.concurrency_limit(), 3);
    }
}
