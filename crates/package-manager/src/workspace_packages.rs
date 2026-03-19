use std::{collections::HashMap, path::PathBuf};

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
    let raw = specifier.strip_prefix("workspace:")?;
    let (target_name, range) = parse_workspace_target(dependency_name, raw);
    let package = workspace_packages.get(target_name)?;

    if !range_is_satisfied(&package.version, range) {
        return None;
    }

    Some(package)
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
    fn resolve_plain_spec_by_exact_range() {
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "2.0.0").is_some());
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "1.0.0").is_none());
    }

    #[test]
    fn resolve_plain_spec_by_wildcard_range() {
        assert!(resolve_workspace_dependency_by_plain_spec(&packages(), "b", "^2").is_some());
    }
}
