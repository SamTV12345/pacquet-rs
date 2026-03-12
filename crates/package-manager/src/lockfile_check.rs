use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pacquet_lockfile::{Lockfile, LockfileSettings, ProjectSnapshot, ResolvedDependencyMap};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

type CatalogConfig = HashMap<String, HashMap<String, String>>;

pub(crate) struct RuntimeLockfileConfig {
    pub settings: LockfileSettings,
    pub catalogs: CatalogConfig,
    pub overrides: Option<HashMap<String, String>>,
    pub package_extensions_checksum: Option<String>,
    pub pnpmfile_checksum: Option<String>,
}

pub(crate) fn collect_runtime_lockfile_config(
    config: &Npmrc,
    manifest: &PackageManifest,
    lockfile_dir: &Path,
    pnpmfile: Option<&Path>,
    ignore_pnpmfile: bool,
) -> RuntimeLockfileConfig {
    let workspace_manifest = read_workspace_manifest(lockfile_dir);
    let root_package = read_root_package_json(lockfile_dir, manifest);

    let mut overrides = HashMap::<String, String>::new();
    overrides.extend(extract_overrides_from_package_json(root_package.as_ref()));
    overrides.extend(extract_overrides_from_workspace(workspace_manifest.as_ref()));
    let overrides = (!overrides.is_empty()).then_some(overrides);

    let package_extensions = merge_json_objects(
        extract_package_extensions_from_package_json(root_package.as_ref()),
        extract_package_extensions_from_workspace(workspace_manifest.as_ref()),
    );

    RuntimeLockfileConfig {
        settings: LockfileSettings {
            auto_install_peers: Some(config.auto_install_peers),
            exclude_links_from_lockfile: Some(config.exclude_links_from_lockfile),
            peers_suffix_max_length: Some(config.peers_suffix_max_length),
            inject_workspace_packages: Some(config.inject_workspace_packages),
        },
        catalogs: extract_catalogs(workspace_manifest.as_ref()),
        overrides,
        package_extensions_checksum: hash_object_nullable_with_prefix(package_extensions.as_ref()),
        pnpmfile_checksum: (!ignore_pnpmfile)
            .then(|| calculate_pnpmfile_checksum(lockfile_dir, pnpmfile))
            .flatten(),
    }
}

pub(crate) fn get_outdated_lockfile_setting(
    lockfile: &Lockfile,
    runtime: &RuntimeLockfileConfig,
) -> Option<&'static str> {
    if !all_catalogs_are_up_to_date(&runtime.catalogs, lockfile.catalogs.as_ref()) {
        return Some("catalogs");
    }
    if lockfile.overrides.clone().unwrap_or_default()
        != runtime.overrides.clone().unwrap_or_default()
    {
        return Some("overrides");
    }
    if lockfile.package_extensions_checksum != runtime.package_extensions_checksum {
        return Some("packageExtensionsChecksum");
    }

    if lockfile
        .settings
        .as_ref()
        .and_then(|settings| settings.auto_install_peers)
        .is_some_and(|value| value != runtime.settings.auto_install_peers.unwrap_or_default())
    {
        return Some("settings.autoInstallPeers");
    }
    if lockfile
        .settings
        .as_ref()
        .and_then(|settings| settings.exclude_links_from_lockfile)
        .is_some_and(|value| value != runtime.settings.exclude_links_from_lockfile.unwrap_or(false))
    {
        return Some("settings.excludeLinksFromLockfile");
    }

    let runtime_peers_suffix = runtime.settings.peers_suffix_max_length.unwrap_or(1000);
    match lockfile.settings.as_ref().and_then(|settings| settings.peers_suffix_max_length) {
        Some(value) if value != runtime_peers_suffix => {
            return Some("settings.peersSuffixMaxLength");
        }
        None if runtime_peers_suffix != 1000 => return Some("settings.peersSuffixMaxLength"),
        _ => {}
    }

    if lockfile.pnpmfile_checksum != runtime.pnpmfile_checksum {
        return Some("pnpmfileChecksum");
    }

    let lockfile_inject_workspace_packages = lockfile
        .settings
        .as_ref()
        .and_then(|settings| settings.inject_workspace_packages)
        .unwrap_or(false);
    if lockfile_inject_workspace_packages
        != runtime.settings.inject_workspace_packages.unwrap_or(false)
    {
        return Some("settings.injectWorkspacePackages");
    }

    None
}

