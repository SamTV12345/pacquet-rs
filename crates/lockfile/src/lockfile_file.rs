use crate::{
    ComVer, DependencyPath, Lockfile, LockfilePeerDependencyMetaValue, LockfileResolution,
    LockfileSettings, LockfileVersion, MultiProjectSnapshot, PackageSnapshot,
    PackageSnapshotDependency, PkgName, ProjectSnapshot, ResolvedDependencyMap,
    RootProjectSnapshot,
};
use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Display, Error)]
pub(crate) enum LockfileFileError {
    #[display("Failed to parse lockfile header: {_0}")]
    ParseHeader(#[error(source)] serde_yaml::Error),

    #[display("Failed to parse lockfile as v6 format: {_0}")]
    ParseV6(#[error(source)] serde_yaml::Error),

    #[display("Failed to parse lockfile as v9 format: {_0}")]
    ParseV9(#[error(source)] serde_yaml::Error),

    #[display("Unsupported lockfileVersion: {_0}. Expected major 6 or 9")]
    UnsupportedVersion(#[error(not(source))] ComVer),

    #[display(
        "Missing package info for snapshot `{snapshot}`. Expected `packages` entry `{package_id}`"
    )]
    MissingPackageInfo { snapshot: String, package_id: String },

    #[display(
        "Importer `{importer}` has conflicting specifiers for `{dependency}`: `{existing}` vs `{received}`"
    )]
    ConflictingSpecifier {
        importer: String,
        dependency: String,
        existing: String,
        received: String,
    },

