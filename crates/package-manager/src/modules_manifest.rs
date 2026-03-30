use crate::create_virtual_store::select_hoisted_packages;
use httpdate::{fmt_http_date, parse_http_date};
use pacquet_fs::{is_symlink_or_junction, symlink_or_junction_target};
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_npmrc::{NodeLinker, Npmrc};
use pacquet_package_manifest::DependencyGroup;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MODULES_MANIFEST_FILE_NAME: &str = ".modules.yaml";
#[cfg(windows)]
pub(crate) const DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH: u16 = 60;
#[cfg(not(windows))]
pub(crate) const DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH: u16 = 120;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModulesManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    hoist_pattern: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hoisted_dependencies: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    injected_deps: Option<BTreeMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout_version: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_linker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_manager: Option<String>,
    #[serde(default)]
    pending_builds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pruned_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_hoist_pattern: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shamefully_hoist: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registries: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ignored_builds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    virtual_store_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    virtual_store_dir_max_length: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    included: Option<IncludedDependencies>,
    #[serde(default)]
    skipped: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IncludedDependencies {
    pub(crate) dependencies: bool,
    pub(crate) dev_dependencies: bool,
    pub(crate) optional_dependencies: bool,
}

pub(crate) fn should_prune_orphaned_virtual_store_entries(
    modules_dir: &Path,
    modules_cache_max_age_minutes: u64,
) -> bool {
    let Some(pruned_at) =
        read_modules_manifest(modules_dir).and_then(|manifest| manifest.pruned_at)
    else {
        return true;
    };

    if modules_cache_max_age_minutes == 0 {
        return true;
    }

    cache_expired(&pruned_at, modules_cache_max_age_minutes)
}

pub fn write_modules_manifest(
    modules_dir: &Path,
    config: &Npmrc,
    dependency_groups: &[DependencyGroup],
    skipped: &[String],
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    direct_dependency_names: Option<&[String]>,
) -> io::Result<()> {
    fs::create_dir_all(modules_dir)?;
    let mut skipped = skipped.iter().cloned().collect::<std::collections::BTreeSet<_>>();
    if let Some(packages) = packages {
        for (dependency_path, package_snapshot) in packages {
            if package_snapshot.optional == Some(true)
                && crate::installability::should_skip_optional_package_snapshot(
                    &dependency_path.to_string(),
                    package_snapshot,
                )
            {
                skipped.insert(modules_manifest_package_id(dependency_path));
            }
        }
    }
    let mut skipped = skipped.into_iter().collect::<Vec<_>>();
    skipped.sort();
    let included = included_dependencies(dependency_groups);
    let manifest = ModulesManifest {
        hoist_pattern: (!config.hoist_pattern.is_empty()).then(|| config.hoist_pattern.clone()),
        hoisted_dependencies: hoisted_dependencies(config, packages, direct_dependency_names),
        injected_deps: Some(BTreeMap::new()),
        layout_version: Some(5),
        node_linker: Some(
            match config.node_linker {
                NodeLinker::Hoisted => "hoisted",
                NodeLinker::Isolated => "isolated",
                NodeLinker::Pnp => "pnp",
            }
            .to_string(),
        ),
        package_manager: Some(detect_pnpm_package_manager()),
        pruned_at: Some(fmt_http_date(SystemTime::now())),
        public_hoist_pattern: Some(config.public_hoist_pattern.clone()),
        shamefully_hoist: None,
        registries: Some(config.effective_registries()),
        pending_builds: Vec::new(),
        store_dir: Some(canonical_store_dir_for_config(config)),
        ignored_builds: Vec::new(),
        virtual_store_dir: Some(relative_virtual_store_dir(modules_dir, &config.virtual_store_dir)),
        virtual_store_dir_max_length: Some(DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH),
        included: Some(included),
        skipped,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(io::Error::other)? + "\n";
    fs::write(modules_dir.join(MODULES_MANIFEST_FILE_NAME), json)
}

fn detect_pnpm_package_manager() -> String {
    static PACKAGE_MANAGER: OnceLock<String> = OnceLock::new();
    PACKAGE_MANAGER
        .get_or_init(|| {
            detect_pnpm_version()
                .map(|version| format!("pnpm@{version}"))
                .unwrap_or_else(|| format!("pacquet@{}", env!("CARGO_PKG_VERSION")))
        })
        .clone()
}

fn detect_pnpm_version() -> Option<String> {
    for command in if cfg!(windows) { ["pnpm", "pnpm.cmd"] } else { ["pnpm", "pnpm"] } {
        let Ok(output) = Command::new(command).arg("--version").output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !version.is_empty() {
            return Some(version);
        }
    }
    None
}

pub(crate) fn canonical_store_dir_for_config(config: &Npmrc) -> String {
    let path = PathBuf::from(config.store_dir.display().to_string()).join("v10");
    normalize_windows_verbatim_path(&fs::canonicalize(&path).unwrap_or(path).display().to_string())
}

fn normalize_windows_verbatim_path(path: &str) -> String {
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{path}");
    }
    path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
}