pub(crate) fn satisfies_package_manifest(
    project_snapshot: &ProjectSnapshot,
    manifest: &PackageManifest,
    auto_install_peers: bool,
    exclude_links_from_lockfile: bool,
    strict_specifier_match: bool,
) -> Result<(), String> {
    let mut manifest = extract_manifest(manifest);
    let importer_dep_names = project_snapshot
        .dependencies
        .iter()
        .chain(project_snapshot.optional_dependencies.iter())
        .chain(project_snapshot.dev_dependencies.iter())
        .flat_map(|map| map.keys().map(ToString::to_string))
        .chain(project_snapshot.specifiers.iter().flat_map(|map| map.keys().cloned()))
        .collect::<std::collections::HashSet<_>>();

    let mut existing_deps = merge_maps(&[
        &manifest.dev_dependencies,
        &manifest.dependencies,
        &manifest.optional_dependencies,
    ]);

    if auto_install_peers {
        let mut dependencies_with_auto_peers = HashMap::<String, String>::new();
        for (name, specifier) in &manifest.peer_dependencies {
            if !existing_deps.contains_key(name) && importer_dep_names.contains(name) {
                dependencies_with_auto_peers.insert(name.clone(), specifier.clone());
            }
        }
        dependencies_with_auto_peers.extend(manifest.dependencies.clone());
        manifest.dependencies = dependencies_with_auto_peers;

        let mut deps_with_peers = manifest
            .peer_dependencies
            .iter()
            .filter(|(name, _)| importer_dep_names.contains(*name))
            .map(|(name, specifier)| (name.clone(), specifier.clone()))
            .collect::<HashMap<_, _>>();
        deps_with_peers.extend(existing_deps);
        existing_deps = deps_with_peers;
    }

    let mut specifiers = project_snapshot.specifiers.clone().unwrap_or_default();
    if exclude_links_from_lockfile {
        existing_deps.retain(|_, specifier| !specifier.starts_with("link:"));
        specifiers.retain(|_, specifier| !specifier.starts_with("link:"));
    }

    if strict_specifier_match && specifiers != existing_deps {
        return Err("specifiers in the lockfile don't match specifiers in package.json".to_string());
    }

    if project_snapshot.publish_directory != manifest.publish_directory {
        return Err(
            "\"publishDirectory\" in the lockfile doesn't match \"publishConfig.directory\" in package.json"
                .to_string(),
        );
    }

    let importer_dependencies_meta = project_snapshot
        .dependencies_meta
        .as_ref()
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or_else(empty_json_object);
    if importer_dependencies_meta != manifest.dependencies_meta {
        return Err("dependenciesMeta in the lockfile doesn't match package.json".to_string());
    }

    let mut dependencies = manifest.dependencies.clone();
    let mut optional_dependencies = manifest.optional_dependencies.clone();
    let mut dev_dependencies = manifest.dev_dependencies.clone();

    if exclude_links_from_lockfile {
        dependencies.retain(|_, specifier| !specifier.starts_with("link:"));
        optional_dependencies.retain(|_, specifier| !specifier.starts_with("link:"));
        dev_dependencies.retain(|_, specifier| !specifier.starts_with("link:"));
    }

    for dependency_field in ["dependencies", "optionalDependencies", "devDependencies"] {
        let importer_deps = match dependency_field {
            "dependencies" => resolved_map_to_versions(project_snapshot.dependencies.as_ref()),
            "optionalDependencies" => {
                resolved_map_to_versions(project_snapshot.optional_dependencies.as_ref())
            }
            "devDependencies" => {
                resolved_map_to_versions(project_snapshot.dev_dependencies.as_ref())
            }
            _ => unreachable!(),
        };

        let package_deps = match dependency_field {
            "dependencies" => &dependencies,
            "optionalDependencies" => &optional_dependencies,
            "devDependencies" => &dev_dependencies,
            _ => unreachable!(),
        };

        let package_dep_names = match dependency_field {
            "optionalDependencies" => package_deps.keys().cloned().collect::<Vec<_>>(),
            "devDependencies" => package_deps
                .keys()
                .filter(|name| {
                    !optional_dependencies.contains_key(*name) && !dependencies.contains_key(*name)
                })
                .cloned()
                .collect::<Vec<_>>(),
            "dependencies" => package_deps
                .keys()
                .filter(|name| !optional_dependencies.contains_key(*name))
                .cloned()
                .collect::<Vec<_>>(),
            _ => unreachable!(),
        };

        if package_dep_names.len() != importer_deps.len()
            && package_dep_names.len() != count_non_linked_deps(&importer_deps)
        {
            return Err(format!(
                "\"{dependency_field}\" in the lockfile doesn't match the same field in package.json"
            ));
        }

        for dep_name in package_dep_names {
            let manifest_specifier =
                package_deps.get(dep_name.as_str()).expect("dependency exists");
            let specifier_mismatch = specifiers.get(dep_name.as_str()) != Some(manifest_specifier);
            if !importer_deps.contains_key(dep_name.as_str())
                || (strict_specifier_match && specifier_mismatch)
            {
                return Err(format!(
                    "importer {dependency_field}.{dep_name} specifier doesn't match package manifest"
                ));
            }

            let Some(lockfile_specifier) = specifiers.get(dep_name.as_str()) else {
                continue;
            };
            let Ok(range) = lockfile_specifier.parse::<node_semver::Range>() else {
                continue;
            };
            let Some(resolved_version) = importer_deps
                .get(dep_name.as_str())
                .and_then(|version| parse_semver_from_dep(version))
            else {
                continue;
            };
            if !resolved_version.satisfies(&range) {
                return Err(format!(
                    "dependency \"{dep_name}\" is resolved to \"{resolved_version}\" which doesn't satisfy \"{lockfile_specifier}\""
                ));
            }
        }
    }

    Ok(())
}

