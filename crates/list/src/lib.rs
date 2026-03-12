mod why;

pub use why::{WhyOptions, WhyReportAs, render_why};

use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{
    DependencyPath, Lockfile, PackageSnapshot, PackageSnapshotDependency, PkgName, PkgNameVerPeer,
    PkgVerPeer, ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencySpec,
    ResolvedDependencyVersion, RootProjectSnapshot,
};
use pacquet_package_manifest::PackageManifest;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy)]
pub struct IncludedDependencies {
    pub dependencies: bool,
    pub dev_dependencies: bool,
    pub optional_dependencies: bool,
}

#[derive(Clone, Copy)]
pub struct ListJsonOptions<'a> {
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub lockfile_importer_id: &'a str,
    pub project_dir: &'a Path,
    pub modules_dir: &'a Path,
    pub registry: &'a str,
    pub include: IncludedDependencies,
    pub depth: i32,
    pub long: bool,
}

struct DependencyView {
    from: String,
    version: String,
    lookup_name: Option<String>,
    lookup_full_version: Option<String>,
    lookup_version_without_peers: Option<String>,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ParseableNode {
    alias: String,
    from: String,
    version: String,
    path: String,
}

#[derive(Clone)]
enum SnapshotDependencyRef {
    PkgVerPeer(PkgVerPeer),
    PkgNameVerPeer(PkgNameVerPeer),
    DependencyPath(DependencyPath),
    Raw(String),
}

impl From<PackageSnapshotDependency> for SnapshotDependencyRef {
    fn from(value: PackageSnapshotDependency) -> Self {
        match value {
            PackageSnapshotDependency::PkgVerPeer(value) => {
                SnapshotDependencyRef::PkgVerPeer(value)
            }
            PackageSnapshotDependency::PkgNameVerPeer(value) => {
                SnapshotDependencyRef::PkgNameVerPeer(value)
            }
            PackageSnapshotDependency::DependencyPath(value) => {
                SnapshotDependencyRef::DependencyPath(value)
            }
            PackageSnapshotDependency::Link(value) => SnapshotDependencyRef::Raw(value),
        }
    }
}

pub fn render_json(opts: ListJsonOptions<'_>) -> miette::Result<String> {
    let root = build_root_json(opts)?;
    serde_json::to_string_pretty(&vec![Value::Object(root)])
        .into_diagnostic()
        .wrap_err("serialize `pacquet ls --json` output")
}

pub fn render_parseable(opts: ListJsonOptions<'_>) -> miette::Result<String> {
    let root = build_root_json(opts)?;
    let root_path = root.get("path").and_then(Value::as_str).unwrap_or_default().to_string();

    let mut lines = Vec::<String>::new();
    if opts.long {
        let mut first_line = root_path;
        if let Some(name) = root.get("name").and_then(Value::as_str) {
            first_line.push(':');
            first_line.push_str(name);
            if let Some(version) = root.get("version").and_then(Value::as_str) {
                first_line.push('@');
                first_line.push_str(version);
            }
            if root.get("private").and_then(Value::as_bool).unwrap_or(false) {
                first_line.push_str(":PRIVATE");
            }
        }
        lines.push(first_line);
    } else {
        lines.push(root_path);
    }

    let mut nodes = Vec::<ParseableNode>::new();
    let mut seen_paths = BTreeSet::<String>::new();
    for group_name in ["optionalDependencies", "dependencies", "devDependencies"] {
        if let Some(group) = root.get(group_name).and_then(Value::as_object) {
            flatten_parseable_group(group, &mut seen_paths, &mut nodes);
        }
    }
    nodes.sort_by(|left, right| left.from.cmp(&right.from));

    for node in nodes {
        if opts.long {
            if node.alias != node.from {
                if node.version.contains('@') {
                    lines.push(format!("{}:{} {}", node.path, node.alias, node.version));
                } else {
                    lines.push(format!(
                        "{}:{} npm:{}@{}",
                        node.path, node.alias, node.from, node.version
                    ));
                }
            } else if node.version.contains('@') {
                lines.push(format!("{}:{}", node.path, node.version));
            } else {
                lines.push(format!("{}:{}@{}", node.path, node.from, node.version));
            }
        } else {
            lines.push(node.path);
        }
    }

    Ok(lines.join("\n"))
}

pub fn render_tree(opts: ListJsonOptions<'_>) -> miette::Result<String> {
    if opts.depth != 0 {
        miette::bail!("Only --depth=0 is currently implemented for `pacquet ls` tree output");
    }

    let root = build_root_json(opts)?;
    let mut out = String::new();
    out.push_str("Legend: production dependency, optional only, dev only\n\n");

    let root_name = root.get("name").and_then(Value::as_str).unwrap_or_default();
    let root_version = root.get("version").and_then(Value::as_str).unwrap_or_default();
    let root_path = root.get("path").and_then(Value::as_str).unwrap_or_default();
    if root_name.is_empty() {
        out.push_str(root_path);
    } else if root_version.is_empty() {
        out.push_str(&format!("{root_name} {root_path}"));
    } else {
        out.push_str(&format!("{root_name}@{root_version} {root_path}"));
    }
    if root.get("private").and_then(Value::as_bool).unwrap_or(false) {
        out.push_str(" (PRIVATE)");
    }

    let groups = ["dependencies", "devDependencies", "optionalDependencies"]
        .into_iter()
        .filter_map(|group_name| {
            root.get(group_name)
                .and_then(Value::as_object)
                .map(|group| (group_name, group))
                .filter(|(_, group)| !group.is_empty())
        })
        .collect::<Vec<_>>();

    if groups.is_empty() {
        return Ok(out);
    }

    out.push_str("\n\n");
    for (group_index, (group_name, group)) in groups.iter().enumerate() {
        out.push_str(&format!("{group_name}:\n"));
        let mut aliases = group.keys().cloned().collect::<Vec<_>>();
        aliases.sort();
        for alias in aliases.iter() {
            out.push_str(&plain_label(alias, group.get(alias).and_then(Value::as_object)));
            out.push('\n');
        }
        if group_index + 1 != groups.len() {
            out.push('\n');
        }
    }

    Ok(out)
}

fn plain_label(alias: &str, dep: Option<&Map<String, Value>>) -> String {
    let Some(dep) = dep else {
        return alias.to_string();
    };
    let from = dep.get("from").and_then(Value::as_str).unwrap_or(alias);
    let version = dep.get("version").and_then(Value::as_str).unwrap_or_default();
    if alias != from {
        if version.contains('@') {
            format!("{alias} {version}")
        } else {
            format!("{alias} npm:{from}@{version}")
        }
    } else if version.contains('@') {
        format!("{alias} {version}")
    } else {
        format!("{from} {version}")
    }
}

fn flatten_parseable_group(
    group: &Map<String, Value>,
    seen_paths: &mut BTreeSet<String>,
    nodes: &mut Vec<ParseableNode>,
) {
    for (alias, value) in group {
        let Some(dep) = value.as_object() else {
            continue;
        };
        let Some(path) = dep.get("path").and_then(Value::as_str) else {
            continue;
        };
        if seen_paths.insert(path.to_string()) {
            nodes.push(ParseableNode {
                alias: alias.to_string(),
                from: dep.get("from").and_then(Value::as_str).unwrap_or(alias).to_string(),
                version: dep.get("version").and_then(Value::as_str).unwrap_or_default().to_string(),
                path: path.to_string(),
            });
        }
        if let Some(nested) = dep.get("dependencies").and_then(Value::as_object) {
            flatten_parseable_group(nested, seen_paths, nodes);
        }
    }
}

fn build_root_json(opts: ListJsonOptions<'_>) -> miette::Result<Map<String, Value>> {
    let mut root = Map::new();
    if let Some(name) = opts.manifest.value().get("name").and_then(Value::as_str) {
        root.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(version) = opts.manifest.value().get("version").and_then(Value::as_str) {
        root.insert("version".to_string(), Value::String(version.to_string()));
    }
    root.insert(
        "path".to_string(),
        Value::String(opts.project_dir.as_os_str().to_string_lossy().into_owned()),
    );
    root.insert(
        "private".to_string(),
        Value::Bool(opts.manifest.value().get("private").and_then(Value::as_bool).unwrap_or(false)),
    );

    if opts.depth >= 0 {
        let importer = importer_snapshot(opts.lockfile, opts.lockfile_importer_id);

        if opts.include.dependencies {
            let deps = group_to_json(
                "dependencies",
                importer.and_then(|snapshot| snapshot.dependencies.as_ref()),
                opts,
            )?;
            if !deps.is_empty() {
                root.insert("dependencies".to_string(), Value::Object(deps));
            }
        }
        if opts.include.dev_dependencies {
            let deps = group_to_json(
                "devDependencies",
                importer.and_then(|snapshot| snapshot.dev_dependencies.as_ref()),
                opts,
            )?;
            if !deps.is_empty() {
                root.insert("devDependencies".to_string(), Value::Object(deps));
            }
        }
        if opts.include.optional_dependencies {
            let deps = group_to_json(
                "optionalDependencies",
                importer.and_then(|snapshot| snapshot.optional_dependencies.as_ref()),
                opts,
            )?;
            if !deps.is_empty() {
                root.insert("optionalDependencies".to_string(), Value::Object(deps));
            }
        }
    }

    Ok(root)
}

fn importer_snapshot<'a>(
    lockfile: Option<&'a Lockfile>,
    importer_id: &str,
) -> Option<&'a ProjectSnapshot> {
    let lockfile = lockfile?;
    match &lockfile.project_snapshot {
        RootProjectSnapshot::Single(snapshot) => (importer_id == ".").then_some(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(importer_id),
    }
}

fn group_to_json(
    group_name: &str,
    resolved_map: Option<&ResolvedDependencyMap>,
    opts: ListJsonOptions<'_>,
) -> miette::Result<Map<String, Value>> {
    if let Some(resolved_map) = resolved_map {
        return resolved_group_to_json(resolved_map, opts, opts.depth, &mut Vec::new());
    }
    manifest_group_to_json(group_name, opts)
}

fn resolved_group_to_json(
    resolved_map: &ResolvedDependencyMap,
    opts: ListJsonOptions<'_>,
    depth_left: i32,
    trail: &mut Vec<String>,
) -> miette::Result<Map<String, Value>> {
    let mut entries =
        resolved_map.iter().map(|(alias, spec)| (alias.to_string(), spec)).collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut result = Map::new();
    for (alias, spec) in entries {
        let dep = resolved_dependency_to_json(&alias, spec, opts, depth_left, trail)?;
        result.insert(alias, Value::Object(dep));
    }
    Ok(result)
}

fn manifest_group_to_json(
    group_name: &str,
    opts: ListJsonOptions<'_>,
) -> miette::Result<Map<String, Value>> {
    let Some(group) = opts.manifest.value().get(group_name).and_then(Value::as_object) else {
        return Ok(Map::new());
    };

    let mut entries = group
        .iter()
        .filter_map(|(name, version)| {
            version.as_str().map(|version| (name.clone(), version.to_string()))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut result = Map::new();
    for (alias, version) in entries {
        let mut dep = Map::new();
        dep.insert("from".to_string(), Value::String(alias.clone()));
        dep.insert("version".to_string(), Value::String(version));
        let path = package_path_direct(opts.modules_dir, &alias);
        if opts.long {
            insert_long_manifest_fields(&mut dep, path.as_path());
        }
        dep.insert(
            "path".to_string(),
            Value::String(path.as_os_str().to_string_lossy().into_owned()),
        );
        result.insert(alias, Value::Object(dep));
    }
    Ok(result)
}

fn resolved_dependency_to_json(
    alias: &str,
    spec: &ResolvedDependencySpec,
    opts: ListJsonOptions<'_>,
    depth_left: i32,
    trail: &mut Vec<String>,
) -> miette::Result<Map<String, Value>> {
    let mut dep = Map::new();
    let view = dependency_view_from_resolved(alias, spec, opts.modules_dir);

    dep.insert("from".to_string(), Value::String(view.from.clone()));
    dep.insert("version".to_string(), Value::String(view.version.clone()));

    if let Some(resolved) = resolved_from_view(opts, &view) {
        dep.insert("resolved".to_string(), Value::String(resolved));
    }

    if opts.long {
        insert_long_manifest_fields(&mut dep, view.path.as_path());
    }
    dep.insert(
        "path".to_string(),
        Value::String(view.path.as_os_str().to_string_lossy().into_owned()),
    );

    if depth_left > 0
        && let Some(snapshot) = snapshot_from_view(opts.lockfile, &view)
    {
        let stack_key = format!("{}@{}", view.from, view.version);
        if !trail.contains(&stack_key) {
            trail.push(stack_key);
            let nested = snapshot_dependencies_to_json(snapshot, opts, depth_left - 1, trail)?;
            trail.pop();
            if !nested.is_empty() {
                dep.insert("dependencies".to_string(), Value::Object(nested));
            }
        }
    }

    Ok(dep)
}

fn snapshot_dependencies_to_json(
    snapshot: &PackageSnapshot,
    opts: ListJsonOptions<'_>,
    depth_left: i32,
    trail: &mut Vec<String>,
) -> miette::Result<Map<String, Value>> {
    let mut entries = BTreeMap::<String, SnapshotDependencyRef>::new();

    if let Some(dependencies) = snapshot.dependencies.as_ref() {
        for (name, dependency) in dependencies {
            entries.insert(name.to_string(), dependency.clone().into());
        }
    }

    if opts.include.optional_dependencies
        && let Some(optional_dependencies) = snapshot.optional_dependencies.as_ref()
    {
        for (name, dependency) in optional_dependencies {
            entries
                .entry(name.clone())
                .or_insert_with(|| parse_snapshot_dependency_string(dependency));
        }
    }

    let mut result = Map::new();
    for (alias, dependency) in entries {
        let dep = snapshot_dependency_to_json(&alias, &dependency, opts, depth_left, trail)?;
        result.insert(alias, Value::Object(dep));
    }
    Ok(result)
}

fn snapshot_dependency_to_json(
    alias: &str,
    dependency: &SnapshotDependencyRef,
    opts: ListJsonOptions<'_>,
    depth_left: i32,
    trail: &mut Vec<String>,
) -> miette::Result<Map<String, Value>> {
    let mut dep = Map::new();
    let view = dependency_view_from_snapshot(alias, dependency, opts.modules_dir);

    dep.insert("from".to_string(), Value::String(view.from.clone()));
    dep.insert("version".to_string(), Value::String(view.version.clone()));

    if let Some(resolved) = resolved_from_view(opts, &view) {
        dep.insert("resolved".to_string(), Value::String(resolved));
    }

    if opts.long {
        insert_long_manifest_fields(&mut dep, view.path.as_path());
    }
    dep.insert(
        "path".to_string(),
        Value::String(view.path.as_os_str().to_string_lossy().into_owned()),
    );

    if depth_left > 0
        && let Some(snapshot) = snapshot_from_view(opts.lockfile, &view)
    {
        let stack_key = format!("{}@{}", view.from, view.version);
        if !trail.contains(&stack_key) {
            trail.push(stack_key);
            let nested = snapshot_dependencies_to_json(snapshot, opts, depth_left - 1, trail)?;
            trail.pop();
            if !nested.is_empty() {
                dep.insert("dependencies".to_string(), Value::Object(nested));
            }
        }
    }

    Ok(dep)
}

fn dependency_view_from_resolved(
    alias: &str,
    spec: &ResolvedDependencySpec,
    modules_dir: &Path,
) -> DependencyView {
    match &spec.version {
        ResolvedDependencyVersion::PkgVerPeer(version) => {
            let from = alias.to_string();
            let full_version = version.to_string();
            let path = package_path_direct(modules_dir, alias);
            DependencyView {
                from: from.clone(),
                version: full_version.clone(),
                lookup_name: Some(from),
                lookup_full_version: Some(full_version),
                lookup_version_without_peers: Some(version.version().to_string()),
                path,
            }
        }
        ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => {
            let from = name_ver_peer.name.to_string();
            let full_version = name_ver_peer.suffix.to_string();
            let path = package_path_direct(modules_dir, alias);
            DependencyView {
                from: from.clone(),
                version: full_version.clone(),
                lookup_name: Some(from),
                lookup_full_version: Some(full_version),
                lookup_version_without_peers: Some(name_ver_peer.suffix.version().to_string()),
                path,
            }
        }
        ResolvedDependencyVersion::Link(link) => DependencyView {
            from: alias.to_string(),
            version: link.to_string(),
            lookup_name: None,
            lookup_full_version: None,
            lookup_version_without_peers: None,
            path: package_path_direct(modules_dir, alias),
        },
    }
}

fn dependency_view_from_snapshot(
    alias: &str,
    dependency: &SnapshotDependencyRef,
    modules_dir: &Path,
) -> DependencyView {
    match dependency {
        SnapshotDependencyRef::PkgVerPeer(version) => {
            let from = alias.to_string();
            let full_version = version.to_string();
            let path = package_path_in_virtual_store(modules_dir, &from, &full_version)
                .unwrap_or_else(|| package_path_direct(modules_dir, alias));
            DependencyView {
                from: from.clone(),
                version: full_version.clone(),
                lookup_name: Some(from),
                lookup_full_version: Some(full_version),
                lookup_version_without_peers: Some(version.version().to_string()),
                path,
            }
        }
        SnapshotDependencyRef::PkgNameVerPeer(name_ver_peer) => {
            let from = name_ver_peer.name.to_string();
            let full_version = name_ver_peer.suffix.to_string();
            let path = package_path_in_virtual_store(modules_dir, &from, &full_version)
                .unwrap_or_else(|| package_path_direct(modules_dir, &from));
            DependencyView {
                from: from.clone(),
                version: full_version.clone(),
                lookup_name: Some(from),
                lookup_full_version: Some(full_version),
                lookup_version_without_peers: Some(name_ver_peer.suffix.version().to_string()),
                path,
            }
        }
        SnapshotDependencyRef::DependencyPath(dependency_path) => {
            let from = dependency_path.package_name().to_string();
            let full_version = dependency_path
                .local_file_reference()
                .map(ToString::to_string)
                .or_else(|| {
                    dependency_path
                        .package_specifier
                        .registry_specifier()
                        .map(|specifier| specifier.suffix.to_string())
                })
                .unwrap_or_default();
            let path = package_path_in_virtual_store(modules_dir, &from, &full_version)
                .unwrap_or_else(|| package_path_direct(modules_dir, &from));
            DependencyView {
                from: from.clone(),
                version: full_version.clone(),
                lookup_name: Some(from),
                lookup_full_version: Some(full_version),
                lookup_version_without_peers: dependency_path
                    .package_specifier
                    .registry_specifier()
                    .map(|specifier| specifier.suffix.version().to_string()),
                path,
            }
        }
        SnapshotDependencyRef::Raw(value) => DependencyView {
            from: alias.to_string(),
            version: value.to_string(),
            lookup_name: None,
            lookup_full_version: None,
            lookup_version_without_peers: None,
            path: package_path_direct(modules_dir, alias),
        },
    }
}

fn parse_snapshot_dependency_string(value: &str) -> SnapshotDependencyRef {
    if let Ok(dependency_path) = value.parse::<DependencyPath>() {
        return SnapshotDependencyRef::DependencyPath(dependency_path);
    }
    if let Ok(name_ver_peer) = value.parse::<PkgNameVerPeer>() {
        return SnapshotDependencyRef::PkgNameVerPeer(name_ver_peer);
    }
    if let Ok(ver_peer) = value.parse::<PkgVerPeer>() {
        return SnapshotDependencyRef::PkgVerPeer(ver_peer);
    }
    SnapshotDependencyRef::Raw(value.to_string())
}

fn snapshot_from_view<'a>(
    lockfile: Option<&'a Lockfile>,
    view: &DependencyView,
) -> Option<&'a PackageSnapshot> {
    let (Some(name), Some(full_version), Some(version_without_peers)) = (
        view.lookup_name.as_deref(),
        view.lookup_full_version.as_deref(),
        view.lookup_version_without_peers.as_deref(),
    ) else {
        return None;
    };
    find_package_snapshot(lockfile, name, full_version, version_without_peers)
}

fn resolved_from_view(opts: ListJsonOptions<'_>, view: &DependencyView) -> Option<String> {
    let (Some(name), Some(full_version), Some(version_without_peers)) = (
        view.lookup_name.as_deref(),
        view.lookup_full_version.as_deref(),
        view.lookup_version_without_peers.as_deref(),
    ) else {
        return None;
    };

    let snapshot = find_package_snapshot(opts.lockfile, name, full_version, version_without_peers)?;
    resolved_field(snapshot, name, version_without_peers, opts.registry)
}

fn find_package_snapshot<'a>(
    lockfile: Option<&'a Lockfile>,
    package_name: &str,
    full_version: &str,
    version_without_peers: &str,
) -> Option<&'a PackageSnapshot> {
    lockfile.and_then(|lockfile| lockfile.packages.as_ref()).and_then(|packages| {
        packages
            .iter()
            .find(|(dep_path, _)| {
                dep_path.package_name().to_string() == package_name
                    && dep_path
                        .package_specifier
                        .registry_specifier()
                        .is_some_and(|specifier| specifier.suffix.to_string() == full_version)
            })
            .or_else(|| {
                packages.iter().find(|(dep_path, _)| {
                    dep_path.package_name().to_string() == package_name
                        && dep_path.package_specifier.registry_specifier().is_some_and(
                            |specifier| {
                                specifier.suffix.version().to_string() == version_without_peers
                            },
                        )
                })
            })
            .map(|(_, snapshot)| snapshot)
    })
}

