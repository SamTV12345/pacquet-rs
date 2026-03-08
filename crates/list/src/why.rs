use crate::IncludedDependencies;
use glob::Pattern;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{
    DependencyPath, Lockfile, PackageSnapshotDependency, PkgName, PkgNameVerPeer, PkgVerPeer,
    ResolvedDependencySpec, ResolvedDependencyVersion, RootProjectSnapshot,
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::Path,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhyReportAs {
    Json,
    Parseable,
    Tree,
}

#[derive(Clone, Copy)]
pub struct WhyOptions<'a> {
    pub lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub root_importer_id: &'a str,
    pub modules_dir: &'a Path,
    pub include: IncludedDependencies,
    pub package_queries: &'a [String],
    pub depth: Option<usize>,
    pub long: bool,
}

#[derive(Clone, Debug)]
struct WhyTree {
    name: String,
    version: String,
    path: Option<String>,
    dependents: Vec<WhyDependent>,
}

#[derive(Clone, Debug)]
struct WhyDependent {
    name: String,
    version: String,
    dep_field: Option<String>,
    dependents: Vec<WhyDependent>,
    circular: bool,
    deduped: bool,
}

#[derive(Clone, Debug)]
struct ReverseEdge {
    parent_key: String,
    alias: String,
}

#[derive(Clone, Debug)]
struct ImporterNode {
    name: String,
    version: String,
    dependency_aliases: HashSet<String>,
    dev_dependency_aliases: HashSet<String>,
    optional_dependency_aliases: HashSet<String>,
}

impl ImporterNode {
    fn dep_field_for_alias(&self, alias: &str) -> Option<&'static str> {
        if self.dev_dependency_aliases.contains(alias) {
            return Some("devDependencies");
        }
        if self.optional_dependency_aliases.contains(alias) {
            return Some("optionalDependencies");
        }
        if self.dependency_aliases.contains(alias) {
            return Some("dependencies");
        }
        None
    }
}

#[derive(Clone, Debug)]
struct PackageNode {
    name: String,
    version: String,
    full_version: String,
}

#[derive(Clone, Debug)]
enum Node {
    Importer(ImporterNode),
    Package(PackageNode),
}

#[derive(Default)]
struct PackageLookups {
    by_name_full_version: HashMap<(String, String), Vec<String>>,
    by_name_version: HashMap<(String, String), Vec<String>>,
}

impl PackageLookups {
    fn insert(
        &mut self,
        package_name: String,
        full_version: String,
        version: String,
        package_key: String,
    ) {
        self.by_name_full_version
            .entry((package_name.clone(), full_version))
            .or_default()
            .push(package_key.clone());
        self.by_name_version.entry((package_name, version)).or_default().push(package_key);
    }

    fn finalize(&mut self) {
        for values in self.by_name_full_version.values_mut() {
            values.sort();
        }
        for values in self.by_name_version.values_mut() {
            values.sort();
        }
    }

    fn resolve(
        &self,
        package_name: &str,
        full_version: &str,
        version: Option<&str>,
    ) -> Option<String> {
        self.by_name_full_version
            .get(&(package_name.to_string(), full_version.to_string()))
            .and_then(|values| values.first())
            .cloned()
            .or_else(|| {
                version.and_then(|version| {
                    self.by_name_version
                        .get(&(package_name.to_string(), version.to_string()))
                        .and_then(|values| values.first())
                        .cloned()
                })
            })
    }
}

