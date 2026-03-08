use crate::symlink_package;
use pacquet_lockfile::{PkgName, PkgNameVerPeer, ProjectSnapshot, ResolvedDependencyVersion};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use rayon::prelude::*;

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
    pub dependency_groups: DependencyGroupList,
}

impl<'a, DependencyGroupList> SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub fn run(self) {
        let SymlinkDirectDependencies { config, project_snapshot, dependency_groups } = self;

        project_snapshot
            .dependencies_by_groups(dependency_groups)
            .collect::<Vec<_>>()
            .par_iter()
            .for_each(|(name, spec)| {
                let name_str = name.to_string();
                let symlink_target = match &spec.version {
                    ResolvedDependencyVersion::PkgVerPeer(ver_peer) => {
                        let virtual_store_name =
                            PkgNameVerPeer::new(PkgName::clone(name), ver_peer.clone())
                                .to_virtual_store_name();
                        config
                            .virtual_store_dir
                            .join(virtual_store_name)
                            .join("node_modules")
                            .join(&name_str)
                    }
                    ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
                        let virtual_store_name = name_ver_peer.to_virtual_store_name();
                        let target_name = name_ver_peer.name.to_string();
                        config
                            .virtual_store_dir
                            .join(virtual_store_name)
                            .join("node_modules")
                            .join(target_name)
                    }
                    ResolvedDependencyVersion::Link(link) => {
                        let relative = link.strip_prefix("link:").unwrap_or(link);
                        config
                            .modules_dir
                            .parent()
                            .unwrap_or(config.modules_dir.as_path())
                            .join(relative)
                    }
                };
                let dependency_path = config.modules_dir.join(&name_str);
                if dependency_path.exists() {
                    return;
                }

                symlink_package(&symlink_target, &dependency_path).unwrap_or_else(|error| {
                    panic!("direct dependency symlink should succeed ({name_str}): {error}")
                });
            });
    }
}
