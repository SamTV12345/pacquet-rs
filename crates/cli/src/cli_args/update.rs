use crate::State;
use crate::cli_args::install::{InstallArgs, InstallDependencyOptions, parse_install_reporter};
use clap::Args;
use glob::Pattern;
use miette::Context;
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::current_lockfile_for_installers;
use pacquet_package_manifest::PackageManifest;
use pacquet_registry::{Package, PackageTag, PackageVersion};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Filter by package name, glob, or explicit package spec.
    packages: Vec<String>,

    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    dependency_options: InstallDependencyOptions,

    /// Ignore the currently declared range and move to the latest version.
    #[arg(short = 'L', long)]
    latest: bool,

    /// Show outdated dependencies and select which ones to update.
    #[arg(short = 'i', long)]
    interactive: bool,

    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    filter: Vec<String>,

    /// Update every workspace project recursively (including the workspace root).
    #[clap(short = 'r', long)]
    recursive: bool,

    /// Skip lifecycle scripts during the install phase.
    #[clap(long)]
    ignore_scripts: bool,

    /// Saved dependencies will be configured with an exact version.
    #[clap(short = 'E', long = "save-exact")]
    save_exact: bool,

    /// Skip staleness checks for cached metadata and prefer local metadata when possible.
    #[clap(long)]
    prefer_offline: bool,

    /// Disallow network requests and use only locally available lockfile/store data.
    #[clap(long)]
    offline: bool,

    /// Reporter name.
    #[clap(long)]
    reporter: Option<String>,

    /// Disable pnpm hooks defined in .pnpmfile.cjs.
    #[clap(long)]
    ignore_pnpmfile: bool,

    /// Use hooks from the specified pnpmfile instead of <lockfileDir>/.pnpmfile.cjs.
    #[clap(long)]
    pnpmfile: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct UpdateRequest {
    name_pattern: String,
    explicit_spec: Option<String>,
    pattern: Option<Pattern>,
}

#[derive(Debug, Clone, Copy, Default)]
struct UpdateOutcome {
    matched_dependencies: usize,
    changed_manifest: bool,
}

impl UpdateArgs {
    pub async fn run(self, state: State) -> miette::Result<()> {
        if self.interactive {
            miette::bail!("`pacquet update --interactive` is not implemented yet");
        }
        if self.latest {
            let invalid = self
                .packages
                .iter()
                .filter(|value| split_package_spec(value).1.is_some())
                .cloned()
                .collect::<Vec<_>>();
            if !invalid.is_empty() {
                miette::bail!(
                    "Specs are not allowed to be used with --latest ({})",
                    invalid.join(", ")
                );
            }
        }

        let reporter = parse_install_reporter(self.reporter.as_deref())?;
        let requests = self
            .packages
            .iter()
            .map(|value| parse_update_request(value))
            .collect::<miette::Result<Vec<_>>>()?;
        let config = state.config;
        let manifest = &state.manifest;
        let lockfile = &state.lockfile;
        let lockfile_dir = &state.lockfile_dir;
        let lockfile_importer_id = &state.lockfile_importer_id;
        let workspace_packages = &state.workspace_packages;
        let http_client = &state.http_client;

        let targets = select_update_targets(
            manifest,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            self.recursive,
            &self.filter,
        )?;
        let selected_importers = targets.keys().cloned().collect::<HashSet<_>>();
        let mut current_lockfile = lockfile.clone();
        let mut executed_importers = HashSet::<String>::new();

        for (importer_id, manifest_path) in targets {
            let mut target_manifest = if manifest_path == manifest.path() {
                PackageManifest::from_path(manifest.path().to_path_buf())
                    .wrap_err("reload package.json before update")?
            } else {
                PackageManifest::from_path(manifest_path.clone()).wrap_err_with(|| {
                    format!("load workspace manifest: {}", manifest_path.display())
                })?
            };
            let outcome = apply_updates_to_manifest(
                &mut target_manifest,
                &requests,
                &self.dependency_options,
                self.latest,
                self.save_exact,
                config,
                http_client,
            )
            .await?;

            if outcome.changed_manifest {
                target_manifest.save().wrap_err("save updated package.json")?;
            }

            if outcome.matched_dependencies == 0 && !requests.is_empty() {
                continue;
            }

            let project_dir = target_manifest
                .path()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| lockfile_dir.to_path_buf());
            let target_config = config_for_project(config, &project_dir).leak();
            let target_state = State::init(target_manifest.path().to_path_buf(), target_config)
                .wrap_err("initialize update state")?;
            let target_install_args = self.install_args(reporter);
            target_install_args.run(target_state).await?;
            executed_importers.insert(importer_id.clone());

            current_lockfile = if config.lockfile {
                Lockfile::load_from_dir(lockfile_dir).wrap_err("reload lockfile after update")?
            } else {
                None
            };
        }

