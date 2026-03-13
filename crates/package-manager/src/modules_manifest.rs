use crate::create_virtual_store::select_hoisted_packages;
use httpdate::{fmt_http_date, parse_http_date};
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_npmrc::{NodeLinker, Npmrc};
use pacquet_package_manifest::DependencyGroup;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MODULES_MANIFEST_FILE_NAME: &str = ".modules.yaml";
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

pub(crate) fn write_modules_manifest(
    modules_dir: &Path,
    config: &Npmrc,
    dependency_groups: &[DependencyGroup],
    skipped: &[String],
    packages: Option<&HashMap<DependencyPath, PackageSnapshot>>,
    direct_dependency_names: Option<&[String]>,
) -> io::Result<()> {
    fs::create_dir_all(modules_dir)?;
    let mut skipped = skipped.to_vec();
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
        store_dir: Some(canonical_store_dir(config)),
        ignored_builds: Vec::new(),
        virtual_store_dir: Some(relative_virtual_store_dir(modules_dir, &config.virtual_store_dir)),
        virtual_store_dir_max_length: Some(DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH),
        included: Some(included),
        skipped,
    };
    let yaml = serde_yaml::to_string(&manifest).map_err(io::Error::other)?;
    fs::write(modules_dir.join(MODULES_MANIFEST_FILE_NAME), yaml)
}

fn detect_pnpm_package_manager() -> String {
    static PACKAGE_MANAGER: OnceLock<String> = OnceLock::new();
    PACKAGE_MANAGER.get_or_init(|| format!("pacquet@{}", env!("CARGO_PKG_VERSION"))).clone()
}

fn canonical_store_dir(config: &Npmrc) -> String {
    let path = PathBuf::from(config.store_dir.display().to_string()).join("v10");
    fs::canonicalize(&path).unwrap_or(path).display().to_string()
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

    if config.hoist || config.shamefully_hoist {
        for (name, package_specifier) in
            select_hoisted_packages(packages, config.dedupe_peer_dependents, &config.hoist_pattern)
        {
            if direct_dependency_names.contains(&name) {
                continue;
            }
            hoisted
                .entry(package_specifier.to_string())
                .or_default()
                .insert(name, "private".to_string());
        }
    }

    if !config.public_hoist_pattern.is_empty() {
        for (name, package_specifier) in select_hoisted_packages(
            packages,
            config.dedupe_peer_dependents,
            &config.public_hoist_pattern,
        ) {
            if direct_dependency_names.contains(&name) {
                continue;
            }
            hoisted
                .entry(package_specifier.to_string())
                .or_default()
                .insert(name, "public".to_string());
        }
    }

    (!hoisted.is_empty()).then_some(hoisted)
}

fn relative_virtual_store_dir(modules_dir: &Path, virtual_store_dir: &Path) -> String {
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
        assert!(content.contains("skipped: []"));

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
        assert!(content.contains("skipped:"));
        assert!(content.contains("- a@1.0.0"));
        assert!(content.contains("- b@1.0.0"));
        assert!(content.contains("layoutVersion: 5"));
        assert!(content.contains("nodeLinker: isolated"));
        assert!(content.contains("packageManager: pacquet@"));
        assert!(content.contains("injectedDeps: {}"));
        assert!(content.contains("virtualStoreDir: .pnpm"));
        assert!(content.contains("dependencies: true"));
        assert!(content.contains("optionalDependencies: true"));
    }

    #[test]
    fn detect_package_manager_is_pacquet_and_does_not_shell_out() {
        assert_eq!(detect_pnpm_package_manager(), format!("pacquet@{}", env!("CARGO_PKG_VERSION")));
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
        assert!(content.contains("dependencies: false"));
        assert!(content.contains("devDependencies: true"));
        assert!(content.contains("optionalDependencies: false"));
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
        assert!(content.contains("hoistPattern:"));
        assert!(content.contains("- '*eslint*'"));
        assert!(content.contains("publicHoistPattern:"));
        assert!(content.contains("- '*'"));
        assert!(content.contains("- '!typescript'"));
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
        assert!(content.contains("registries:"));
        assert!(content.contains("default: https://default.example/"));
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
        assert!(content.contains("hoistedDependencies:"));
        assert!(content.contains("'@scope/pkg@1.0.0':"));
        assert!(content.contains("'@scope/pkg': private"));
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
        assert!(content.contains("virtualStoreDirMaxLength: 120"));
        assert!(content.contains("pendingBuilds: []"));
        assert!(!content.contains("ignoredBuilds"));
        assert!(content.contains("storeDir:"));
        assert!(
            PathBuf::from(manifest.store_dir.expect("store dir")).ends_with(Path::new("store/v10"))
        );
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
}
