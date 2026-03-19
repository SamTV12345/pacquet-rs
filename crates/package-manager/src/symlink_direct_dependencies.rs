use crate::{
    SymlinkPackageError, import_local_package_dir, link_package, should_materialize_root_links,
    symlink_package,
};
use pacquet_fs::{is_symlink_or_junction, symlink_or_junction_target};
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PkgName, PkgNameVerPeer, ProjectSnapshot,
    ResolvedDependencyVersion,
};
use pacquet_npmrc::{Npmrc, PackageImportMethod};
use pacquet_package_manifest::DependencyGroup;
use rayon::prelude::*;
use serde_yaml::Value as YamlValue;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// This subroutine creates symbolic links in the `node_modules` directory for
/// the direct dependencies. The targets of the link are the virtual directories.
///
/// If package `foo@x.y.z` is declared as a dependency in `package.json`,
/// symlink `foo -> .pacquet/foo@x.y.z/node_modules/foo` shall be created
/// in the `node_modules` directory.
#[must_use]
pub struct SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub config: &'static Npmrc,
    pub project_snapshot: &'a ProjectSnapshot,
    pub packages: Option<&'a std::collections::HashMap<DependencyPath, PackageSnapshot>>,
    pub dependency_groups: DependencyGroupList,
}

impl<'a, DependencyGroupList> SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub fn run(self) {
        let SymlinkDirectDependencies { config, project_snapshot, packages, dependency_groups } =
            self;
        if !should_materialize_root_links(config) {
            return;
        }

        project_snapshot
            .dependencies_by_groups(dependency_groups)
            .collect::<Vec<_>>()
            .par_iter()
            .for_each(|(name, spec)| {
                let name_str = name.to_string();
                let (symlink_target, materialize_virtual_store_tree) = match &spec.version {
                    ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
                        let virtual_store_name =
                            PkgNameVerPeer::new(PkgName::clone(name), ver_peer.clone())
                                .to_virtual_store_name();
                        (
                            config
                                .virtual_store_dir
                                .join(virtual_store_name)
                                .join("node_modules")
                                .join(&name_str),
                            false,
                        )
                    }
                    ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
                        let virtual_store_name = name_ver_peer.to_virtual_store_name();
                        let target_name = name_ver_peer.name.to_string();
                        (
                            config
                                .virtual_store_dir
                                .join(virtual_store_name)
                                .join("node_modules")
                                .join(target_name),
                            false,
                        )
                    }
                    ResolvedDependencyVersion::Link(link) => {
                        local_link_target(config, packages, &name_str, link)
                    }
                };
                let dependency_path = config.modules_dir.join(&name_str);
                let should_refresh_existing = matches!(
                    &spec.version,
                    ResolvedDependencyVersion::Link(link)
                        if link.starts_with("file:")
                            || should_materialize_workspace_dependency(
                                project_snapshot,
                                &name_str,
                                &spec.specifier,
                                &spec.version,
                                config,
                            )
                ) && !config.disable_relink_local_dir_deps;
                if dependency_path.exists() && !should_refresh_existing {
                    return;
                }

                let result = match &spec.version {
                    ResolvedDependencyVersion::Link(link) => {
                        let should_materialize_local_copy = should_materialize_workspace_dependency(
                            project_snapshot,
                            &name_str,
                            &spec.specifier,
                            &spec.version,
                            config,
                        ) || link.starts_with("file:")
                            || spec.specifier.starts_with("file:");
                        if should_materialize_local_copy {
                            if materialize_virtual_store_tree {
                                materialize_virtual_store_package(
                                    config.package_import_method,
                                    &symlink_target,
                                    &dependency_path,
                                )
                            } else {
                                import_local_package_dir(
                                    config.package_import_method,
                                    &symlink_target,
                                    &dependency_path,
                                )
                            }
                        } else {
                            if !config.symlink {
                                return;
                            }
                            symlink_package(&symlink_target, &dependency_path)
                        }
                    }
                    _ => link_package(config.symlink, &symlink_target, &dependency_path),
                };
                result.unwrap_or_else(|error| {
                    panic!("direct dependency symlink should succeed ({name_str}): {error}")
                });
            });
    }
}

fn local_link_target(
    config: &Npmrc,
    packages: Option<&std::collections::HashMap<DependencyPath, PackageSnapshot>>,
    dependency_name: &str,
    link: &str,
) -> (PathBuf, bool) {
    if link.starts_with("file:")
        && let Some(target) = packages.and_then(|packages| {
            packages.iter().find_map(|(dependency_path, _)| {
                (dependency_path.local_file_reference() == Some(link)
                    && dependency_path.package_name().to_string() == dependency_name)
                    .then(|| {
                        config
                            .virtual_store_dir
                            .join(dependency_path.to_virtual_store_name())
                            .join("node_modules")
                            .join(dependency_path.package_name().to_string())
                    })
            })
        })
    {
        return (target, true);
    }

    let relative =
        link.strip_prefix("link:").or_else(|| link.strip_prefix("file:")).unwrap_or(link);
    let base_target =
        config.modules_dir.parent().unwrap_or(config.modules_dir.as_path()).join(relative);
    (link_target_with_publish_config_directory(&base_target), false)
}