fn resolved_field(
    snapshot: &PackageSnapshot,
    package_name: &str,
    version_without_peers: &str,
    registry: &str,
) -> Option<String> {
    match &snapshot.resolution {
        pacquet_lockfile::LockfileResolution::Tarball(resolution) => {
            Some(resolution.tarball.clone())
        }
        pacquet_lockfile::LockfileResolution::Registry(_) => {
            Some(default_tarball_url(registry, package_name, version_without_peers))
        }
        pacquet_lockfile::LockfileResolution::Directory(resolution) => {
            Some(resolution.directory.clone())
        }
        pacquet_lockfile::LockfileResolution::Git(resolution) => {
            Some(format!("{}#{}", resolution.repo, resolution.commit))
        }
    }
}

fn default_tarball_url(registry: &str, package_name: &str, version: &str) -> String {
    let registry =
        if registry.ends_with('/') { registry.to_string() } else { format!("{registry}/") };
    let bare = package_name.rsplit('/').next().unwrap_or(package_name);
    format!("{registry}{package_name}/-/{bare}-{version}.tgz")
}

fn package_path_direct(modules_dir: &Path, alias: &str) -> PathBuf {
    let path = modules_dir.join(alias);
    fs::canonicalize(&path).unwrap_or(path)
}

fn package_path_in_virtual_store(
    modules_dir: &Path,
    package_name: &str,
    full_version: &str,
) -> Option<PathBuf> {
    let name: PkgName = package_name.parse().ok()?;
    let version: PkgVerPeer = full_version.parse().ok()?;
    let virtual_store_name = PkgNameVerPeer::new(name, version).to_virtual_store_name();
    let path =
        modules_dir.join(".pnpm").join(virtual_store_name).join("node_modules").join(package_name);
    Some(fs::canonicalize(&path).unwrap_or(path))
}

