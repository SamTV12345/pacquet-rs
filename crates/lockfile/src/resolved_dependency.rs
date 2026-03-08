use crate::{PkgName, PkgNameVerPeer, PkgVerPeer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Map of resolved dependencies stored in a [`ProjectSnapshot`](crate::ProjectSnapshot).
///
/// The keys are package names.
pub type ResolvedDependencyMap = HashMap<PkgName, ResolvedDependencySpec>;

/// Value type of [`ResolvedDependencyMap`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedDependencySpec {
    pub specifier: String,
    pub version: ResolvedDependencyVersion,
}

/// Version field of a resolved dependency in importer snapshot.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ResolvedDependencyVersion {
    /// Registry/tarball resolved package version with optional peer suffix.
    PkgVerPeer(PkgVerPeer),
    /// Alias to another package name with pinned version/peer suffix.
    PkgNameVerPeer(PkgNameVerPeer),
    /// Workspace/local link version (for example: `link:../foo`).
    Link(String),
}

impl fmt::Display for ResolvedDependencyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolvedDependencyVersion::PkgVerPeer(value) => write!(f, "{value}"),
            ResolvedDependencyVersion::PkgNameVerPeer(value) => write!(f, "{value}"),
            ResolvedDependencyVersion::Link(value) => write!(f, "{value}"),
        }
    }
}
