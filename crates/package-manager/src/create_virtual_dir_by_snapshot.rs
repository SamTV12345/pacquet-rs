use crate::{CreateCasFilesError, create_cas_files};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer, PkgVerPeer,
};
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

/// This subroutine installs the files from [`cas_paths`](Self::cas_paths) then creates the symlink layout.
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    pub virtual_store_dir: &'a Path,
    pub cas_paths: &'a HashMap<String, PathBuf>,
    pub import_method: PackageImportMethod,
    pub dependency_path: &'a DependencyPath,
    pub package_snapshot: &'a PackageSnapshot,
}

/// Error type of [`CreateVirtualDirBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[display("Failed to recursively create node_modules directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::create_node_modules_dir))]
    CreateNodeModulesDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot {
            virtual_store_dir,
            cas_paths,
            import_method,
            dependency_path,
            package_snapshot: _,
        } = self;

        // node_modules/.pacquet/pkg-name@x.y.z/node_modules
        let virtual_node_modules_dir = virtual_store_dir
            .join(dependency_path.package_specifier.to_virtual_store_name())
            .join("node_modules");
        fs::create_dir_all(&virtual_node_modules_dir).map_err(|error| {
            CreateVirtualDirError::CreateNodeModulesDir {
                dir: virtual_node_modules_dir.to_path_buf(),
                error,
            }
        })?;

        // 1. Install the files from `cas_paths`
        let save_path =
            virtual_node_modules_dir.join(dependency_path.package_specifier.name.to_string());
        create_cas_files(import_method, &save_path, cas_paths)
            .map_err(CreateVirtualDirError::CreateCasFiles)?;

        Ok(())
    }
}

pub(crate) fn package_dependency_map(
    package_snapshot: &PackageSnapshot,
) -> HashMap<PkgName, PackageSnapshotDependency> {
    let mut all_dependencies = package_snapshot.dependencies.clone().unwrap_or_default();
    if let Some(optional_dependencies) = &package_snapshot.optional_dependencies {
        for (name, reference) in optional_dependencies {
            let Ok(name) = name.parse::<PkgName>() else {
                continue;
            };
            let Some(spec) = parse_package_snapshot_dependency(reference) else {
                continue;
            };
            all_dependencies.entry(name).or_insert(spec);
        }
    }
    all_dependencies
}

fn parse_package_snapshot_dependency(value: &str) -> Option<PackageSnapshotDependency> {
    value
        .parse::<PkgVerPeer>()
        .ok()
        .map(PackageSnapshotDependency::PkgVerPeer)
        .or_else(|| {
            value.parse::<DependencyPath>().ok().map(PackageSnapshotDependency::DependencyPath)
        })
        .or_else(|| {
            value.parse::<PkgNameVerPeer>().ok().map(PackageSnapshotDependency::PkgNameVerPeer)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn package_dependency_map_includes_optional_dependencies() {
        let snapshot_yaml = r#"
resolution:
  integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==
optionalDependencies:
  '@esbuild/win32-x64': 0.27.1
"#;
        let snapshot: PackageSnapshot =
            serde_yaml::from_str(snapshot_yaml).expect("parse snapshot");
        let dependencies = package_dependency_map(&snapshot);

        assert_eq!(dependencies.len(), 1);
        assert!(dependencies.contains_key(&"@esbuild/win32-x64".parse::<PkgName>().unwrap()));
    }
}