fn parse_semver_from_dep(value: &str) -> Option<node_semver::Version> {
    let base = value.split('(').next().unwrap_or(value);
    base.parse::<node_semver::Version>().ok()
}

fn count_non_linked_deps(lockfile_deps: &HashMap<String, String>) -> usize {
    lockfile_deps
        .values()
        .filter(|version| !version.contains("link:") && !version.contains("file:"))
        .count()
}

fn resolved_map_to_versions(map: Option<&ResolvedDependencyMap>) -> HashMap<String, String> {
    map.into_iter()
        .flatten()
        .map(|(name, spec)| (name.to_string(), spec.version.to_string()))
        .collect()
}

fn merge_maps(maps: &[&HashMap<String, String>]) -> HashMap<String, String> {
    let mut merged = HashMap::<String, String>::new();
    for map in maps {
        merged.extend((*map).clone());
    }
    merged
}

fn all_catalogs_are_up_to_date(
    catalogs_config: &CatalogConfig,
    snapshot: Option<&YamlValue>,
) -> bool {
    let Some(snapshot) = snapshot else {
        return true;
    };
    let Some(catalogs) = snapshot.as_mapping() else {
        return false;
    };

    for (catalog_name, catalog) in catalogs {
        let Some(catalog_name) = catalog_name.as_str() else {
            return false;
        };
        let Some(entries) = catalog.as_mapping() else {
            return false;
        };

        for (alias, entry) in entries {
            let Some(alias) = alias.as_str() else {
                return false;
            };
            let Some(entry) = entry.as_mapping() else {
                return false;
            };
            let Some(specifier) = yaml_mapping_string(entry, "specifier") else {
                return false;
            };
            let expected = catalogs_config.get(catalog_name).and_then(|catalog| catalog.get(alias));
            if expected != Some(&specifier) {
                return false;
            }
        }
    }

    true
}