pub(crate) fn link_target_with_publish_config_directory(target: &Path) -> PathBuf {
    let manifest_path = target.join("package.json");
    let Ok(content) = fs::read_to_string(&manifest_path) else {
        return target.to_path_buf();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return target.to_path_buf();
    };
    let Some(publish_config) = value.get("publishConfig") else {
        return target.to_path_buf();
    };
    let link_directory =
        publish_config.get("linkDirectory").and_then(serde_json::Value::as_bool).unwrap_or(false);
    if !link_directory {
        return target.to_path_buf();
    }
    let Some(directory) = publish_config.get("directory").and_then(serde_json::Value::as_str)
    else {
        return target.to_path_buf();
    };
    if directory.is_empty() {
        return target.to_path_buf();
    }
    target.join(directory)
}

fn materialize_virtual_store_package(
    import_method: PackageImportMethod,
    source_package_dir: &Path,
    destination_package_dir: &Path,
) -> Result<(), SymlinkPackageError> {
    fn resolved_link_target(path: &Path) -> Option<PathBuf> {
        let target = symlink_or_junction_target(path).ok()?;
        let resolved = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).join(target)
        };
        fs::canonicalize(resolved).ok()
    }

    fn should_preserve_link(path: &Path, virtual_store_dir: &Path) -> bool {
        let Some(target) = resolved_link_target(path) else {
            return false;
        };
        !target.starts_with(virtual_store_dir)
    }

    let mut seen = HashSet::new();
    let mut pending =
        vec![(source_package_dir.to_path_buf(), destination_package_dir.to_path_buf())];

    while let Some((source_package_dir, destination_package_dir)) = pending.pop() {
        let source_package_dir = fs::canonicalize(&source_package_dir).map_err(|error| {
            SymlinkPackageError::CanonicalizePath { path: source_package_dir.to_path_buf(), error }
        })?;
        if !seen.insert(source_package_dir.clone()) {
            continue;
        }

        import_local_package_dir(import_method, &source_package_dir, &destination_package_dir)?;

        let dependency_source_dir = if source_package_dir
            .ancestors()
            .any(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some(".pnpm"))
        {
            source_package_dir
                .ancestors()
                .skip(1)
                .find(|ancestor| {
                    ancestor.file_name().and_then(|name| name.to_str()) == Some("node_modules")
                })
                .map(Path::to_path_buf)
                .unwrap_or_else(|| source_package_dir.join("node_modules"))
        } else {
            source_package_dir.join("node_modules")
        };
        if !dependency_source_dir.is_dir() {
            continue;
        }
        let virtual_store_dir = source_package_dir
            .ancestors()
            .find(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some(".pnpm"))
            .unwrap_or_else(|| source_package_dir.parent().unwrap_or(source_package_dir.as_path()));
        let entries = match fs::read_dir(&dependency_source_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            if file_name_str == ".bin" {
                continue;
            }

            if file_name_str.starts_with('@') {
                if entry.path().join("package.json").is_file() {
                    let source_dependency_dir = entry.path();
                    if source_dependency_dir == source_package_dir {
                        continue;
                    }
                    let destination_dependency_dir =
                        destination_package_dir.join("node_modules").join(&file_name);
                    if is_symlink_or_junction(&source_dependency_dir).unwrap_or(false)
                        && should_preserve_link(&source_dependency_dir, virtual_store_dir)
                    {
                        symlink_package(&source_dependency_dir, &destination_dependency_dir)?;
                        continue;
                    }
                    pending.push((source_dependency_dir, destination_dependency_dir));
                    continue;
                }

                let scoped_entries = match fs::read_dir(entry.path()) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for scoped_entry in scoped_entries.flatten() {
                    let scoped_name = scoped_entry.file_name();
                    let scoped_source_dir = scoped_entry.path();
                    let scoped_target_name =
                        format!("{}/{}", file_name_str, scoped_name.to_string_lossy());
                    if scoped_source_dir == source_package_dir {
                        continue;
                    }
                    let destination_dependency_dir =
                        destination_package_dir.join("node_modules").join(scoped_target_name);
                    if is_symlink_or_junction(&scoped_source_dir).unwrap_or(false)
                        && should_preserve_link(&scoped_source_dir, virtual_store_dir)
                    {
                        symlink_package(&scoped_source_dir, &destination_dependency_dir)?;
                        continue;
                    }
                    pending.push((scoped_source_dir, destination_dependency_dir));
                }
                continue;
            }

            let source_dependency_dir = entry.path();
            if source_dependency_dir == source_package_dir {
                continue;
            }
            let destination_dependency_dir =
                destination_package_dir.join("node_modules").join(&file_name);
            if is_symlink_or_junction(&source_dependency_dir).unwrap_or(false)
                && should_preserve_link(&source_dependency_dir, virtual_store_dir)
            {
                symlink_package(&source_dependency_dir, &destination_dependency_dir)?;
                continue;
            }
            pending.push((source_dependency_dir, destination_dependency_dir));
        }
    }

    Ok(())
}