fn insert_long_manifest_fields(dep: &mut Map<String, Value>, package_dir: &Path) {
    let manifest = read_package_json(package_dir.join("package.json"));
    let Some(manifest) = manifest else {
        return;
    };

    if let Some(description) = manifest.get("description").and_then(Value::as_str) {
        dep.insert("description".to_string(), Value::String(description.to_string()));
    }
    if let Some(license) = manifest.get("license").and_then(Value::as_str) {
        dep.insert("license".to_string(), Value::String(license.to_string()));
    }
    if let Some(author) = manifest.get("author").and_then(normalize_author_field) {
        dep.insert("author".to_string(), author);
    }

    let mut normalized_homepage =
        manifest.get("homepage").and_then(Value::as_str).map(ToOwned::to_owned);
    if let Some(repository) = manifest.get("repository").and_then(repository_url) {
        let normalized = normalize_repository_field(repository);
        dep.insert("repository".to_string(), Value::String(normalized.repository));
        if normalized_homepage.is_none() {
            normalized_homepage = normalized.inferred_homepage;
        }
    }
    if let Some(homepage) = normalized_homepage {
        dep.insert("homepage".to_string(), Value::String(homepage));
    }
}

fn repository_url(repository: &Value) -> Option<&str> {
    repository.as_str().or_else(|| {
        repository.as_object().and_then(|repository| repository.get("url")).and_then(Value::as_str)
    })
}