struct ManifestSnapshot {
    dependencies: HashMap<String, String>,
    optional_dependencies: HashMap<String, String>,
    dev_dependencies: HashMap<String, String>,
    peer_dependencies: HashMap<String, String>,
    dependencies_meta: JsonValue,
    publish_directory: Option<String>,
}

fn extract_manifest(manifest: &PackageManifest) -> ManifestSnapshot {
    let value = manifest.value();
    let dependencies_meta =
        value.get("dependenciesMeta").cloned().unwrap_or_else(empty_json_object);
    let publish_directory = value
        .get("publishConfig")
        .and_then(|publish_config| publish_config.get("directory"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);

    ManifestSnapshot {
        dependencies: json_string_map(value.get("dependencies")),
        optional_dependencies: json_string_map(value.get("optionalDependencies")),
        dev_dependencies: json_string_map(value.get("devDependencies")),
        peer_dependencies: json_string_map(value.get("peerDependencies")),
        dependencies_meta,
        publish_directory,
    }
}

fn read_root_package_json(lockfile_dir: &Path, manifest: &PackageManifest) -> Option<JsonValue> {
    if manifest.path().parent().is_some_and(|dir| dir == lockfile_dir) {
        return Some(manifest.value().clone());
    }
    read_json_file(lockfile_dir.join("package.json"))
}

fn read_workspace_manifest(lockfile_dir: &Path) -> Option<YamlValue> {
    let path = lockfile_dir.join("pnpm-workspace.yaml");
    let content = fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

fn extract_catalogs(workspace_manifest: Option<&YamlValue>) -> CatalogConfig {
    let Some(workspace_manifest) = workspace_manifest else {
        return HashMap::new();
    };
    let Some(root) = workspace_manifest.as_mapping() else {
        return HashMap::new();
    };

    let mut catalogs =
        yaml_mapping_get(root, "catalogs").map(yaml_nested_string_map).unwrap_or_default();
    if let Some(default_catalog) = yaml_mapping_get(root, "catalog").map(json_string_map_from_yaml)
    {
        catalogs.entry("default".to_string()).or_default().extend(default_catalog);
    }

    catalogs
}

fn extract_overrides_from_workspace(
    workspace_manifest: Option<&YamlValue>,
) -> HashMap<String, String> {
    workspace_manifest
        .and_then(YamlValue::as_mapping)
        .and_then(|root| yaml_mapping_get(root, "overrides"))
        .map(json_string_map_from_yaml)
        .unwrap_or_default()
}

fn extract_overrides_from_package_json(
    package_json: Option<&JsonValue>,
) -> HashMap<String, String> {
    package_json
        .and_then(|json| json.get("pnpm"))
        .and_then(|pnpm| pnpm.get("overrides"))
        .map(|value| json_string_map(Some(value)))
        .unwrap_or_default()
}

fn extract_package_extensions_from_workspace(
    workspace_manifest: Option<&YamlValue>,
) -> Option<JsonValue> {
    workspace_manifest
        .and_then(YamlValue::as_mapping)
        .and_then(|root| yaml_mapping_get(root, "packageExtensions"))
        .and_then(|value| serde_json::to_value(value).ok())
}

fn extract_package_extensions_from_package_json(
    package_json: Option<&JsonValue>,
) -> Option<JsonValue> {
    package_json
        .and_then(|json| json.get("pnpm"))
        .and_then(|pnpm| pnpm.get("packageExtensions"))
        .cloned()
}

fn merge_json_objects(left: Option<JsonValue>, right: Option<JsonValue>) -> Option<JsonValue> {
    let mut merged = JsonMap::<String, JsonValue>::new();

    if let Some(JsonValue::Object(map)) = left {
        merged.extend(map);
    }
    if let Some(JsonValue::Object(map)) = right {
        merged.extend(map);
    }

    (!merged.is_empty()).then_some(JsonValue::Object(merged))
}

fn read_json_file(path: PathBuf) -> Option<JsonValue> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn json_string_map(value: Option<&JsonValue>) -> HashMap<String, String> {
    let Some(JsonValue::Object(map)) = value else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(name, value)| value.as_str().map(|value| (name.clone(), value.to_string())))
        .collect()
}

fn json_string_map_from_yaml(value: &YamlValue) -> HashMap<String, String> {
    let Some(map) = value.as_mapping() else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(name, value)| Some((name.as_str()?.to_string(), value.as_str()?.to_string())))
        .collect()
}