    #[display("Invalid v9 snapshot key: `{_0}`")]
    InvalidV9SnapshotKey(#[error(not(source))] String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfileHeader {
    lockfile_version: ComVer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfileV6File {
    lockfile_version: LockfileVersion<6>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<LockfileSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    never_built_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overrides: Option<HashMap<String, String>>,
    #[serde(flatten)]
    project_snapshot: RootProjectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfileV9File {
    lockfile_version: LockfileVersion<9>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<LockfileSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    catalogs: Option<serde_yaml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ignored_optional_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overrides: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_extensions_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patched_dependencies: Option<HashMap<String, serde_yaml::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pnpmfile_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    importers: Option<HashMap<String, LockfileFileProjectSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    packages: Option<HashMap<String, LockfilePackageInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshots: Option<HashMap<String, LockfilePackageSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfileFileProjectSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional_dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dev_dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies_meta: Option<serde_yaml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publish_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfilePackageInfo {
    resolution: LockfileResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engines: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    libc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_bin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepare: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_build: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundled_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMetaValue>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockfilePackageSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<HashMap<PkgName, PackageSnapshotDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transitive_peer_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dev: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional: Option<bool>,
}

pub(crate) fn parse_lockfile_content(content: &str) -> Result<Lockfile, LockfileFileError> {
    let header: LockfileHeader =
        serde_yaml::from_str(content).map_err(LockfileFileError::ParseHeader)?;
    match header.lockfile_version.major {
        6 => {
            let v6: LockfileV6File =
                serde_yaml::from_str(content).map_err(LockfileFileError::ParseV6)?;
            Ok(Lockfile {
                lockfile_version: v6.lockfile_version.into(),
                settings: v6.settings,
                never_built_dependencies: v6.never_built_dependencies,
                ignored_optional_dependencies: None,
                overrides: v6.overrides,
                package_extensions_checksum: None,
                patched_dependencies: None,
                pnpmfile_checksum: None,
                catalogs: None,
                time: None,
                project_snapshot: v6.project_snapshot,
                packages: v6.packages,
            })
        }
        9 => {
            let v9: LockfileV9File =
                serde_yaml::from_str(content).map_err(LockfileFileError::ParseV9)?;
            convert_v9_to_lockfile(v9)
        }
        _ => Err(LockfileFileError::UnsupportedVersion(header.lockfile_version)),
    }
}

pub(crate) fn render_lockfile_content(lockfile: &Lockfile) -> Result<String, serde_yaml::Error> {
    let lockfile_file = convert_lockfile_to_v9(lockfile);
    serde_yaml::to_string(&lockfile_file)
}

fn convert_v9_to_lockfile(v9: LockfileV9File) -> Result<Lockfile, LockfileFileError> {
    let importers = v9.importers.unwrap_or_default();
    let project_snapshot = file_importers_to_root_snapshot(importers)?;
    let package_infos = v9.packages.unwrap_or_default();
    let package_snapshots = v9.snapshots.unwrap_or_default();

    let mut packages = HashMap::<DependencyPath, PackageSnapshot>::new();
    for (snapshot_key, pkg_snapshot) in package_snapshots {
        let dep_path = dependency_path_from_v9_key(&snapshot_key)?;
        let package_id = v9_key_from_dependency_path(&dep_path);
        let package_info = package_infos.get(&package_id).cloned().ok_or_else(|| {
            LockfileFileError::MissingPackageInfo { snapshot: snapshot_key.clone(), package_id }
        })?;
        packages.insert(dep_path, merge_package_info_and_snapshot(package_info, pkg_snapshot));
    }

    Ok(Lockfile {
        lockfile_version: v9.lockfile_version.into(),
        settings: v9.settings,
        never_built_dependencies: None,
        ignored_optional_dependencies: v9.ignored_optional_dependencies,
        overrides: v9.overrides,
        package_extensions_checksum: v9.package_extensions_checksum,
        patched_dependencies: v9.patched_dependencies,
        pnpmfile_checksum: v9.pnpmfile_checksum,
        catalogs: v9.catalogs,
        time: v9.time,
        project_snapshot,
        packages: (!packages.is_empty()).then_some(packages),
    })
}

fn convert_lockfile_to_v9(lockfile: &Lockfile) -> LockfileV9File {
    let mut package_infos = HashMap::<String, LockfilePackageInfo>::new();
    let mut package_snapshots = HashMap::<String, LockfilePackageSnapshot>::new();

    for (dep_path, package_snapshot) in lockfile.packages.as_ref().into_iter().flatten() {
        let (package_info, snapshot_part) = split_package_snapshot(package_snapshot);
        let package_id = v9_key_from_dependency_path(dep_path);
        package_infos.entry(package_id).or_insert(package_info);
        package_snapshots.insert(v9_key_from_dependency_path(dep_path), snapshot_part);
    }

    let version = if lockfile.lockfile_version.major == 9 {
        lockfile.lockfile_version
    } else {
        ComVer::new(9, 0)
    };
    let lockfile_version =
        LockfileVersion::<9>::try_from(version).expect("v9 file always uses a 9.x lockfileVersion");

    LockfileV9File {
        lockfile_version,
        settings: lockfile.settings.clone(),
        catalogs: lockfile.catalogs.clone(),
        ignored_optional_dependencies: lockfile.ignored_optional_dependencies.clone(),
        overrides: lockfile.overrides.clone(),
        package_extensions_checksum: lockfile.package_extensions_checksum.clone(),
        patched_dependencies: lockfile.patched_dependencies.clone(),
        pnpmfile_checksum: lockfile.pnpmfile_checksum.clone(),
        time: lockfile.time.clone(),
        importers: Some(root_snapshot_to_file_importers(&lockfile.project_snapshot)),
        packages: (!package_infos.is_empty()).then_some(package_infos),
        snapshots: (!package_snapshots.is_empty()).then_some(package_snapshots),
    }
}

fn split_package_snapshot(
    package_snapshot: &PackageSnapshot,
) -> (LockfilePackageInfo, LockfilePackageSnapshot) {
    let info = LockfilePackageInfo {
        resolution: package_snapshot.resolution.clone(),
        id: package_snapshot.id.clone(),
        name: package_snapshot.name.clone(),
        version: package_snapshot.version.clone(),
        engines: package_snapshot.engines.clone(),
        cpu: package_snapshot.cpu.clone(),
        os: package_snapshot.os.clone(),
        libc: package_snapshot.libc.clone(),
        deprecated: package_snapshot.deprecated.clone(),
        has_bin: package_snapshot.has_bin,
        prepare: package_snapshot.prepare,
        requires_build: package_snapshot.requires_build,
        bundled_dependencies: package_snapshot.bundled_dependencies.clone(),
        peer_dependencies: package_snapshot.peer_dependencies.clone(),
        peer_dependencies_meta: package_snapshot.peer_dependencies_meta.clone(),
    };
    let snapshot = LockfilePackageSnapshot {
        dependencies: package_snapshot.dependencies.clone(),
        optional_dependencies: package_snapshot.optional_dependencies.clone(),
        transitive_peer_dependencies: package_snapshot.transitive_peer_dependencies.clone(),
        dev: package_snapshot.dev,
        optional: package_snapshot.optional,
    };
    (info, snapshot)
}

fn merge_package_info_and_snapshot(
    package_info: LockfilePackageInfo,
    package_snapshot: LockfilePackageSnapshot,
) -> PackageSnapshot {
    PackageSnapshot {
        resolution: package_info.resolution,
        id: package_info.id,
        name: package_info.name,
        version: package_info.version,
        engines: package_info.engines,
        cpu: package_info.cpu,
        os: package_info.os,
        libc: package_info.libc,
        deprecated: package_info.deprecated,
        has_bin: package_info.has_bin,
        prepare: package_info.prepare,
        requires_build: package_info.requires_build,
        bundled_dependencies: package_info.bundled_dependencies,
        peer_dependencies: package_info.peer_dependencies,
        peer_dependencies_meta: package_info.peer_dependencies_meta,
        dependencies: package_snapshot.dependencies,
        optional_dependencies: package_snapshot.optional_dependencies,
        transitive_peer_dependencies: package_snapshot.transitive_peer_dependencies,
        dev: package_snapshot.dev,
        optional: package_snapshot.optional,
    }
}

fn v9_key_from_dependency_path(dep_path: &DependencyPath) -> String {
    let package_specifier = dep_path.package_specifier.to_string();
    match dep_path.custom_registry.as_deref() {
        Some(custom_registry) => format!("{custom_registry}/{package_specifier}"),
        None => package_specifier,
    }
}

fn dependency_path_from_v9_key(value: &str) -> Result<DependencyPath, LockfileFileError> {
    if let Ok(package_specifier) = value.parse() {
        return Ok(DependencyPath { custom_registry: None, package_specifier });
    }
    value
        .parse::<DependencyPath>()
        .map_err(|_| LockfileFileError::InvalidV9SnapshotKey(value.to_string()))
}

fn root_snapshot_to_file_importers(
    snapshot: &RootProjectSnapshot,
) -> HashMap<String, LockfileFileProjectSnapshot> {
    match snapshot {
        RootProjectSnapshot::Single(project_snapshot) => {
            HashMap::from([(".".to_string(), project_snapshot_to_file(project_snapshot))])
        }
        RootProjectSnapshot::Multi(multi) => multi
            .importers
            .iter()
            .map(|(importer_id, project_snapshot)| {
                (importer_id.clone(), project_snapshot_to_file(project_snapshot))
            })
            .collect(),
    }
}

fn file_importers_to_root_snapshot(
    importers: HashMap<String, LockfileFileProjectSnapshot>,
) -> Result<RootProjectSnapshot, LockfileFileError> {
    if importers.len() == 1
        && let Some(single) = importers.get(".")
    {
        return file_project_to_internal(".", single.clone()).map(RootProjectSnapshot::Single);
    }

    let mut internal_importers = HashMap::new();
    for (importer_id, importer_snapshot) in importers {
        internal_importers.insert(
            importer_id.clone(),
            file_project_to_internal(&importer_id, importer_snapshot)?,
        );
    }
    Ok(RootProjectSnapshot::Multi(MultiProjectSnapshot { importers: internal_importers }))
}

fn project_snapshot_to_file(snapshot: &ProjectSnapshot) -> LockfileFileProjectSnapshot {
    LockfileFileProjectSnapshot {
        dependencies: snapshot.dependencies.clone(),
        optional_dependencies: snapshot.optional_dependencies.clone(),
        dev_dependencies: snapshot.dev_dependencies.clone(),
        dependencies_meta: snapshot.dependencies_meta.clone(),
        publish_directory: snapshot.publish_directory.clone(),
    }
}

fn file_project_to_internal(
    importer_id: &str,
    snapshot: LockfileFileProjectSnapshot,
) -> Result<ProjectSnapshot, LockfileFileError> {
    let mut specifiers = HashMap::<String, String>::new();

    for deps in [
        snapshot.dependencies.as_ref(),
        snapshot.optional_dependencies.as_ref(),
        snapshot.dev_dependencies.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        collect_specifiers(importer_id, &mut specifiers, deps)?;
    }

    Ok(ProjectSnapshot {
        specifiers: (!specifiers.is_empty()).then_some(specifiers),
        dependencies: snapshot.dependencies,
        optional_dependencies: snapshot.optional_dependencies,
        dev_dependencies: snapshot.dev_dependencies,
        dependencies_meta: snapshot.dependencies_meta,
        publish_directory: snapshot.publish_directory,
    })
}

fn collect_specifiers(
    importer_id: &str,
    specifiers: &mut HashMap<String, String>,
    deps: &ResolvedDependencyMap,
) -> Result<(), LockfileFileError> {
    for (name, resolved) in deps {
        let dependency = name.to_string();
        if let Some(existing) = specifiers.get(&dependency) {
            if existing != &resolved.specifier {
                return Err(LockfileFileError::ConflictingSpecifier {
                    importer: importer_id.to_string(),
                    dependency,
                    existing: existing.clone(),
                    received: resolved.specifier.clone(),
                });
            }
        } else {
            specifiers.insert(dependency, resolved.specifier.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use text_block_macros::text_block;

    #[test]
    fn load_v6_and_save_as_v9() {
        let v6 = text_block! {
            "lockfileVersion: '6.0'"
            "settings:"
            "  autoInstallPeers: true"
            "  excludeLinksFromLockfile: false"
            "dependencies:"
            "  foo:"
            "    specifier: ^1.0.0"
            "    version: 1.0.0"
            "packages:"
            "  /foo@1.0.0:"
            "    resolution:"
            "      integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
        };

        let lockfile = parse_lockfile_content(v6).expect("parse v6 lockfile");
        assert_eq!(lockfile.lockfile_version.major, 6);

        let rendered = render_lockfile_content(&lockfile).expect("render as v9 lockfile");
        assert!(rendered.contains("lockfileVersion"));
        assert!(rendered.contains("importers:"));
        assert!(rendered.contains("snapshots:"));

        let roundtrip = parse_lockfile_content(&rendered).expect("parse rendered lockfile");
        assert_eq!(roundtrip.lockfile_version.major, 9);
    }

    #[test]
    fn parse_v9_packages_and_snapshots() {
        let v9 = text_block! {
            "lockfileVersion: '9.0'"
            "importers:"
            "  .:"
            "    dependencies:"
            "      '@pnpm.e2e/hello-world-js-bin-parent':"
            "        specifier: 1.0.0"
            "        version: 1.0.0"
            "packages:"
            "  '@pnpm.e2e/hello-world-js-bin-parent@1.0.0':"
            "    resolution:"
            "      integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
            "snapshots:"
            "  '@pnpm.e2e/hello-world-js-bin-parent@1.0.0': {}"
        };

        let lockfile = parse_lockfile_content(v9).expect("parse v9 lockfile");
        assert_eq!(lockfile.lockfile_version.major, 9);

        let packages = lockfile.packages.expect("combined package map");
        assert!(packages.keys().any(|key| {
            key.to_string() == "/@pnpm.e2e/hello-world-js-bin-parent@1.0.0"
        }));

        let RootProjectSnapshot::Single(project) = lockfile.project_snapshot else {
            panic!("expected single importer after conversion");
        };
        let deps = project.dependencies.expect("dependencies map");
        assert_eq!(
            deps.get(&"@pnpm.e2e/hello-world-js-bin-parent".parse().unwrap())
                .unwrap()
                .version
                .to_string(),
            "1.0.0".to_string()
        );
    }
}