fn normalize_author_field(author: &Value) -> Option<Value> {
    match author {
        Value::Object(_) => Some(author.clone()),
        Value::String(author) => {
            let parsed = parse_author_string(author);
            if parsed.is_empty() {
                Some(Value::String(author.to_string()))
            } else {
                Some(Value::Object(parsed))
            }
        }
        _ => None,
    }
}

fn parse_author_string(author: &str) -> Map<String, Value> {
    let mut value = Map::<String, Value>::new();
    let author = author.trim();
    if author.is_empty() {
        return value;
    }

    let name_end = author.find(['<', '(']).unwrap_or(author.len());
    let name = author[..name_end].trim();
    if !name.is_empty() {
        value.insert("name".to_string(), Value::String(name.to_string()));
    }

    if let Some(start) = author.find('<')
        && let Some(end) = author[start + 1..].find('>')
    {
        let email = author[start + 1..start + 1 + end].trim();
        if !email.is_empty() {
            value.insert("email".to_string(), Value::String(email.to_string()));
        }
    }
    if let Some(start) = author.find('(')
        && let Some(end) = author[start + 1..].find(')')
    {
        let url = author[start + 1..start + 1 + end].trim();
        if !url.is_empty() {
            value.insert("url".to_string(), Value::String(url.to_string()));
        }
    }

    value
}

struct NormalizedRepository {
    repository: String,
    inferred_homepage: Option<String>,
}

fn normalize_repository_field(repository: &str) -> NormalizedRepository {
    if let Some(rest) = repository.strip_prefix("https://github.com/") {
        let mut segments = rest.split('/');
        let owner = segments.next().unwrap_or_default();
        let repo = segments.next().unwrap_or_default();
        let tree_literal = segments.next().unwrap_or_default();
        let branch = segments.next().unwrap_or_default();
        if !owner.is_empty() && !repo.is_empty() && tree_literal == "tree" && !branch.is_empty() {
            let repo = repo.trim_end_matches(".git");
            return NormalizedRepository {
                repository: format!("git+https://github.com/{owner}/{repo}.git#{branch}"),
                inferred_homepage: Some(format!(
                    "https://github.com/{owner}/{repo}/tree/{branch}#readme"
                )),
            };
        }
    }

    NormalizedRepository { repository: repository.to_string(), inferred_homepage: None }
}

fn read_package_json(path: PathBuf) -> Option<Map<String, Value>> {
    let value: Value =
        fs::read_to_string(path).ok().and_then(|content| serde_json::from_str(&content).ok())?;
    value.as_object().cloned()
}