fn yaml_nested_string_map(value: &YamlValue) -> CatalogConfig {
    let Some(root) = value.as_mapping() else {
        return HashMap::new();
    };
    root.iter()
        .filter_map(|(catalog_name, entries)| {
            let catalog_name = catalog_name.as_str()?.to_string();
            let entries = entries.as_mapping()?;
            let entry_map = entries
                .iter()
                .filter_map(|(name, specifier)| {
                    Some((name.as_str()?.to_string(), specifier.as_str()?.to_string()))
                })
                .collect::<HashMap<_, _>>();
            Some((catalog_name, entry_map))
        })
        .collect()
}

fn yaml_mapping_get<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a YamlValue> {
    mapping.get(YamlValue::String(key.to_string()))
}

fn yaml_mapping_string(mapping: &YamlMapping, key: &str) -> Option<String> {
    yaml_mapping_get(mapping, key).and_then(YamlValue::as_str).map(ToString::to_string)
}

fn hash_object_nullable_with_prefix(object: Option<&JsonValue>) -> Option<String> {
    let Some(JsonValue::Object(map)) = object else {
        return None;
    };
    if map.is_empty() {
        return None;
    }
    let raw = object_hash_serialize_json(object.expect("checked above"));
    Some(format!("sha256-{}", BASE64.encode(Sha256::digest(raw.as_bytes()))))
}

fn calculate_pnpmfile_checksum(lockfile_dir: &Path, pnpmfile: Option<&Path>) -> Option<String> {
    let pnpmfile_path = crate::resolve_pnpmfile_path(lockfile_dir, pnpmfile);
    match crate::pnpmfile_exports_value(&pnpmfile_path).ok()? {
        None | Some(false) => return None,
        Some(true) => {}
    }
    let content = fs::read_to_string(pnpmfile_path).ok()?;
    let normalized = content.replace("\r\n", "\n");
    Some(format!("sha256-{}", BASE64.encode(Sha256::digest(normalized.as_bytes()))))
}

fn object_hash_serialize_json(value: &JsonValue) -> String {
    match value {
        JsonValue::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();

            let mut out = format!("object:{}:", keys.len());
            for key in keys {
                out.push_str(&serialize_string_token(&key));
                out.push(':');
                let entry = map.get(key.as_str()).expect("sorted key exists");
                out.push_str(&object_hash_serialize_json(entry));
                out.push(',');
            }
            out
        }
        JsonValue::Array(list) => {
            let mut out = format!("array:{}:", list.len());
            if list.len() <= 1 {
                for entry in list {
                    out.push_str(&object_hash_serialize_json(entry));
                }
                return out;
            }

            // object-hash with `unorderedArrays: true`:
            // write array header, serialize each entry, sort, then recurse
            // into ordered array serialization of serialized entries.
            let mut serialized_entries =
                list.iter().map(object_hash_serialize_json).collect::<Vec<_>>();
            serialized_entries.sort();
            out.push_str(&serialize_array_of_strings(&serialized_entries));
            out
        }
        JsonValue::String(text) => serialize_string_token(text),
        JsonValue::Number(number) => format!("number:{number}"),
        JsonValue::Bool(value) => format!("bool:{value}"),
        JsonValue::Null => "Null".to_string(),
    }
}

fn serialize_string_token(value: &str) -> String {
    format!("string:{}:{value}", value.len())
}

fn serialize_array_of_strings(values: &[String]) -> String {
    let mut out = format!("array:{}:", values.len());
    for value in values {
        out.push_str(&serialize_string_token(value));
    }
    out
}