        if config.lockfile
            && let Some(lockfile) = current_lockfile.as_ref()
            && !executed_importers.is_empty()
        {
            let current_lockfile = current_lockfile_for_installers(
                lockfile,
                &selected_importers,
                &self.dependency_options.dependency_groups().collect::<Vec<_>>(),
                &HashSet::new(),
            );
            current_lockfile
                .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
                .wrap_err("write node_modules/.pnpm/lock.yaml after update")?;
        }

        Ok(())
    }

    fn install_args(&self, reporter: pacquet_package_manager::InstallReporter) -> InstallArgs {
        InstallArgs {
            dependency_options: InstallDependencyOptions {
                prod: self.dependency_options.prod,
                dev: self.dependency_options.dev,
                no_optional: self.dependency_options.no_optional,
            },
            frozen_lockfile: false,
            prefer_frozen_lockfile: false,
            no_prefer_frozen_lockfile: true,
            fix_lockfile: false,
            ignore_scripts: self.ignore_scripts,
            lockfile_only: false,
            force: false,
            resolution_only: false,
            ignore_pnpmfile: self.ignore_pnpmfile,
            pnpmfile: self.pnpmfile.clone(),
            reporter: Some(
                match reporter {
                    pacquet_package_manager::InstallReporter::Default => "default",
                    pacquet_package_manager::InstallReporter::AppendOnly => "append-only",
                    pacquet_package_manager::InstallReporter::Silent => "silent",
                }
                .to_string(),
            ),
            use_store_server: false,
            shamefully_hoist: false,
            filter: vec![],
            recursive: false,
            prefer_offline: self.prefer_offline,
            offline: self.offline,
        }
    }
}

async fn apply_updates_to_manifest(
    manifest: &mut PackageManifest,
    requests: &[UpdateRequest],
    dependency_options: &InstallDependencyOptions,
    latest: bool,
    save_exact: bool,
    config: &Npmrc,
    http_client: &pacquet_network::ThrottledClient,
) -> miette::Result<UpdateOutcome> {
    let dependency_groups = dependency_options.dependency_groups().collect::<Vec<_>>();
    let existing = dependency_groups
        .iter()
        .copied()
        .flat_map(|group| {
            manifest
                .dependencies([group])
                .map(move |(name, spec)| (group, name.to_string(), spec.to_string()))
        })
        .collect::<Vec<_>>();

    let mut outcome = UpdateOutcome::default();
    for (group, package_name, current_spec) in existing {
        let request = match_request(requests, &package_name);
        if requests.is_empty() {
            outcome.matched_dependencies += 1;
        } else {
            let Some(request) = request else {
                continue;
            };
            outcome.matched_dependencies += 1;
            let Some(next_spec) = resolve_updated_spec(
                &package_name,
                &current_spec,
                request.explicit_spec.as_deref(),
                latest,
                save_exact,
                config,
                http_client,
            )
            .await?
            else {
                continue;
            };
            if next_spec == current_spec {
                continue;
            }
            manifest
                .add_dependency(&package_name, &next_spec, group)
                .wrap_err_with(|| format!("update {package_name} in package.json"))?;
            outcome.changed_manifest = true;
            continue;
        };
        let Some(next_spec) = resolve_updated_spec(
            &package_name,
            &current_spec,
            None,
            latest,
            save_exact,
            config,
            http_client,
        )
        .await?
        else {
            continue;
        };
        if next_spec == current_spec {
            continue;
        }
        manifest
            .add_dependency(&package_name, &next_spec, group)
            .wrap_err_with(|| format!("update {package_name} in package.json"))?;
        outcome.changed_manifest = true;
    }

    Ok(outcome)
}

async fn resolve_updated_spec(
    package_name: &str,
    current_spec: &str,
    explicit_spec: Option<&str>,
    latest: bool,
    save_exact: bool,
    config: &Npmrc,
    http_client: &pacquet_network::ThrottledClient,
) -> miette::Result<Option<String>> {
    if let Some(explicit_spec) = explicit_spec {
        return Ok(Some(explicit_spec.to_string()));
    }
    if should_skip_specifier(current_spec) {
        return Ok(None);
    }

    let registry = config.registry_for_package_name(package_name);
    let auth_header = config.auth_header_for_url(&format!("{registry}{package_name}"));
    let package =
        Package::fetch_from_registry(package_name, http_client, &registry, auth_header.as_deref())
            .await
            .wrap_err_with(|| format!("fetch metadata for {package_name}"))?;

    let selected_version = if latest {
        package.latest()
    } else {
        package.pinned_version(current_spec).unwrap_or_else(|| package.latest())
    };
    Ok(Some(render_updated_spec(current_spec, selected_version, latest, save_exact)))
}

fn render_updated_spec(
    current_spec: &str,
    selected_version: &PackageVersion,
    latest: bool,
    save_exact: bool,
) -> String {
    if latest {
        if current_spec.starts_with('~') {
            return format!("~{}", selected_version.version);
        }
        if current_spec.starts_with('^') {
            return format!("^{}", selected_version.version);
        }
        return selected_version.serialize(save_exact);
    }

    if current_spec.starts_with('~') {
        return format!("~{}", selected_version.version);
    }
    if current_spec.starts_with('^') {
        return format!("^{}", selected_version.version);
    }
    if current_spec.parse::<PackageTag>().is_ok() || is_exact_version_spec(current_spec) {
        return selected_version.version.to_string();
    }
    current_spec.to_string()
}

