use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDependencyError {
    PackageNotFound { dependency_name: String, target_name: String, specifier: String },
    NoMatchingVersion { dependency_name: String, specifier: String, available_version: String },
}

impl std::fmt::Display for WorkspaceDependencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PackageNotFound { dependency_name, target_name, specifier } => write!(
                f,
                "\"{dependency_name}@{specifier}\" is in the dependencies but no package named \"{target_name}\" is present in the workspace"
            ),
            Self::NoMatchingVersion { dependency_name, specifier, available_version } => write!(
                f,
                "No matching version found for {dependency_name}@{specifier} inside the workspace. Available versions: {available_version}"
            ),
        }
    }
}

/// Metadata of a package that is part of the current workspace.
#[derive(Debug, Clone)]
pub struct WorkspacePackageInfo {
    pub root_dir: PathBuf,
    pub version: String,
}

/// Map from package name to workspace package metadata.
pub type WorkspacePackages = HashMap<String, WorkspacePackageInfo>;

/// Resolve a `workspace:` dependency specifier to a workspace package.
pub fn resolve_workspace_dependency<'a>(
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
) -> Option<&'a WorkspacePackageInfo> {
    require_workspace_dependency(workspace_packages, dependency_name, specifier).ok()
}

pub fn require_workspace_dependency<'a>(
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
) -> Result<&'a WorkspacePackageInfo, WorkspaceDependencyError> {
    let raw = specifier.strip_prefix("workspace:").ok_or_else(|| {
        WorkspaceDependencyError::PackageNotFound {
            dependency_name: dependency_name.to_string(),
            target_name: dependency_name.to_string(),
            specifier: specifier.to_string(),
        }
    })?;
    let (target_name, range) = parse_workspace_target(dependency_name, raw);
    let Some(package) = workspace_packages.get(target_name) else {
        return Err(WorkspaceDependencyError::PackageNotFound {
            dependency_name: dependency_name.to_string(),
            target_name: target_name.to_string(),
            specifier: specifier.to_string(),
        });
    };

    if !range_is_satisfied(&package.version, range) {
        return Err(WorkspaceDependencyError::NoMatchingVersion {
            dependency_name: dependency_name.to_string(),
            specifier: specifier.to_string(),
            available_version: package.version.clone(),
        });
    }

    Ok(package)
}

/// Resolve a non-`workspace:` dependency range to a workspace package of the same name.
pub fn resolve_workspace_dependency_by_plain_spec<'a>(
    workspace_packages: &'a WorkspacePackages,
    dependency_name: &str,
    specifier: &str,
) -> Option<&'a WorkspacePackageInfo> {
    let package = workspace_packages.get(dependency_name)?;

    if !range_is_satisfied(&package.version, specifier) {
        return None;
    }

    Some(package)
}

pub fn resolve_workspace_dependency_by_relative_path<'a>(
    workspace_packages: &'a WorkspacePackages,
    project_dir: &Path,
    specifier: &str,
) -> Option<&'a WorkspacePackageInfo> {
    let raw = specifier.strip_prefix("workspace:")?;
    let candidate = Path::new(raw);
    if !(candidate.is_absolute()
        || raw == "."
        || raw == ".."
        || raw.starts_with("./")
        || raw.starts_with("../"))
    {
        return None;
    }

    let target_dir = if candidate.is_absolute() {
        normalize_path(candidate)
    } else {
        normalize_path(&project_dir.join(candidate))
    };

    workspace_packages.values().find(|package| normalize_path(&package.root_dir) == target_dir)
}

fn parse_workspace_target<'a>(dependency_name: &'a str, raw: &'a str) -> (&'a str, &'a str) {
    if raw.is_empty() || raw == "*" || raw == "^" || raw == "~" {
        return (dependency_name, raw);
    }

    // Support aliases like `workspace:@scope/pkg@*` and `workspace:pkg@^1.0.0`.
    let maybe_alias = if let Some(scoped_name) = raw.strip_prefix('@') {
        scoped_name.rfind('@').map(|index| index + 1)
    } else {
        raw.rfind('@')
    };

    if let Some(index) = maybe_alias {
        let name = &raw[..index];
        let range = &raw[(index + 1)..];
        if !name.is_empty() && !range.is_empty() {
            return (name, range);
        }
    }

    (dependency_name, raw)
}

fn range_is_satisfied(version: &str, range: &str) -> bool {
    if range.is_empty() || range == "*" || range == "^" || range == "~" {
        return true;
    }

    let Ok(version) = version.parse::<node_semver::Version>() else {
        return false;
    };
    let Ok(range) = range.parse::<node_semver::Range>() else {
        return false;
    };
    version.satisfies(&range)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packages() -> WorkspacePackages {
        [
            (
                "@scope/a".to_string(),
                WorkspacePackageInfo {
                    root_dir: PathBuf::from("/repo/packages/a"),
                    version: "1.2.3".to_string(),
                },
            ),
            (
                "b".to_string(),
                WorkspacePackageInfo {
                    root_dir: PathBuf::from("/repo/packages/b"),
                    version: "2.0.0".to_string(),
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn resolve_by_wildcard() {
        assert!(resolve_workspace_dependency(&packages(), "b", "workspace:*").is_some());
    }

    #[test]
    fn resolve_by_exact_range() {
        assert!(resolve_workspace_dependency(&packages(), "b", "workspace:2.0.0").is_some());
        assert!(resolve_workspace_dependency(&packages(), "b", "workspace:1.0.0").is_none());
    }

    #[test]
    fn resolve_alias_target() {
        assert!(
            resolve_workspace_dependency(&packages(), "alias", "workspace:@scope/a@*").is_some()
        );
    }

    #[test]
    fn resolve_by_relative_path() {
        let packages = [
            (
                "foo".to_string(),
                WorkspacePackageInfo {
                    root_dir: PathBuf::from("/repo/packages/foo"),
                    version: "1.0.0".to_string(),
                },
            ),
            (
                "bar".to_string(),
                WorkspacePackageInfo {
                    root_dir: PathBuf::from("/repo/packages/bar"),
                    version: "1.0.0".to_string(),
                },
            ),
        ]
        .into_iter()
        .collect::<WorkspacePackages>();

        assert_eq!(
            resolve_workspace_dependency_by_relative_path(
                &packages,
                Path::new("/repo/packages/bar"),
                "workspace:../foo",
            )
            .map(|package| package.root_dir.clone()),
            Some(PathBuf::from("/repo/packages/foo"))
        );
    }

    #[test]
    fn require_workspace_dependency_returns_missing_package_error() {
        assert_eq!(
            require_workspace_dependency(&packages(), "missing", "workspace:*").unwrap_err(),
            WorkspaceDependencyError::PackageNotFound {
                dependency_name: "missing".to_string(),
                target_name: "missing".to_string(),
                specifier: "workspace:*".to_string(),
            }
        );
    }

    #[test]
    fn require_workspace_dependency_returns_version_mismatch_error() {
        assert_eq!(
            require_workspace_dependency(&packages(), "b", "workspace:1.0.0").unwrap_err(),
            WorkspaceDependencyError::NoMatchingVersion {
                dependency_name: "b".to_string(),
                specifier: "workspace:1.0.0".to_string(),
                available_version: "2.0.0".to_string(),
            }
        );
    }

    #[test]
    fn resolve_plain_spec_by_exact_range() {
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "2.0.0").is_some());
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "1.0.0").is_none());
    }

    #[test]
    fn resolve_plain_spec_by_wildcard_range() {
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "^2").is_some());
    }
}