fn empty_json_object() -> JsonValue {
    JsonValue::Object(JsonMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::{
        ComVer, ResolvedDependencySpec, ResolvedDependencyVersion, RootProjectSnapshot,
    };
    use tempfile::tempdir;

    fn load_manifest_from_json(dir: &Path, value: JsonValue) -> PackageManifest {
        let path = dir.join("package.json");
        fs::write(&path, value.to_string()).expect("write package.json");
        PackageManifest::from_path(path).expect("load package.json")
    }

    fn empty_lockfile(project_snapshot: ProjectSnapshot) -> Lockfile {
        Lockfile {
            lockfile_version: ComVer::new(9, 0),
            settings: None,
            never_built_dependencies: None,
            ignored_optional_dependencies: None,
            overrides: None,
            package_extensions_checksum: None,
            patched_dependencies: None,
            pnpmfile_checksum: None,
            catalogs: None,
            time: None,
            extra_fields: Default::default(),
            project_snapshot: RootProjectSnapshot::Single(project_snapshot),
            packages: None,
        }
    }

    #[test]
    fn outdated_setting_detects_overrides() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(dir.path(), serde_json::json!({}));
        let mut lockfile = empty_lockfile(ProjectSnapshot::default());
        lockfile.overrides = Some(HashMap::from([("foo".to_string(), "1.0.0".to_string())]));

        let runtime =
            collect_runtime_lockfile_config(&Npmrc::new(), &manifest, dir.path(), None, false);
        assert_eq!(get_outdated_lockfile_setting(&lockfile, &runtime), Some("overrides"));
    }

    #[test]
    fn outdated_setting_detects_auto_install_peers() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(dir.path(), serde_json::json!({}));
        let mut lockfile = empty_lockfile(ProjectSnapshot::default());
        lockfile.settings =
            Some(LockfileSettings { auto_install_peers: Some(false), ..Default::default() });

        let runtime =
            collect_runtime_lockfile_config(&Npmrc::new(), &manifest, dir.path(), None, false);
        assert_eq!(
            get_outdated_lockfile_setting(&lockfile, &runtime),
            Some("settings.autoInstallPeers")
        );
    }

    #[test]
    fn outdated_setting_detects_catalog_drift() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("pnpm-workspace.yaml"), "catalog:\n  is-positive: =1.0.0\n")
            .expect("write workspace");
        let manifest = load_manifest_from_json(dir.path(), serde_json::json!({}));

        let mut lockfile = empty_lockfile(ProjectSnapshot::default());
        lockfile.catalogs = Some(
            serde_yaml::from_str(
                "default:\n  is-positive:\n    specifier: =2.0.0\n    version: 2.0.0\n",
            )
            .expect("parse catalog snapshot"),
        );

        let runtime =
            collect_runtime_lockfile_config(&Npmrc::new(), &manifest, dir.path(), None, false);
        assert_eq!(get_outdated_lockfile_setting(&lockfile, &runtime), Some("catalogs"));
    }

    #[test]
    fn pnpmfile_checksum_is_none_when_pnpmfile_exports_undefined() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join(".pnpmfile.cjs"), "module.exports = undefined;\n")
            .expect("write pnpmfile");
        let manifest = load_manifest_from_json(dir.path(), serde_json::json!({}));

        let runtime =
            collect_runtime_lockfile_config(&Npmrc::new(), &manifest, dir.path(), None, false);
        assert_eq!(runtime.pnpmfile_checksum, None);
    }

    #[test]
    fn satisfies_manifest_for_matching_snapshot() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "dependencies": { "foo": "^1.0.0" }
            }),
        );

        let mut dependencies = ResolvedDependencyMap::new();
        dependencies.insert(
            "foo".parse().expect("valid package name"),
            ResolvedDependencySpec {
                specifier: "^1.0.0".to_string(),
                version: ResolvedDependencyVersion::PkgVerPeer("1.0.1".parse().unwrap()),
            },
        );

        let snapshot = ProjectSnapshot {
            specifiers: Some(HashMap::from([("foo".to_string(), "^1.0.0".to_string())])),
            dependencies: Some(dependencies),
            optional_dependencies: None,
            dev_dependencies: None,
            dependencies_meta: None,
            publish_directory: None,
        };

        assert!(satisfies_package_manifest(&snapshot, &manifest, false, false, true).is_ok());
    }

    #[test]
    fn fails_when_specifier_drift_exists() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "dependencies": { "foo": "^2.0.0" }
            }),
        );

        let mut dependencies = ResolvedDependencyMap::new();
        dependencies.insert(
            "foo".parse().expect("valid package name"),
            ResolvedDependencySpec {
                specifier: "^1.0.0".to_string(),
                version: ResolvedDependencyVersion::PkgVerPeer("1.0.1".parse().unwrap()),
            },
        );

        let snapshot = ProjectSnapshot {
            specifiers: Some(HashMap::from([("foo".to_string(), "^1.0.0".to_string())])),
            dependencies: Some(dependencies),
            optional_dependencies: None,
            dev_dependencies: None,
            dependencies_meta: None,
            publish_directory: None,
        };

        assert!(satisfies_package_manifest(&snapshot, &manifest, false, false, true).is_err());
    }

    #[test]
    fn allows_specifier_drift_when_strict_check_is_disabled() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "dependencies": { "foo": "^1.0.0" }
            }),
        );

        let mut dependencies = ResolvedDependencyMap::new();
        dependencies.insert(
            "foo".parse().expect("valid package name"),
            ResolvedDependencySpec {
                specifier: "~1.0.0".to_string(),
                version: ResolvedDependencyVersion::PkgVerPeer("1.0.5".parse().unwrap()),
            },
        );

        let snapshot = ProjectSnapshot {
            specifiers: Some(HashMap::from([("foo".to_string(), "~1.0.0".to_string())])),
            dependencies: Some(dependencies),
            optional_dependencies: None,
            dev_dependencies: None,
            dependencies_meta: None,
            publish_directory: None,
        };

        assert!(satisfies_package_manifest(&snapshot, &manifest, false, false, false).is_ok());
    }

    #[test]
    fn auto_install_peers_allows_peer_only_importer_without_lockfile_specifiers() {
        let dir = tempdir().expect("tempdir");
        let manifest = load_manifest_from_json(
            dir.path(),
            serde_json::json!({
                "peerDependencies": { "is-positive": ">=1.0.0" }
            }),
        );

        let snapshot = ProjectSnapshot {
            specifiers: None,
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            dependencies_meta: None,
            publish_directory: None,
        };

        assert!(satisfies_package_manifest(&snapshot, &manifest, true, false, true).is_ok());
    }

    #[test]
    fn package_extensions_checksum_matches_pnpm_object_hash_simple_case() {
        // Reference computed with object-hash@3.0.0 and options:
        // { respectType: false, algorithm: "sha256", encoding: "base64",
        //   unorderedArrays: true, unorderedObjects: true, unorderedSets: true }
        let value = serde_json::json!({
            "is-positive": {
                "dependencies": {
                    "@pnpm.e2e/bar": "100.1.0"
                }
            }
        });
        let checksum = hash_object_nullable_with_prefix(Some(&value)).expect("checksum exists");
        assert_eq!(checksum, "sha256-HZEpjtRdr7gJfO0V6YoFDfxWmaw3anoE1/tQQbzas+E=");
    }

    #[test]
    fn package_extensions_checksum_matches_pnpm_object_hash_array_case() {
        // Reference computed with object-hash@3.0.0 and options:
        // { respectType: false, algorithm: "sha256", encoding: "base64",
        //   unorderedArrays: true, unorderedObjects: true, unorderedSets: true }
        let value = serde_json::json!({
            "a": [3, 1, 2],
            "b": [
                { "x": 2, "y": 1 },
                { "y": 1, "x": 2 }
            ]
        });
        let checksum = hash_object_nullable_with_prefix(Some(&value)).expect("checksum exists");
        assert_eq!(checksum, "sha256-fWykAxu8ur0i18o5r+cTdIb5mxx4Rzd75UKVAW44mHM=");
    }
}