pub fn render_why(opts: WhyOptions<'_>, report_as: WhyReportAs) -> miette::Result<String> {
    if opts.package_queries.is_empty() {
        miette::bail!("`pacquet why` requires the package name");
    }

    let Some(lockfile) = opts.lockfile else {
        return Ok(String::new());
    };
    let Some(packages) = lockfile.packages.as_ref() else {
        return Ok(String::new());
    };
    if packages.is_empty() {
        return Ok(String::new());
    }

    let query_patterns = compile_query_patterns(opts.package_queries);
    let mut nodes = HashMap::<String, Node>::new();
    let mut reverse_edges = HashMap::<String, Vec<ReverseEdge>>::new();
    let mut forward_edges = HashMap::<String, Vec<String>>::new();
    let mut package_lookups = PackageLookups::default();

    for (dep_path, snapshot) in packages {
        let package_key = package_key(dep_path);
        let package_name =
            snapshot.name.clone().unwrap_or_else(|| dep_path.package_specifier.name.to_string());
        let full_version = dep_path.package_specifier.suffix.to_string();
        let version = snapshot
            .version
            .clone()
            .unwrap_or_else(|| dep_path.package_specifier.suffix.version().to_string());

        package_lookups.insert(
            package_name.clone(),
            full_version.clone(),
            version.clone(),
            package_key.clone(),
        );
        nodes.insert(
            package_key,
            Node::Package(PackageNode { name: package_name, version, full_version }),
        );
    }
    package_lookups.finalize();

    let mut importer_ids = HashSet::<String>::new();
    let mut importer_snapshots = Vec::new();
    match &lockfile.project_snapshot {
        RootProjectSnapshot::Single(snapshot) => {
            importer_ids.insert(".".to_string());
            importer_snapshots.push((".".to_string(), snapshot));
        }
        RootProjectSnapshot::Multi(snapshot) => {
            for (importer_id, project_snapshot) in &snapshot.importers {
                importer_ids.insert(importer_id.clone());
                importer_snapshots.push((importer_id.clone(), project_snapshot));
            }
        }
    }

    for (importer_id, snapshot) in &importer_snapshots {
        let (name, version) = read_importer_info(opts.lockfile_dir, importer_id);
        let regular_dependency_aliases = dependency_aliases(snapshot.dependencies.as_ref());
        let dev_dependency_aliases = dependency_aliases(snapshot.dev_dependencies.as_ref());
        let optional_dependency_aliases =
            optional_dependency_aliases(snapshot.optional_dependencies.as_ref());
        nodes.insert(
            importer_key(importer_id),
            Node::Importer(ImporterNode {
                name,
                version,
                dependency_aliases: regular_dependency_aliases,
                dev_dependency_aliases,
                optional_dependency_aliases,
            }),
        );
    }

    for (importer_id, snapshot) in &importer_snapshots {
        let parent_key = importer_key(importer_id);

        if opts.include.dependencies
            && let Some(dependencies) = snapshot.dependencies.as_ref()
        {
            for (alias, spec) in dependencies {
                if let Some(child_key) = resolve_importer_dependency(
                    importer_id,
                    &alias.to_string(),
                    spec,
                    &package_lookups,
                    opts.lockfile_dir,
                    &importer_ids,
                ) {
                    push_reverse_edge(
                        &mut reverse_edges,
                        &mut forward_edges,
                        child_key,
                        &parent_key,
                        &alias.to_string(),
                    );
                }
            }
        }

        if opts.include.dev_dependencies
            && let Some(dependencies) = snapshot.dev_dependencies.as_ref()
        {
            for (alias, spec) in dependencies {
                if let Some(child_key) = resolve_importer_dependency(
                    importer_id,
                    &alias.to_string(),
                    spec,
                    &package_lookups,
                    opts.lockfile_dir,
                    &importer_ids,
                ) {
                    push_reverse_edge(
                        &mut reverse_edges,
                        &mut forward_edges,
                        child_key,
                        &parent_key,
                        &alias.to_string(),
                    );
                }
            }
        }

        if opts.include.optional_dependencies
            && let Some(dependencies) = snapshot.optional_dependencies.as_ref()
        {
            for (alias, spec) in dependencies {
                if let Some(child_key) = resolve_importer_dependency(
                    importer_id,
                    &alias.to_string(),
                    spec,
                    &package_lookups,
                    opts.lockfile_dir,
                    &importer_ids,
                ) {
                    push_reverse_edge(
                        &mut reverse_edges,
                        &mut forward_edges,
                        child_key,
                        &parent_key,
                        &alias.to_string(),
                    );
                }
            }
        }
    }

    for (dep_path, snapshot) in packages {
        let parent_key = package_key(dep_path);

        if let Some(dependencies) = snapshot.dependencies.as_ref() {
            for (alias, dependency) in dependencies {
                if let Some(child_key) =
                    resolve_snapshot_dependency(&alias.to_string(), dependency, &package_lookups)
                {
                    push_reverse_edge(
                        &mut reverse_edges,
                        &mut forward_edges,
                        child_key,
                        &parent_key,
                        &alias.to_string(),
                    );
                }
            }
        }

        if opts.include.optional_dependencies
            && let Some(optional_dependencies) = snapshot.optional_dependencies.as_ref()
        {
            for (alias, dependency) in optional_dependencies {
                if let Some(child_key) =
                    resolve_optional_snapshot_dependency(alias, dependency, &package_lookups)
                {
                    push_reverse_edge(
                        &mut reverse_edges,
                        &mut forward_edges,
                        child_key,
                        &parent_key,
                        alias,
                    );
                }
            }
        }
    }

    let reachable_nodes = compute_reachable_nodes(opts.root_importer_id, &nodes, &forward_edges);
    if let Some(reachable_nodes) = &reachable_nodes {
        reverse_edges.retain(|child_key, parents| {
            if !reachable_nodes.contains(child_key) {
                return false;
            }
            parents.retain(|edge| reachable_nodes.contains(&edge.parent_key));
            !parents.is_empty()
        });
    }

    let mut package_keys = nodes
        .iter()
        .filter_map(|(key, node)| match node {
            Node::Package(package) => {
                Some((key.clone(), package.name.clone(), package.version.clone()))
            }
            Node::Importer(_) => None,
        })
        .collect::<Vec<_>>();
    package_keys.sort_by(|left, right| {
        left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)).then_with(|| left.0.cmp(&right.0))
    });

    let mut trees = Vec::<WhyTree>::new();
    for (package_key, package_name, _) in package_keys {
        if reachable_nodes
            .as_ref()
            .is_some_and(|reachable_nodes| !reachable_nodes.contains(&package_key))
        {
            continue;
        }
        let incoming_aliases = reverse_edges
            .get(&package_key)
            .map(|edges| edges.iter().map(|edge| edge.alias.clone()).collect::<HashSet<_>>())
            .unwrap_or_default();
        let is_match = query_patterns.iter().any(|query| query.matches(&package_name))
            || incoming_aliases
                .iter()
                .any(|alias| query_patterns.iter().any(|query| query.matches(alias)));
        if !is_match {
            continue;
        }

        let Some(Node::Package(package)) = nodes.get(&package_key) else {
            continue;
        };
        let mut visited = HashSet::<String>::from([package_key.clone()]);
        let mut expanded = HashSet::<String>::new();
        let dependents = walk_reverse_dependents(
            &package_key,
            &nodes,
            &reverse_edges,
            &mut visited,
            &mut expanded,
        );
        let path = opts
            .long
            .then(|| installed_package_path(opts.modules_dir, &package.name, &package.full_version))
            .flatten();
        trees.push(WhyTree {
            name: package.name.clone(),
            version: package.version.clone(),
            path,
            dependents,
        });
    }

    match report_as {
        WhyReportAs::Json => render_json(&trees, opts.depth),
        WhyReportAs::Parseable => Ok(render_parseable(&trees, opts.depth, opts.long)),
        WhyReportAs::Tree => Ok(render_tree(&trees, opts.depth, opts.long)),
    }
}