fn hoisted_dependencies(
    config: &Npmrc,
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    direct_dependency_names: Option<&[String]>,
) -> Option<BTreeMap<String, BTreeMap<String, String>>> {
    let packages = packages?;
    if !config.symlink {
        return None;
    }
    let direct_dependency_names = direct_dependency_names.unwrap_or(&[]);

    let mut hoisted = BTreeMap::<String, BTreeMap<String, String>>::new();

    if (config.hoist || config.shamefully_hoist)
        && !collect_hoisted_dependencies_from_fs(
            &config.virtual_store_dir.join("node_modules"),
            packages,
            direct_dependency_names,
            "private",
            &mut hoisted,
        )
    {
        for (name, package_specifier) in
            select_hoisted_packages(packages, config.dedupe_peer_dependents, &config.hoist_pattern)
        {
            if direct_dependency_names.contains(&name) {
                continue;
            }
            // pnpm writes hoistedDependencies keys without leading `/`
            let key = package_specifier.to_string();
            let key = key.strip_prefix('/').unwrap_or(&key).to_string();
            hoisted.entry(key).or_default().insert(name, "private".to_string());
        }
    }

    if !config.public_hoist_pattern.is_empty()
        && !collect_hoisted_dependencies_from_fs(
            &config.modules_dir,
            packages,
            direct_dependency_names,
            "public",
            &mut hoisted,
        )
    {
        for (name, package_specifier) in select_hoisted_packages(
            packages,
            config.dedupe_peer_dependents,
            &config.public_hoist_pattern,
        ) {
            if direct_dependency_names.contains(&name) {
                continue;
            }
            let key = package_specifier.to_string();
            let key = key.strip_prefix('/').unwrap_or(&key).to_string();
            hoisted.entry(key).or_default().insert(name, "public".to_string());
        }
    }

    (!hoisted.is_empty()).then_some(hoisted)
}

fn collect_hoisted_dependencies_from_fs(
    hoist_dir: &Path,
    packages: &HashMap<DependencyPath, PackageSnapshot>,
    direct_dependency_names: &[String],
    visibility: &str,
    hoisted: &mut BTreeMap<String, BTreeMap<String, String>>,
) -> bool {
    let Ok(entries) = collect_hoist_dir_entries(hoist_dir) else {
        return false;
    };
    if entries.is_empty() {
        return false;
    }
    // Build a map from package name to its depPath (without leading `/`).
    // pnpm uses the actual package's depPath as the hoistedDependencies key,
    // NOT the parent package that hosts it in its node_modules.
    let dep_path_by_name = packages
        .keys()
        .map(|dependency_path| {
            let name = dependency_path.package_specifier.name().to_string();
            let key = dependency_path.to_string();
            let key = key.strip_prefix('/').unwrap_or(&key).to_string();
            (name, key)
        })
        .collect::<HashMap<_, _>>();
    let mut added_any = false;
    for (alias, entry_path) in entries {
        if direct_dependency_names.contains(&alias) {
            continue;
        }
        // Verify the hoisted entry actually resolves to a valid target.
        if resolve_hoisted_entry_target(&entry_path).is_err() {
            continue;
        }
        // Use the alias (package name) to find the actual package's depPath,
        // matching pnpm's behavior of using `graph[nodeId].depPath`.
        let Some(dep_path_key) = dep_path_by_name.get(&alias) else {
            continue;
        };
        hoisted.entry(dep_path_key.clone()).or_default().insert(alias, visibility.to_string());
        added_any = true;
    }
    added_any
}

fn collect_hoist_dir_entries(hoist_dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    if !hoist_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(hoist_dir)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if matches!(file_name.as_str(), ".bin" | ".pnpm") {
            continue;
        }
        let entry_path = entry.path();
        if file_name.starts_with('@') && entry_path.is_dir() {
            for scoped_entry in fs::read_dir(&entry_path)? {
                let scoped_entry = scoped_entry?;
                let scoped_name = scoped_entry.file_name().to_string_lossy().into_owned();
                let alias = format!("{file_name}/{scoped_name}");
                entries.push((alias, scoped_entry.path()));
            }
            continue;
        }
        let alias = file_name.strip_prefix(".ignored_").unwrap_or(&file_name).to_string();
        entries.push((alias, entry_path));
    }
    Ok(entries)
}