fn should_materialize_workspace_dependency(
    project_snapshot: &ProjectSnapshot,
    dependency_name: &str,
    specifier: &str,
    resolved_version: &ResolvedDependencyVersion,
    config: &Npmrc,
) -> bool {
    if !specifier.starts_with("workspace:") {
        return false;
    }
    if !matches!(resolved_version, ResolvedDependencyVersion::Link(link) if link.starts_with("file:"))
    {
        return false;
    }
    config.inject_workspace_packages
        || project_snapshot_dependency_meta_injected(project_snapshot, dependency_name)
}

fn project_snapshot_dependency_meta_injected(
    project_snapshot: &ProjectSnapshot,
    dependency_name: &str,
) -> bool {
    project_snapshot
        .dependencies_meta
        .as_ref()
        .and_then(|value| value.get(dependency_name))
        .and_then(|value| value.get("injected"))
        .and_then(YamlValue::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_fs::symlink_dir;
    use tempfile::tempdir;

    #[test]
    fn materialize_virtual_store_package_preserves_workspace_links() {
        let dir = tempdir().expect("tempdir");
        let workspace_dep = dir.path().join("packages/project-1");
        let source_package_dir = dir.path().join(".pnpm/project-2@file+pkg/node_modules/project-2");
        let source_container_dir =
            source_package_dir.parent().expect("source container").to_path_buf();
        let destination_package_dir = dir.path().join("project/node_modules/project-2");

        fs::create_dir_all(&workspace_dep).expect("create workspace dependency");
        fs::create_dir_all(&source_package_dir).expect("create source package");
        fs::write(
            workspace_dep.join("package.json"),
            "{\"name\":\"project-1\",\"version\":\"1.0.0\"}",
        )
        .expect("write workspace dep manifest");
        fs::write(
            source_package_dir.join("package.json"),
            "{\"name\":\"project-2\",\"version\":\"1.0.0\"}",
        )
        .expect("write source package manifest");
        symlink_dir(&workspace_dep, &source_container_dir.join("project-1"))
            .expect("create workspace link");

        materialize_virtual_store_package(
            PackageImportMethod::Copy,
            &source_package_dir,
            &destination_package_dir,
        )
        .expect("materialize package");

        let destination_dep = destination_package_dir.join("node_modules/project-1");
        assert!(destination_dep.exists());
        assert!(is_symlink_or_junction(&destination_dep).expect("check destination link"));
    }

    #[test]
    fn materialize_virtual_store_package_recurses_into_virtual_store_links() {
        let dir = tempdir().expect("tempdir");
        let virtual_store_dir = dir.path().join(".pnpm");
        let nested_pkg = virtual_store_dir.join("project-1@file+pkg/node_modules/project-1");
        let nested_container_dir = nested_pkg.parent().expect("nested container").to_path_buf();
        let source_package_dir =
            virtual_store_dir.join("project-2@file+pkg/node_modules/project-2");
        let source_container_dir =
            source_package_dir.parent().expect("source container").to_path_buf();
        let destination_package_dir = dir.path().join("project/node_modules/project-2");

        fs::create_dir_all(&nested_pkg).expect("create nested package");
        fs::create_dir_all(nested_container_dir.join("is-number"))
            .expect("create transitive dependency");
        fs::create_dir_all(&source_package_dir).expect("create source package");
        fs::write(
            nested_pkg.join("package.json"),
            "{\"name\":\"project-1\",\"version\":\"1.0.0\"}",
        )
        .expect("write nested package manifest");
        fs::write(
            nested_container_dir.join("is-number/package.json"),
            "{\"name\":\"is-number\",\"version\":\"7.0.0\"}",
        )
        .expect("write transitive package manifest");
        fs::write(
            source_package_dir.join("package.json"),
            "{\"name\":\"project-2\",\"version\":\"1.0.0\"}",
        )
        .expect("write source package manifest");
        symlink_dir(&nested_pkg, &source_container_dir.join("project-1"))
            .expect("create nested virtual store link");

        materialize_virtual_store_package(
            PackageImportMethod::Copy,
            &source_package_dir,
            &destination_package_dir,
        )
        .expect("materialize package");

        let destination_dep = destination_package_dir.join("node_modules/project-1");
        assert!(destination_dep.exists());
        assert!(!is_symlink_or_junction(&destination_dep).expect("check destination metadata"));
        assert!(destination_dep.join("node_modules/is-number").exists());
    }
}