fn render_json(trees: &[WhyTree], depth: Option<usize>) -> miette::Result<String> {
    let payload = trees
        .iter()
        .map(|tree| {
            let dependents = depth
                .map(|depth| truncate_dependents(&tree.dependents, 0, depth))
                .unwrap_or_else(|| tree.dependents.clone());
            let mut value = Map::new();
            value.insert("name".to_string(), Value::String(tree.name.clone()));
            value.insert("version".to_string(), Value::String(tree.version.clone()));
            if let Some(path) = &tree.path {
                value.insert("path".to_string(), Value::String(path.clone()));
            }
            value.insert(
                "dependents".to_string(),
                Value::Array(dependents.iter().map(dependent_to_json).collect()),
            );
            Value::Object(value)
        })
        .collect::<Vec<_>>();

    serde_json::to_string_pretty(&payload)
        .into_diagnostic()
        .wrap_err("serialize `pacquet why --json`")
}

fn dependent_to_json(dependent: &WhyDependent) -> Value {
    let mut value = Map::new();
    value.insert("name".to_string(), Value::String(dependent.name.clone()));
    value.insert("version".to_string(), Value::String(dependent.version.clone()));
    if let Some(dep_field) = &dependent.dep_field {
        value.insert("depField".to_string(), Value::String(dep_field.clone()));
    }
    if dependent.circular {
        value.insert("circular".to_string(), Value::Bool(true));
    }
    if dependent.deduped {
        value.insert("deduped".to_string(), Value::Bool(true));
    }
    if !dependent.dependents.is_empty() {
        value.insert(
            "dependents".to_string(),
            Value::Array(dependent.dependents.iter().map(dependent_to_json).collect()),
        );
    }
    Value::Object(value)
}

