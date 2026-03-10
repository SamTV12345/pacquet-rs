use crate::{SymlinkPackageError, link_package, should_materialize_root_links, symlink_package};
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PkgName, PkgNameVerPeer, ProjectSnapshot,
    ResolvedDependencyVersion,
};
use pacquet_npmrc::Npmrc;
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
                                materialize_virtual_store_package(&symlink_target, &dependency_path)
                            } else {
                                link_package(false, &symlink_target, &dependency_path)
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
    (config.modules_dir.parent().unwrap_or(config.modules_dir.as_path()).join(relative), false)
}

fn materialize_virtual_store_package(
    source_package_dir: &Path,
    destination_package_dir: &Path,
) -> Result<(), SymlinkPackageError> {
    fn inner(
        source_package_dir: &Path,
        destination_package_dir: &Path,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<(), SymlinkPackageError> {
        let source_package_dir = fs::canonicalize(source_package_dir).map_err(|error| {
            SymlinkPackageError::CanonicalizePath { path: source_package_dir.to_path_buf(), error }
        })?;
        if !seen.insert(source_package_dir.clone()) {
            return Ok(());
        }

        link_package(false, &source_package_dir, destination_package_dir)?;

        let Some(virtual_node_modules_dir) = source_package_dir.ancestors().find(|ancestor| {
            ancestor.file_name().and_then(|name| name.to_str()) == Some("node_modules")
        }) else {
            return Ok(());
        };
        let entries = match fs::read_dir(virtual_node_modules_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            if file_name_str == ".bin" {
                continue;
            }

            if file_name_str.starts_with('@') {
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
                    inner(&scoped_source_dir, &destination_dependency_dir, seen)?;
                }
                continue;
            }

            let source_dependency_dir = entry.path();
            if source_dependency_dir == source_package_dir {
                continue;
            }
            let destination_dependency_dir =
                destination_package_dir.join("node_modules").join(&file_name);
            inner(&source_dependency_dir, &destination_dependency_dir, seen)?;
        }

        Ok(())
    }

    inner(source_package_dir, destination_package_dir, &mut HashSet::new())
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