fn resolve_hoisted_entry_target(entry_path: &Path) -> io::Result<PathBuf> {
    if is_symlink_or_junction(entry_path).unwrap_or(false) {
        let target = symlink_or_junction_target(entry_path)?;
        if target.is_absolute() {
            Ok(target)
        } else {
            Ok(entry_path.parent().unwrap_or_else(|| Path::new(".")).join(target))
        }
    } else {
        fs::canonicalize(entry_path)
    }
}

fn modules_manifest_package_id(dependency_path: &DependencyPath) -> String {
    match dependency_path.custom_registry.as_deref() {
        Some(custom_registry) => {
            format!("{custom_registry}/{}", dependency_path.package_specifier)
        }
        None => dependency_path.package_specifier.to_string(),
    }
}

fn relative_virtual_store_dir(modules_dir: &Path, virtual_store_dir: &Path) -> String {
    // pnpm writes virtualStoreDir as absolute path on Windows and relative on Unix.
    // On Windows, pnpm uses native backslashes in the absolute path WITHOUT the
    // \\?\ verbatim prefix that fs::canonicalize produces.
    if cfg!(windows) {
        // Canonicalize to resolve 8.3 short names (RUNNER~1 → runneradmin) and
        // normalize mixed forward/back slashes to native backslashes, then strip
        // the \\?\ prefix that Windows canonicalization adds.
        let resolved =
            fs::canonicalize(virtual_store_dir).unwrap_or_else(|_| virtual_store_dir.to_path_buf());
        return normalize_windows_verbatim_path(&resolved.display().to_string());
    }
    if let Ok(relative) = virtual_store_dir.strip_prefix(modules_dir) {
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(relative)
        };
        return relative.display().to_string();
    }
    virtual_store_dir.display().to_string()
}

pub(crate) fn read_modules_manifest(modules_dir: &Path) -> Option<ModulesManifest> {
    let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME)).ok()?;
    let mut manifest = serde_yaml::from_str::<ModulesManifest>(&content).ok()?;

    manifest.virtual_store_dir = Some(
        manifest
            .virtual_store_dir
            .clone()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                modules_dir
                    .join(manifest.virtual_store_dir.clone().unwrap_or_else(|| ".pnpm".to_string()))
            })
            .display()
            .to_string(),
    );
    match manifest.shamefully_hoist {
        Some(true) if manifest.public_hoist_pattern.is_none() => {
            manifest.public_hoist_pattern = Some(vec!["*".to_string()]);
        }
        Some(false) if manifest.public_hoist_pattern.is_none() => {
            manifest.public_hoist_pattern = Some(Vec::new());
        }
        _ => {}
    }
    if manifest.pruned_at.is_none() {
        manifest.pruned_at = Some(fmt_http_date(SystemTime::now()));
    }
    if manifest.virtual_store_dir_max_length.is_none() {
        manifest.virtual_store_dir_max_length = Some(DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH);
    }

    Some(manifest)
}

pub(crate) fn included_dependencies(dependency_groups: &[DependencyGroup]) -> IncludedDependencies {
    IncludedDependencies {
        dependencies: dependency_groups.contains(&DependencyGroup::Prod),
        dev_dependencies: dependency_groups.contains(&DependencyGroup::Dev),
        optional_dependencies: dependency_groups.contains(&DependencyGroup::Optional),
    }
}

impl ModulesManifest {
    pub(crate) fn hoist_pattern(&self) -> Option<&[String]> {
        self.hoist_pattern.as_deref()
    }

    pub(crate) fn node_linker(&self) -> Option<&str> {
        self.node_linker.as_deref()
    }

    pub(crate) fn public_hoist_pattern(&self) -> Option<&[String]> {
        self.public_hoist_pattern.as_deref()
    }

    pub(crate) fn virtual_store_dir_max_length(&self) -> u16 {
        self.virtual_store_dir_max_length.unwrap_or(DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH)
    }

    pub(crate) fn store_dir(&self) -> Option<&str> {
        self.store_dir.as_deref()
    }

    pub(crate) fn included(&self) -> Option<&IncludedDependencies> {
        self.included.as_ref()
    }

    pub(crate) fn skipped(&self) -> &[String] {
        &self.skipped
    }