fn render_parseable(trees: &[WhyTree], depth: Option<usize>, long: bool) -> String {
    let mut lines = Vec::<String>::new();

    for tree in trees {
        let root_segment = if long {
            tree.path
                .as_ref()
                .map(|path| format!("{path}:{}", name_at_version(&tree.name, &tree.version)))
                .unwrap_or_else(|| name_at_version(&tree.name, &tree.version))
        } else {
            name_at_version(&tree.name, &tree.version)
        };
        collect_parseable_paths(&tree.dependents, vec![root_segment], &mut lines, 0, depth);
    }

    lines.join("\n")
}

fn collect_parseable_paths(
    dependents: &[WhyDependent],
    current_path: Vec<String>,
    lines: &mut Vec<String>,
    current_depth: usize,
    max_depth: Option<usize>,
) {
    for dependent in dependents {
        let mut new_path = current_path.clone();
        new_path.push(name_at_version(&dependent.name, &dependent.version));
        let at_depth_limit =
            max_depth.map(|max_depth| current_depth + 1 >= max_depth).unwrap_or(false);
        if !dependent.dependents.is_empty() && !at_depth_limit {
            collect_parseable_paths(
                &dependent.dependents,
                new_path,
                lines,
                current_depth + 1,
                max_depth,
            );
        } else {
            let mut reversed = new_path;
            reversed.reverse();
            lines.push(reversed.join(" > "));
        }
    }
}

fn render_tree(trees: &[WhyTree], depth: Option<usize>, long: bool) -> String {
    if trees.is_empty() {
        return String::new();
    }

    let mut output_chunks = Vec::<String>::new();
    for tree in trees {
        let mut chunk = String::new();
        chunk.push_str(&name_at_version(&tree.name, &tree.version));
        if long && let Some(path) = &tree.path {
            chunk.push('\n');
            chunk.push_str(path);
        }
        if !tree.dependents.is_empty() {
            chunk.push('\n');
            render_dependents_tree(&tree.dependents, "", &mut chunk, 0, depth);
            while chunk.ends_with('\n') {
                chunk.pop();
            }
        }
        output_chunks.push(chunk);
    }

    let summary = render_tree_summary(trees);
    if summary.is_empty() {
        output_chunks.join("\n\n")
    } else {
        format!("{}\n\n{summary}", output_chunks.join("\n\n"))
    }
}

fn render_dependents_tree(
    dependents: &[WhyDependent],
    prefix: &str,
    output: &mut String,
    current_depth: usize,
    max_depth: Option<usize>,
) {
    for (index, dependent) in dependents.iter().enumerate() {
        let is_last = index + 1 == dependents.len();
        let branch = if is_last { "└── " } else { "├── " };
        output.push_str(prefix);
        output.push_str(branch);
        output.push_str(&dependent_tree_label(dependent));
        output.push('\n');

        let at_depth_limit =
            max_depth.map(|max_depth| current_depth + 1 >= max_depth).unwrap_or(false);
        if !dependent.dependents.is_empty() && !at_depth_limit {
            let mut next_prefix = prefix.to_string();
            next_prefix.push_str(if is_last { "    " } else { "│   " });
            render_dependents_tree(
                &dependent.dependents,
                &next_prefix,
                output,
                current_depth + 1,
                max_depth,
            );
        }
    }
}