fn match_request<'a>(
    requests: &'a [UpdateRequest],
    package_name: &str,
) -> Option<&'a UpdateRequest> {
    if requests.is_empty() {
        return None;
    }
    requests.iter().find(|request| {
        request
            .pattern
            .as_ref()
            .map(|pattern| pattern.matches(package_name))
            .unwrap_or_else(|| request.name_pattern == package_name)
    })
}

fn is_exact_version_spec(spec: &str) -> bool {
    !spec.is_empty()
        && spec
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'a'..='z' | 'A'..='Z'))
}

fn parse_update_request(value: &str) -> miette::Result<UpdateRequest> {
    let (name_pattern, explicit_spec) = split_package_spec(value);
    let pattern =
        if name_pattern.contains('*') || name_pattern.contains('?') || name_pattern.contains('[') {
            Some(Pattern::new(name_pattern).map_err(|error| {
                miette::miette!("invalid update package selector `{value}`: {error}")
            })?)
        } else {
            None
        };
    Ok(UpdateRequest {
        name_pattern: name_pattern.to_string(),
        explicit_spec: explicit_spec.map(ToString::to_string),
        pattern,
    })
}

fn split_package_spec(package: &str) -> (&str, Option<&str>) {
    let separator = if let Some(stripped) = package.strip_prefix('@') {
        stripped.rfind('@').map(|index| index + 1)
    } else {
        package.rfind('@')
    };
    match separator {
        Some(index) => {
            let (name, spec) = package.split_at(index);
            let spec = &spec[1..];
            if spec.is_empty() { (package, None) } else { (name, Some(spec)) }
        }
        None => (package, None),
    }
}

fn select_update_targets(
    manifest: &PackageManifest,
    lockfile_dir: &Path,
    lockfile_importer_id: &str,
    workspace_packages: &pacquet_package_manager::WorkspacePackages,
    recursive: bool,
    filters: &[String],
) -> miette::Result<BTreeMap<String, PathBuf>> {
    if !recursive && filters.is_empty() {
        return Ok(BTreeMap::from([(
            lockfile_importer_id.to_string(),
            manifest.path().to_path_buf(),
        )]));
    }

    let mut targets = BTreeMap::<String, PathBuf>::new();
    if lockfile_dir.join("pnpm-workspace.yaml").is_file() {
        let root_manifest = lockfile_dir.join("package.json");
        if root_manifest.is_file()
            && (recursive
                || matches_filter(".", &root_manifest, filters, workspace_packages, lockfile_dir))
        {
            targets.insert(".".to_string(), root_manifest);
        }
    }
    for (name, info) in workspace_packages.iter() {
        let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
        if recursive
            || filters.iter().any(|selector| selector_matches(selector, &importer_id, name))
        {
            targets.insert(importer_id, info.root_dir.join("package.json"));
        }
    }

    if targets.is_empty() {
        if recursive {
            miette::bail!(
                "No workspace projects found for --recursive. Ensure pnpm-workspace.yaml includes package patterns."
            );
        }
        miette::bail!("No workspace projects matched --filter selectors: {}", filters.join(", "));
    }

    Ok(targets)
}

fn matches_filter(
    importer_id: &str,
    manifest_path: &Path,
    filters: &[String],
    workspace_packages: &pacquet_package_manager::WorkspacePackages,
    lockfile_dir: &Path,
) -> bool {
    filters.iter().any(|selector| {
        let normalized = selector.trim_start_matches("./").replace('\\', "/");
        normalized == importer_id
            || (importer_id == "."
                && root_package_name(manifest_path).is_some_and(|name| normalized == name))
            || workspace_packages.iter().any(|(name, info)| {
                to_lockfile_importer_id(lockfile_dir, &info.root_dir) == importer_id
                    && normalized == *name
            })
    })
}

fn selector_matches(selector: &str, importer_id: &str, package_name: &str) -> bool {
    let normalized = selector.trim_start_matches("./").replace('\\', "/");
    normalized == importer_id || normalized == package_name
}

fn config_for_project(config: &Npmrc, project_dir: &Path) -> Npmrc {
    let mut next = config.clone();
    next.modules_dir = project_dir.join("node_modules");
    next.virtual_store_dir = next.modules_dir.join(".pnpm");
    next
}

fn to_lockfile_importer_id(workspace_root: &Path, project_dir: &Path) -> String {
    let Ok(relative) = project_dir.strip_prefix(workspace_root) else {
        return ".".to_string();
    };
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn root_package_name(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    value.get("name").and_then(serde_json::Value::as_str).map(ToString::to_string)
}

fn should_skip_specifier(specifier: &str) -> bool {
    specifier.starts_with("workspace:")
        || specifier.starts_with("file:")
        || specifier.starts_with("link:")
        || specifier.starts_with("npm:")
        || specifier.contains("github:")
        || specifier.contains("git+")
}
