mod comver;
mod dependency_path;
mod load_lockfile;
mod lockfile_file;
mod lockfile_version;
mod multi_project_snapshot;
mod package_snapshot;
mod package_snapshot_dependency;
mod pkg_name;
mod pkg_name_suffix;
mod pkg_name_ver;
mod pkg_name_ver_peer;
mod pkg_ver_peer;
mod project_snapshot;
mod resolution;
mod resolved_dependency;
mod root_project_snapshot;
mod save_lockfile;

pub use comver::*;
pub use dependency_path::*;
pub use load_lockfile::*;
pub use lockfile_version::*;
pub use multi_project_snapshot::*;
pub use package_snapshot::*;
pub use package_snapshot_dependency::*;
pub use pkg_name::*;
pub use pkg_name_suffix::*;
pub use pkg_name_ver::*;
pub use pkg_name_ver_peer::*;
pub use pkg_ver_peer::*;
pub use project_snapshot::*;
pub use resolution::*;
pub use resolved_dependency::*;
pub use root_project_snapshot::*;
pub use save_lockfile::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockfileSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_install_peers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_links_from_lockfile: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_suffix_max_length: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_workspace_packages: Option<bool>,
}

/// Internal lockfile representation used by Pacquet.
///
/// This struct is intentionally format-agnostic: read/write code converts between
/// this internal model and concrete on-disk lockfile formats.
#[derive(Debug, Clone, PartialEq)]
pub struct Lockfile {
    pub lockfile_version: ComVer,
    pub settings: Option<LockfileSettings>,
    /// Legacy v6 field.
    pub never_built_dependencies: Option<Vec<String>>,
    pub ignored_optional_dependencies: Option<Vec<String>>,
    pub overrides: Option<HashMap<String, String>>,
    pub package_extensions_checksum: Option<String>,
    pub patched_dependencies: Option<HashMap<String, serde_yaml::Value>>,
    pub pnpmfile_checksum: Option<String>,
    pub catalogs: Option<serde_yaml::Value>,
    pub time: Option<HashMap<String, String>>,
    pub project_snapshot: RootProjectSnapshot,
    pub packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pnpm-lock.yaml";
}