fn render_tree_summary(trees: &[WhyTree]) -> String {
    let mut by_name = BTreeMap::<String, (BTreeSet<String>, usize)>::new();
    for tree in trees {
        let entry = by_name.entry(tree.name.clone()).or_insert_with(|| (BTreeSet::new(), 0));
        entry.0.insert(tree.version.clone());
        entry.1 += 1;
    }

    by_name
        .into_iter()
        .map(|(name, (versions, count))| {
            let mut parts = vec![format!(
                "{} version{}",
                versions.len(),
                if versions.len() == 1 { "" } else { "s" }
            )];
            if count > versions.len() {
                parts.push(format!("{count} instances"));
            }
            format!("Found {} of {name}", parts.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compute_reachable_nodes(
    root_importer_id: &str,
    nodes: &HashMap<String, Node>,
    forward_edges: &HashMap<String, Vec<String>>,
) -> Option<HashSet<String>> {
    let root_key = importer_key(root_importer_id);
    if !nodes.contains_key(&root_key) {
        return None;
    }

    let mut visited = HashSet::<String>::new();
    let mut queue = VecDeque::<String>::new();
    visited.insert(root_key.clone());
    queue.push_back(root_key);

    while let Some(parent_key) = queue.pop_front() {
        let Some(children) = forward_edges.get(&parent_key) else {
            continue;
        };
        for child in children {
            if visited.insert(child.clone()) {
                queue.push_back(child.clone());
            }
        }
    }

    Some(visited)
}

fn truncate_dependents(
    dependents: &[WhyDependent],
    current_depth: usize,
    max_depth: usize,
) -> Vec<WhyDependent> {
    dependents
        .iter()
        .map(|dependent| {
            if !dependent.dependents.is_empty() && current_depth + 1 < max_depth {
                let mut value = dependent.clone();
                value.dependents =
                    truncate_dependents(&dependent.dependents, current_depth + 1, max_depth);
                value
            } else {
                let mut value = dependent.clone();
                value.dependents = Vec::new();
                value
            }
        })
        .collect()
}

fn walk_reverse_dependents(
    node_key: &str,
    nodes: &HashMap<String, Node>,
    reverse_edges: &HashMap<String, Vec<ReverseEdge>>,
    visited: &mut HashSet<String>,
    expanded: &mut HashSet<String>,
) -> Vec<WhyDependent> {
    let Some(edges) = reverse_edges.get(node_key) else {
        return Vec::new();
    };
    if edges.is_empty() {
        return Vec::new();
    }

    let mut sorted_edges = edges.clone();
    sorted_edges.sort_by(|left, right| {
        parent_sort_name(nodes.get(&left.parent_key))
            .cmp(&parent_sort_name(nodes.get(&right.parent_key)))
            .then_with(|| left.parent_key.cmp(&right.parent_key))
    });

    let mut dependents = Vec::<WhyDependent>::new();
    for edge in sorted_edges {
        if visited.contains(&edge.parent_key) {
            if let Some(dependent) = circular_dependent(nodes.get(&edge.parent_key), &edge.alias) {
                dependents.push(dependent);
            }
            continue;
        }

        let Some(parent_node) = nodes.get(&edge.parent_key) else {
            continue;
        };

        match parent_node {
            Node::Importer(importer) => {
                dependents.push(WhyDependent {
                    name: importer.name.clone(),
                    version: importer.version.clone(),
                    dep_field: importer.dep_field_for_alias(&edge.alias).map(str::to_string),
                    dependents: Vec::new(),
                    circular: false,
                    deduped: false,
                });
            }
            Node::Package(package) => {
                if expanded.contains(&edge.parent_key) {
                    dependents.push(WhyDependent {
                        name: package.name.clone(),
                        version: package.version.clone(),
                        dep_field: None,
                        dependents: Vec::new(),
                        circular: false,
                        deduped: true,
                    });
                    continue;
                }

                visited.insert(edge.parent_key.clone());
                expanded.insert(edge.parent_key.clone());
                let nested = walk_reverse_dependents(
                    &edge.parent_key,
                    nodes,
                    reverse_edges,
                    visited,
                    expanded,
                );
                visited.remove(&edge.parent_key);
                dependents.push(WhyDependent {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    dep_field: None,
                    dependents: nested,
                    circular: false,
                    deduped: false,
                });
            }
        }
    }

    dependents
}

fn circular_dependent(parent: Option<&Node>, alias: &str) -> Option<WhyDependent> {
    match parent {
        Some(Node::Importer(importer)) => Some(WhyDependent {
            name: importer.name.clone(),
            version: importer.version.clone(),
            dep_field: importer.dep_field_for_alias(alias).map(str::to_string),
            dependents: Vec::new(),
            circular: true,
            deduped: false,
        }),
        Some(Node::Package(package)) => Some(WhyDependent {
            name: package.name.clone(),
            version: package.version.clone(),
            dep_field: None,
            dependents: Vec::new(),
            circular: true,
            deduped: false,
        }),
        None => None,
    }
}

fn parent_sort_name(parent: Option<&Node>) -> String {
    match parent {
        Some(Node::Importer(importer)) => importer.name.clone(),
        Some(Node::Package(package)) => package.name.clone(),
        None => String::new(),
    }
}

fn dependent_tree_label(dependent: &WhyDependent) -> String {
    let mut label = name_at_version(&dependent.name, &dependent.version);
    if let Some(dep_field) = &dependent.dep_field {
        label.push_str(&format!(" ({dep_field})"));
    }
    if dependent.circular {
        label.push_str(" [circular]");
    }
    if dependent.deduped {
        label.push_str(" [deduped]");
    }
    label
}

fn name_at_version(name: &str, version: &str) -> String {
    if version.is_empty() { name.to_string() } else { format!("{name}@{version}") }
}

fn compile_query_patterns(queries: &[String]) -> Vec<QueryPattern> {
    queries
        .iter()
        .map(|query| match Pattern::new(query) {
            Ok(pattern) => QueryPattern::Glob(pattern),
            Err(_) => QueryPattern::Exact(query.clone()),
        })
        .collect()
}

enum QueryPattern {
    Glob(Pattern),
    Exact(String),
}

impl QueryPattern {
    fn matches(&self, value: &str) -> bool {
        match self {
            QueryPattern::Glob(pattern) => pattern.matches(value),
            QueryPattern::Exact(pattern) => value == pattern,
        }
    }
}

fn dependency_aliases(
    map: Option<&HashMap<pacquet_lockfile::PkgName, ResolvedDependencySpec>>,
) -> HashSet<String> {
    map.into_iter().flatten().map(|(alias, _)| alias.to_string()).collect()
}

fn optional_dependency_aliases(
    map: Option<&HashMap<pacquet_lockfile::PkgName, ResolvedDependencySpec>>,
) -> HashSet<String> {
    map.into_iter().flatten().map(|(alias, _)| alias.to_string()).collect()
}

fn resolve_importer_dependency(
    parent_importer_id: &str,
    alias: &str,
    spec: &ResolvedDependencySpec,
    package_lookups: &PackageLookups,
    lockfile_dir: &Path,
    importer_ids: &HashSet<String>,
) -> Option<String> {
    match &spec.version {
        ResolvedDependencyVersion::PkgVerPeer(version) => package_lookups.resolve(
            alias,
            &version.to_string(),
            Some(&version.version().to_string()),
        ),
        ResolvedDependencyVersion::PkgNameVerPeer(name_ver_peer) => package_lookups.resolve(
            &name_ver_peer.name.to_string(),
            &name_ver_peer.suffix.to_string(),
            Some(&name_ver_peer.suffix.version().to_string()),
        ),
        ResolvedDependencyVersion::Link(link) => {
            resolve_linked_importer(parent_importer_id, link, lockfile_dir, importer_ids)
                .map(|importer_id| importer_key(&importer_id))
        }
    }
}

fn resolve_snapshot_dependency(
    alias: &str,
    dependency: &PackageSnapshotDependency,
    package_lookups: &PackageLookups,
) -> Option<String> {
    match dependency {
        PackageSnapshotDependency::PkgVerPeer(version) => package_lookups.resolve(
            alias,
            &version.to_string(),
            Some(&version.version().to_string()),
        ),
        PackageSnapshotDependency::PkgNameVerPeer(name_ver_peer) => package_lookups.resolve(
            &name_ver_peer.name.to_string(),
            &name_ver_peer.suffix.to_string(),
            Some(&name_ver_peer.suffix.version().to_string()),
        ),
        PackageSnapshotDependency::DependencyPath(dependency_path) => {
            Some(package_key(dependency_path))
        }
    }
}

fn resolve_optional_snapshot_dependency(
    alias: &str,
    dependency: &str,
    package_lookups: &PackageLookups,
) -> Option<String> {
    if let Ok(path) = dependency.parse::<DependencyPath>() {
        return Some(package_key(&path));
    }
    if let Ok(name_ver_peer) = dependency.parse::<pacquet_lockfile::PkgNameVerPeer>() {
        return package_lookups.resolve(
            &name_ver_peer.name.to_string(),
            &name_ver_peer.suffix.to_string(),
            Some(&name_ver_peer.suffix.version().to_string()),
        );
    }
    if let Ok(version) = dependency.parse::<PkgVerPeer>() {
        return package_lookups.resolve(
            alias,
            &version.to_string(),
            Some(&version.version().to_string()),
        );
    }
    None
}

fn push_reverse_edge(
    reverse_edges: &mut HashMap<String, Vec<ReverseEdge>>,
    forward_edges: &mut HashMap<String, Vec<String>>,
    child_key: String,
    parent_key: &str,
    alias: &str,
) {
    forward_edges.entry(parent_key.to_string()).or_default().push(child_key.clone());
    reverse_edges
        .entry(child_key)
        .or_default()
        .push(ReverseEdge { parent_key: parent_key.to_string(), alias: alias.to_string() });
}

fn package_key(dependency_path: &DependencyPath) -> String {
    format!("pkg:{dependency_path}")
}

fn importer_key(importer_id: &str) -> String {
    format!("importer:{importer_id}")
}

fn resolve_linked_importer(
    parent_importer_id: &str,
    link: &str,
    lockfile_dir: &Path,
    importer_ids: &HashSet<String>,
) -> Option<String> {
    let link_path = link.strip_prefix("link:")?;
    let parent_dir = if parent_importer_id == "." {
        lockfile_dir.to_path_buf()
    } else {
        lockfile_dir.join(parent_importer_id)
    };
    let linked_path = parent_dir.join(link_path);
    let linked_path = fs::canonicalize(&linked_path).unwrap_or(linked_path);
    let lockfile_dir =
        fs::canonicalize(lockfile_dir).unwrap_or_else(|_| lockfile_dir.to_path_buf());
    let relative = linked_path.strip_prefix(&lockfile_dir).ok()?;
    let importer_id = to_importer_id(relative);
    importer_ids.contains(&importer_id).then_some(importer_id)
}

fn to_importer_id(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        return ".".to_string();
    }
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_importer_info(lockfile_dir: &Path, importer_id: &str) -> (String, String) {
    let manifest_path = if importer_id == "." {
        lockfile_dir.join("package.json")
    } else {
        lockfile_dir.join(importer_id).join("package.json")
    };
    let manifest = fs::read_to_string(manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    let name =
        manifest.get("name").and_then(Value::as_str).map(ToOwned::to_owned).unwrap_or_else(|| {
            if importer_id == "." {
                "the root project".to_string()
            } else {
                importer_id.to_string()
            }
        });
    let version =
        manifest.get("version").and_then(Value::as_str).map(ToOwned::to_owned).unwrap_or_default();
    (name, version)
}

fn installed_package_path(
    modules_dir: &Path,
    package_name: &str,
    full_version: &str,
) -> Option<String> {
    let package_name_string = package_name.to_string();
    let package_name: PkgName = package_name.parse().ok()?;
    let package_version: PkgVerPeer = full_version.parse().ok()?;
    let store_name = PkgNameVerPeer::new(package_name, package_version).to_virtual_store_name();
    let path =
        modules_dir.join(".pnpm").join(store_name).join("node_modules").join(package_name_string);
    Some(fs::canonicalize(&path).unwrap_or(path).to_string_lossy().into_owned())
}