    pub(crate) fn resolved_virtual_store_dir(&self, modules_dir: &Path) -> PathBuf {
        match self.virtual_store_dir.as_deref() {
            None => modules_dir.join(".pnpm"),
            Some(virtual_store_dir) => {
                let virtual_store_dir = PathBuf::from(virtual_store_dir);
                if virtual_store_dir.is_absolute() {
                    virtual_store_dir
                } else {
                    modules_dir.join(virtual_store_dir)
                }
            }
        }
    }
}

fn cache_expired(pruned_at: &str, modules_cache_max_age_minutes: u64) -> bool {
    let parsed = pruned_at
        .parse::<u64>()
        .ok()
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
        .or_else(|| parse_http_date(pruned_at).ok());
    let Some(pruned_at_time) = parsed else {
        return true;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(pruned_at_time) else {
        return false;
    };
    elapsed >= Duration::from_secs(modules_cache_max_age_minutes.saturating_mul(60))
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH, MODULES_MANIFEST_FILE_NAME,
        detect_pnpm_package_manager, read_modules_manifest,
        should_prune_orphaned_virtual_store_entries, write_modules_manifest,
    };
    use pacquet_lockfile::{
        DependencyPath, LockfileResolution, PackageSnapshot, TarballResolution,
    };
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::DependencyGroup;
    use pacquet_store_dir::StoreDir;
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn dummy_snapshot() -> PackageSnapshot {
        PackageSnapshot {
            resolution: LockfileResolution::Tarball(TarballResolution {
                tarball: "https://example.test/pkg.tgz".to_string(),
                integrity: None,
            }),
            id: None,
            name: None,
            version: None,
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: None,
            has_bin: None,
            prepare: None,
            requires_build: None,
            bundled_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            dependencies: None,
            optional_dependencies: None,
            transitive_peer_dependencies: None,
            dev: None,
            optional: None,
        }
    }

    #[test]
    fn should_prune_without_modules_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 10));
    }

    #[test]
    fn should_not_prune_when_manifest_is_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        write_modules_manifest(
            dir.path(),
            &config,
            &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
            &[],
            None,
            None,
        )
        .expect("write modules manifest");
        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"skipped\": []"));

        assert!(!should_prune_orphaned_virtual_store_entries(dir.path(), 10));
    }

    #[test]
    fn should_prune_when_manifest_is_expired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("unix time")
            .saturating_sub(Duration::from_secs(3 * 60))
            .as_secs();
        fs::write(dir.path().join(MODULES_MANIFEST_FILE_NAME), format!("prunedAt: '{old}'\n"))
            .expect("write modules manifest");

        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 2));
    }

    #[test]
    fn zero_cache_age_prunes_even_when_manifest_is_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        write_modules_manifest(dir.path(), &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        assert!(should_prune_orphaned_virtual_store_entries(dir.path(), 0));
    }

    #[test]
    fn writes_sorted_skipped_packages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        write_modules_manifest(
            dir.path(),
            &config,
            &[DependencyGroup::Prod, DependencyGroup::Optional],
            &["b@1.0.0".to_string(), "a@1.0.0".to_string()],
            None,
            None,
        )
        .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"skipped\": ["));
        assert!(content.contains("\"a@1.0.0\""));
        assert!(content.contains("\"b@1.0.0\""));
        assert!(content.contains("\"layoutVersion\": 5"));
        assert!(content.contains("\"nodeLinker\": \"isolated\""));
        assert!(content.contains("\"packageManager\": "));
        assert!(content.contains("\"injectedDeps\": {}"));
        if cfg!(windows) {
            assert!(!content.contains("\\\\?\\"));
        } else {
            assert!(content.contains("\"virtualStoreDir\": \".pnpm\""));
        }
        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");
        assert_eq!(manifest.resolved_virtual_store_dir(dir.path()), config.virtual_store_dir);
        assert!(content.contains("\"dependencies\": true"));
        assert!(content.contains("\"optionalDependencies\": true"));
    }

    #[test]
    fn detect_package_manager_prefers_pnpm_version_with_fallback() {
        let package_manager = detect_pnpm_package_manager();
        assert!(
            package_manager.starts_with("pnpm@")
                || package_manager == format!("pacquet@{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn writes_included_dependency_flags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        write_modules_manifest(dir.path(), &config, &[DependencyGroup::Dev], &[], None, None)
            .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"dependencies\": false"));
        assert!(content.contains("\"devDependencies\": true"));
        assert!(content.contains("\"optionalDependencies\": false"));
    }

    #[test]
    fn writes_hoist_patterns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        config.hoist_pattern = vec!["*eslint*".to_string()];
        config.public_hoist_pattern = vec!["*".to_string(), "!typescript".to_string()];
        write_modules_manifest(dir.path(), &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"hoistPattern\": ["));
        assert!(content.contains("\"*eslint*\""));
        assert!(content.contains("\"publicHoistPattern\": ["));
        assert!(content.contains("\"!typescript\""));
    }

    #[test]
    fn writes_registries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        config.registry = "https://default.example/".to_string();
        config.set_raw_setting("@foo:registry", "https://foo.example");
        write_modules_manifest(dir.path(), &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"registries\": {"));
        assert!(content.contains("\"default\": \"https://default.example/\""));
        assert!(content.contains("https://foo.example/"));
        assert!(content.contains("@foo"));
        assert!(content.contains("@jsr"));
    }

    #[test]
    fn writes_pnpm_like_hoisted_dependencies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        let dep_path: DependencyPath = "/@scope/pkg@1.0.0".parse().expect("dep path");
        let packages = HashMap::from([(dep_path, dummy_snapshot())]);
        write_modules_manifest(
            dir.path(),
            &config,
            &[DependencyGroup::Prod],
            &[],
            Some(&packages),
            None,
        )
        .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"hoistedDependencies\": {"));
        assert!(content.contains("\"@scope/pkg@1.0.0\": {"));
        assert!(content.contains("\"@scope/pkg\": \"private\""));
    }

    #[test]
    fn writes_virtual_store_dir_max_length_and_build_arrays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        write_modules_manifest(dir.path(), &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");
        assert!(content.contains(&format!(
            "\"virtualStoreDirMaxLength\": {DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH}"
        )));
        assert!(content.contains("\"pendingBuilds\": []"));
        assert!(!content.contains("\"ignoredBuilds\""));
        assert!(content.contains("\"storeDir\":"));
        assert!(
            PathBuf::from(manifest.store_dir.expect("store dir")).ends_with(Path::new("store/v10"))
        );
    }

    #[test]
    fn writes_skipped_packages_detected_from_optional_installability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = dir.path().join(".pnpm");
        let packages = HashMap::from([(
            DependencyPath::registry(
                None,
                "@scope/optional@1.0.0".parse().expect("package specifier"),
            ),
            PackageSnapshot {
                resolution: LockfileResolution::Registry(pacquet_lockfile::RegistryResolution {
                    integrity: "sha512-Bw==".parse().expect("integrity"),
                }),
                id: None,
                name: None,
                version: None,
                engines: None,
                cpu: None,
                os: Some(vec!["definitely-not-this-os".to_string()]),
                libc: None,
                deprecated: None,
                has_bin: None,
                prepare: None,
                requires_build: None,
                bundled_dependencies: None,
                peer_dependencies: None,
                peer_dependencies_meta: None,
                dependencies: None,
                optional_dependencies: None,
                transitive_peer_dependencies: None,
                dev: None,
                optional: Some(true),
            },
        )]);
        write_modules_manifest(
            dir.path(),
            &config,
            &[DependencyGroup::Optional],
            &[],
            Some(&packages),
            None,
        )
        .expect("write modules manifest");

        let content =
            fs::read_to_string(dir.path().join(MODULES_MANIFEST_FILE_NAME)).expect("read manifest");
        assert!(content.contains("\"@scope/optional@1.0.0\""));
    }

    #[test]
    fn read_modules_manifest_normalizes_relative_virtual_store_dir_and_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join(MODULES_MANIFEST_FILE_NAME),
            "shamefullyHoist: false\nvirtualStoreDir: .pnpm\n",
        )
        .expect("write modules manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");

        assert_eq!(manifest.resolved_virtual_store_dir(dir.path()), dir.path().join(".pnpm"));
        assert_eq!(manifest.public_hoist_pattern(), Some([].as_slice()));
        assert_eq!(manifest.virtual_store_dir_max_length(), DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH);
        assert!(manifest.pruned_at.is_some());
    }

    #[test]
    fn read_modules_manifest_defaults_virtual_store_dir_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(MODULES_MANIFEST_FILE_NAME), "layoutVersion: 5\n")
            .expect("write modules manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");

        assert_eq!(manifest.resolved_virtual_store_dir(dir.path()), dir.path().join(".pnpm"));
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");
        config.hoist_pattern = vec!["*".to_string()];
        config.public_hoist_pattern = vec!["*types*".to_string(), "*eslint*".to_string()];
        config.registry = "https://registry.npmjs.org/".to_string();

        write_modules_manifest(
            &modules_dir,
            &config,
            &[DependencyGroup::Prod, DependencyGroup::Dev],
            &["skipped-pkg@1.0.0".to_string()],
            None,
            None,
        )
        .expect("write modules manifest");

        let manifest = read_modules_manifest(&modules_dir).expect("read modules manifest");

        assert_eq!(manifest.layout_version, Some(5));
        assert_eq!(manifest.node_linker(), Some("isolated"));
        assert_eq!(manifest.hoist_pattern(), Some(vec!["*".to_string()].as_slice()));
        assert_eq!(
            manifest.public_hoist_pattern(),
            Some(vec!["*types*".to_string(), "*eslint*".to_string()].as_slice())
        );
        assert!(
            manifest.package_manager.as_ref().expect("package_manager").contains("pnpm@")
                || manifest.package_manager.as_ref().expect("package_manager").contains("pacquet@")
        );
        assert!(manifest.pruned_at.is_some());
        assert_eq!(manifest.skipped(), &["skipped-pkg@1.0.0"]);
        assert_eq!(manifest.pending_builds, Vec::<String>::new());
        assert!(manifest.store_dir().is_some());
        assert!(manifest.store_dir().expect("store_dir").contains("store"));
        assert_eq!(
            manifest.virtual_store_dir_max_length(),
            super::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH
        );
        let included = manifest.included().expect("included");
        assert!(included.dependencies);
        assert!(included.dev_dependencies);
        assert!(!included.optional_dependencies);
    }

    #[cfg(not(windows))]
    #[test]
    fn virtual_store_dir_is_relative_on_unix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");

        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME))
            .expect("read manifest");
        let raw: serde_json::Value = serde_json::from_str(&content).expect("parse JSON");
        let vsd = raw["virtualStoreDir"].as_str().expect("virtualStoreDir field");

        // On Unix the virtualStoreDir must be a relative path (e.g. ".pnpm")
        assert!(
            !PathBuf::from(vsd).is_absolute(),
            "virtualStoreDir should be relative on Unix, got: {vsd}"
        );
        assert_eq!(vsd, ".pnpm");
    }

    #[cfg(windows)]
    #[test]
    fn virtual_store_dir_is_absolute_on_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");

        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME))
            .expect("read manifest");
        let raw: serde_json::Value = serde_json::from_str(&content).expect("parse JSON");
        let vsd = raw["virtualStoreDir"].as_str().expect("virtualStoreDir field");

        // On Windows the virtualStoreDir must be an absolute path
        assert!(
            PathBuf::from(vsd).is_absolute(),
            "virtualStoreDir should be absolute on Windows, got: {vsd}"
        );
        // Must NOT contain the \\?\ verbatim prefix
        assert!(
            !vsd.starts_with(r"\\?\"),
            "virtualStoreDir should not have \\\\?\\ prefix on Windows, got: {vsd}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn virtual_store_dir_without_verbatim_prefix_on_windows() {
        use super::normalize_windows_verbatim_path;

        // Standard verbatim path
        let result = normalize_windows_verbatim_path(r"\\?\C:\Users\test\.pnpm");
        assert_eq!(result, r"C:\Users\test\.pnpm");

        // UNC verbatim path
        let unc_result = normalize_windows_verbatim_path(r"\\?\UNC\server\share\path");
        assert_eq!(unc_result, r"\\server\share\path");

        // Path without prefix should be unchanged
        let normal = normalize_windows_verbatim_path(r"C:\Users\test\.pnpm");
        assert_eq!(normal, r"C:\Users\test\.pnpm");
    }

    #[test]
    fn normalize_windows_verbatim_path_strips_prefix() {
        use super::normalize_windows_verbatim_path;

        // Standard \\?\ prefix
        assert_eq!(
            normalize_windows_verbatim_path(r"\\?\C:\Users\test\store\v10"),
            r"C:\Users\test\store\v10"
        );

        // UNC prefix \\?\UNC\ -> \\
        assert_eq!(normalize_windows_verbatim_path(r"\\?\UNC\server\share"), r"\\server\share");

        // No prefix -> unchanged
        assert_eq!(normalize_windows_verbatim_path("/home/user/store/v10"), "/home/user/store/v10");

        // Empty string -> unchanged
        assert_eq!(normalize_windows_verbatim_path(""), "");
    }

    #[test]
    fn store_dir_gets_normalized() {
        use super::canonical_store_dir_for_config;

        let dir = tempfile::tempdir().expect("tempdir");
        let store_path = dir.path().join("store");
        fs::create_dir_all(store_path.join("v10")).expect("create store/v10");

        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(&store_path);

        let result = canonical_store_dir_for_config(&config);

        // The result should end with store/v10
        assert!(
            result.ends_with("store/v10") || result.ends_with(r"store\v10"),
            "store dir should end with store/v10, got: {result}"
        );

        // Should not contain \\?\ prefix (relevant on Windows, always true on Unix)
        assert!(
            !result.starts_with(r"\\?\"),
            "store dir should not have \\\\?\\ prefix, got: {result}"
        );
    }

    #[test]
    fn default_virtual_store_dir_max_length_platform_specific() {
        #[cfg(windows)]
        {
            assert_eq!(
                super::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH,
                60,
                "DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH should be 60 on Windows"
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                super::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH,
                120,
                "DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH should be 120 on Unix"
            );
        }

        // Also verify it is populated after reading a manifest without the field
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(MODULES_MANIFEST_FILE_NAME), "layoutVersion: 5\n")
            .expect("write manifest");
        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");
        assert_eq!(
            manifest.virtual_store_dir_max_length(),
            super::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH
        );
    }

    #[test]
    fn read_backwards_compatible_manifest_shamefully_hoist_true() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Manually write a JSON manifest with shamefullyHoist: true and no publicHoistPattern.
        // This simulates an older pnpm manifest format.
        let manifest_json = serde_json::json!({
            "shamefullyHoist": true,
            "layoutVersion": 5,
            "nodeLinker": "isolated",
            "virtualStoreDir": ".pnpm"
        });
        fs::write(
            dir.path().join(MODULES_MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&manifest_json).expect("serialize"),
        )
        .expect("write manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");

        // When shamefullyHoist is true and publicHoistPattern is missing,
        // read_modules_manifest should default publicHoistPattern to ["*"]
        assert_eq!(manifest.shamefully_hoist, Some(true));
        assert_eq!(
            manifest.public_hoist_pattern(),
            Some(vec!["*".to_string()].as_slice()),
            "shamefullyHoist=true should set publicHoistPattern to [\"*\"]"
        );
    }

    #[test]
    fn read_backwards_compatible_manifest_shamefully_hoist_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Manually write a JSON manifest with shamefullyHoist: false and no publicHoistPattern.
        let manifest_json = serde_json::json!({
            "shamefullyHoist": false,
            "layoutVersion": 5,
            "nodeLinker": "isolated",
            "virtualStoreDir": ".pnpm"
        });
        fs::write(
            dir.path().join(MODULES_MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&manifest_json).expect("serialize"),
        )
        .expect("write manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");

        // When shamefullyHoist is false and publicHoistPattern is missing,
        // read_modules_manifest should default publicHoistPattern to []
        assert_eq!(manifest.shamefully_hoist, Some(false));
        assert_eq!(
            manifest.public_hoist_pattern(),
            Some(Vec::<String>::new().as_slice()),
            "shamefullyHoist=false should set publicHoistPattern to []"
        );
    }

    #[test]
    fn read_backwards_compatible_shamefully_hoist_with_existing_public_hoist_pattern() {
        let dir = tempfile::tempdir().expect("tempdir");
        // When publicHoistPattern is already set, shamefullyHoist should NOT override it
        let manifest_json = serde_json::json!({
            "shamefullyHoist": true,
            "publicHoistPattern": ["@types/*"],
            "layoutVersion": 5,
            "virtualStoreDir": ".pnpm"
        });
        fs::write(
            dir.path().join(MODULES_MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&manifest_json).expect("serialize"),
        )
        .expect("write manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");

        // publicHoistPattern should remain as explicitly set, not overridden by shamefullyHoist
        assert_eq!(manifest.shamefully_hoist, Some(true));
        assert_eq!(
            manifest.public_hoist_pattern(),
            Some(vec!["@types/*".to_string()].as_slice()),
            "existing publicHoistPattern should not be overridden by shamefullyHoist"
        );
    }

    #[test]
    fn hoisted_dependencies_written_correctly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");
        config.hoist = true;
        config.hoist_pattern = vec!["*".to_string()];

        // Create packages that should be hoisted
        let dep_path_a: DependencyPath = "/lodash@4.17.21".parse().expect("dep path");
        let dep_path_b: DependencyPath = "/@babel/core@7.23.0".parse().expect("dep path");
        let packages =
            HashMap::from([(dep_path_a, dummy_snapshot()), (dep_path_b, dummy_snapshot())]);

        write_modules_manifest(
            &modules_dir,
            &config,
            &[DependencyGroup::Prod],
            &[],
            Some(&packages),
            None,
        )
        .expect("write modules manifest");

        let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME))
            .expect("read manifest");
        let raw: serde_json::Value = serde_json::from_str(&content).expect("parse JSON");

        let hoisted = raw.get("hoistedDependencies").expect("hoistedDependencies field");
        assert!(hoisted.is_object(), "hoistedDependencies should be an object");

        // Check that hoisted keys do NOT have leading `/` (pnpm convention)
        for key in hoisted.as_object().expect("object").keys() {
            assert!(
                !key.starts_with('/'),
                "hoistedDependencies key should not have leading '/', got: {key}"
            );
        }

        // Verify lodash is hoisted as private
        let lodash_entry = hoisted.get("lodash@4.17.21");
        assert!(lodash_entry.is_some(), "lodash@4.17.21 should be in hoistedDependencies");
        let lodash_obj = lodash_entry.expect("lodash").as_object().expect("object");
        assert_eq!(
            lodash_obj.get("lodash").and_then(|v| v.as_str()),
            Some("private"),
            "lodash should be hoisted as private"
        );

        // Verify @babel/core is hoisted as private
        let babel_entry = hoisted.get("@babel/core@7.23.0");
        assert!(babel_entry.is_some(), "@babel/core@7.23.0 should be in hoistedDependencies");
        let babel_obj = babel_entry.expect("babel").as_object().expect("object");
        assert_eq!(
            babel_obj.get("@babel/core").and_then(|v| v.as_str()),
            Some("private"),
            "@babel/core should be hoisted as private"
        );
    }

    #[test]
    fn hoisted_dependencies_excludes_direct_dependencies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");
        config.hoist = true;
        config.hoist_pattern = vec!["*".to_string()];

        let dep_path: DependencyPath = "/lodash@4.17.21".parse().expect("dep path");
        let packages = HashMap::from([(dep_path, dummy_snapshot())]);

        // lodash is a direct dependency, so it should NOT appear in hoistedDependencies
        write_modules_manifest(
            &modules_dir,
            &config,
            &[DependencyGroup::Prod],
            &[],
            Some(&packages),
            Some(&["lodash".to_string()]),
        )
        .expect("write modules manifest");

        let content = fs::read_to_string(modules_dir.join(MODULES_MANIFEST_FILE_NAME))
            .expect("read manifest");
        let raw: serde_json::Value = serde_json::from_str(&content).expect("parse JSON");

        // hoistedDependencies should be absent or empty since the only package is a direct dep
        match raw.get("hoistedDependencies") {
            None => {} // OK, not written at all
            Some(hoisted) => {
                assert!(
                    hoisted.as_object().map_or(true, |o| o.is_empty()),
                    "hoistedDependencies should not contain direct dependencies"
                );
            }
        }
    }

    #[test]
    fn roundtrip_preserves_registries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let modules_dir = dir.path().join("node_modules");
        let mut config = Npmrc::new();
        config.store_dir = StoreDir::new(dir.path().join("store"));
        config.virtual_store_dir = modules_dir.join(".pnpm");
        config.registry = "https://custom.registry.example/".to_string();
        config.set_raw_setting("@myorg:registry", "https://myorg.example");

        write_modules_manifest(&modules_dir, &config, &[DependencyGroup::Prod], &[], None, None)
            .expect("write modules manifest");

        let manifest = read_modules_manifest(&modules_dir).expect("read modules manifest");
        let registries = manifest.registries.expect("registries");
        assert_eq!(
            registries.get("default").map(String::as_str),
            Some("https://custom.registry.example/")
        );
        assert!(registries.contains_key("@myorg"));
        assert!(registries.contains_key("@jsr"));
    }

    #[test]
    fn read_manifest_fills_pruned_at_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Write manifest without prunedAt
        fs::write(
            dir.path().join(MODULES_MANIFEST_FILE_NAME),
            "layoutVersion: 5\nnodeLinker: isolated\n",
        )
        .expect("write manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");
        assert!(
            manifest.pruned_at.is_some(),
            "prunedAt should be filled in when missing from manifest"
        );
    }

    #[test]
    fn read_manifest_fills_virtual_store_dir_max_length_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Write manifest without virtualStoreDirMaxLength
        fs::write(dir.path().join(MODULES_MANIFEST_FILE_NAME), "layoutVersion: 5\n")
            .expect("write manifest");

        let manifest = read_modules_manifest(dir.path()).expect("read modules manifest");
        assert_eq!(
            manifest.virtual_store_dir_max_length,
            Some(super::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH),
            "virtualStoreDirMaxLength should be filled in with platform default when missing"
        );
    }
}
