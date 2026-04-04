use crate::{
    Install, InstallReporter, ProgressStats, ResolvedPackages, WorkspacePackages,
    format_summary_dependency_line, is_git_spec, is_tarball_spec, last_finished_progress_reporter,
    normalize_git_spec, resolve_package_version_from_git_spec,
    resolve_package_version_from_tarball_spec,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::Lockfile;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::{LinkWorkspacePackages, Npmrc, SaveWorkspaceProtocol};
use pacquet_package_manifest::PackageManifestError;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::{Package, PackageTag, PackageVersion, RegistryError};
use pacquet_tarball::MemCache;
use std::io::Write;
use std::path::{Path, PathBuf};

/// This subroutine does everything `pacquet add` is supposed to do.
#[must_use]
pub struct Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a mut PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub lockfile_dir: &'a Path,
    pub lockfile_importer_id: &'a str,
    pub workspace_packages: &'a WorkspacePackages,
    pub list_dependency_groups: ListDependencyGroups, // must be a function because it is called multiple times
    pub packages: &'a [String],
    pub save_exact: bool, // TODO: add `save-exact` to `.npmrc`, merge configs, and remove this
    pub workspace_only: bool,
    pub pnpmfile: Option<&'a Path>,
    pub ignore_pnpmfile: bool,
    pub reporter: InstallReporter,
}

/// Error type of [`Add`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum AddError {
    #[display("Failed to add package to manifest: {_0}")]
    AddDependencyToManifest(#[error(source)] PackageManifestError),
    #[display("Failed to resolve package from registry: {_0}")]
    ResolvePackage(#[error(source)] RegistryError),
    #[display("No version of {package} satisfies {spec}")]
    NoMatchingVersion { package: String, spec: String },
    #[display("Cannot resolve local dependency path: {path}")]
    LocalDependencyPathNotFound { path: String },
    #[display("Failed to load local dependency manifest at {_0}: {_1}")]
    LoadLocalDependencyManifest(String, #[error(source)] PackageManifestError),
    #[display("Local dependency manifest at {path} is missing a package name")]
    LocalDependencyManifestNameMissing { path: String },
    #[display("\"{package}\" not found in the workspace")]
    WorkspacePackageNotFound { package: String },
    #[display("Failed to install dependencies: {_0}")]
    InstallDependencies(#[error(not(source))] String),
    #[display("Failed save the manifest file: {_0}")]
    SaveManifest(#[error(source)] PackageManifestError),
}

impl<'a, ListDependencyGroups, DependencyGroupList>
    Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub async fn run(self) -> Result<Vec<String>, AddError> {
        let start_time = std::time::Instant::now();
        let Add {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            list_dependency_groups,
            packages,
            save_exact,
            workspace_only,
            pnpmfile,
            ignore_pnpmfile,
            reporter,
            resolved_packages,
        } = self;

        let mut package_specs = Vec::with_capacity(packages.len());
        let mut summary_entries = Vec::new();
        let project_dir =
            manifest.path().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        for package in packages {
            let package_spec = resolve_package_spec(
                config,
                http_client,
                workspace_packages,
                &project_dir,
                lockfile,
                package,
                save_exact,
                workspace_only,
            )
            .await?;
            package_specs.push(package_spec);
        }

        for (package_name, version_range) in package_specs {
            for dependency_group in list_dependency_groups() {
                manifest
                    .add_dependency(&package_name, &version_range, dependency_group)
                    .map_err(AddError::AddDependencyToManifest)?;
                let header = match dependency_group {
                    DependencyGroup::Prod => "dependencies",
                    DependencyGroup::Dev => "devDependencies",
                    DependencyGroup::Optional => "optionalDependencies",
                    DependencyGroup::Peer => "peerDependencies",
                };
                summary_entries.push((header, package_name.clone(), version_range.clone()));
            }
        }

        let skipped = Install {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            preferred_versions: None,
            // Always pass all dependency groups so that Install preserves
            // existing symlinks from previous add operations (pnpm passes
            // include={dependencies,devDependencies,optionalDependencies}
            // during add, not just the group being added).
            dependency_groups: [
                DependencyGroup::Prod,
                DependencyGroup::Dev,
                DependencyGroup::Optional,
            ],
            frozen_lockfile: false,
            lockfile_only: false,
            force: false,
            prefer_offline: false,
            offline: false,
            pnpmfile,
            ignore_pnpmfile,
            reporter_prefix: None,
            reporter,
            print_summary: false,
            manage_progress_reporter: true,
            resolved_packages,
            additional_importers: Vec::new(),
        }
        .run()
        .await
        .map_err(|error| AddError::InstallDependencies(error.to_string()))?;

        manifest.save().map_err(AddError::SaveManifest)?;
        if reporter != InstallReporter::Silent {
            print_add_summary(
                &summary_entries,
                last_finished_progress_reporter().unwrap_or_default(),
                start_time.elapsed().as_millis(),
            );
        }

        Ok(skipped)
    }
}

fn print_add_summary(
    entries: &[(&'static str, String, String)],
    progress: ProgressStats,
    elapsed_ms: u128,
) {
    use std::collections::BTreeMap;

    let mut out = std::io::stdout().lock();

    if progress.added == 0 {
        let _ = writeln!(out, "Already up to date");
        let _ = writeln!(out);
    } else {
        let _ = writeln!(out, "Packages: +{}", progress.added);
        let _ = writeln!(out, "{}", "+".repeat(progress.added.min(80)));
        let _ = writeln!(out);
    }

    let mut grouped = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for (header, name, spec) in entries {
        grouped.entry(header).or_default().push((name, spec));
    }
    for (header, group_entries) in grouped {
        let _ = writeln!(out, "{header}:");
        for (name, spec) in group_entries {
            let _ = writeln!(out, "{}", format_summary_dependency_line(name, spec));
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "Done in {elapsed_ms}ms using pacquet v{}", env!("CARGO_PKG_VERSION"));
}

#[allow(clippy::too_many_arguments)]
async fn resolve_package_spec(
    config: &Npmrc,
    http_client: &ThrottledClient,
    workspace_packages: &WorkspacePackages,
    project_dir: &Path,
    lockfile: Option<&Lockfile>,
    package: &str,
    save_exact: bool,
    workspace_only: bool,
) -> Result<(String, String), AddError> {
    if let Some((alias_name, target_name, target_requested_spec)) = parse_npm_alias_spec(package) {
        let resolved_spec = resolve_npm_alias_target_spec(
            config,
            http_client,
            target_name,
            target_requested_spec,
            save_exact,
        )
        .await?;
        return Ok((alias_name.to_string(), format!("npm:{target_name}@{resolved_spec}")));
    }

    let (package_name, requested_spec) = split_package_spec(package);
    if let Some(spec) = requested_spec
        && is_git_spec(spec)
    {
        resolve_package_version_from_git_spec(config, http_client, spec)
            .await
            .map_err(AddError::InstallDependencies)?;
        let normalized_spec = normalize_git_spec(spec).unwrap_or_else(|| spec.to_string());
        return Ok((package_name.to_string(), normalized_spec));
    }

    if let Some((name, spec)) = resolve_local_path_spec(project_dir, package)? {
        return Ok((name, spec));
    }
    if is_tarball_spec(package) {
        let package_version =
            resolve_package_version_from_tarball_spec(config, http_client, package)
                .await
                .map_err(AddError::InstallDependencies)?;
        return Ok((package_version.name, package.to_string()));
    }
    if is_git_spec(package) {
        let package_version = resolve_package_version_from_git_spec(config, http_client, package)
            .await
            .map_err(AddError::InstallDependencies)?;
        let normalized_spec = normalize_git_spec(package).unwrap_or_else(|| package.to_string());
        return Ok((package_version.name, normalized_spec));
    }
    if let Some(spec) = requested_spec
        && spec.starts_with("workspace:")
    {
        let Some(workspace_package) = workspace_packages.get(package_name) else {
            return Err(AddError::WorkspacePackageNotFound { package: package_name.to_string() });
        };
        let workspace_spec = match config.save_workspace_protocol {
            SaveWorkspaceProtocol::Rolling => spec.to_string(),
            SaveWorkspaceProtocol::True | SaveWorkspaceProtocol::False => {
                workspace_spec_with_prefix(workspace_package.version.as_str(), spec)
            }
        };
        return Ok((package_name.to_string(), workspace_spec));
    }

    if workspace_only {
        let Some(workspace_package) = workspace_packages.get(package_name) else {
            return Err(AddError::WorkspacePackageNotFound { package: package_name.to_string() });
        };
        let workspace_spec = render_added_workspace_spec(
            config.save_workspace_protocol,
            workspace_package.version.as_str(),
            requested_spec,
        );
        return Ok((package_name.to_string(), workspace_spec));
    }

    if requested_spec.is_none()
        && let Some(workspace_package) = workspace_packages.get(package_name)
    {
        match config.save_workspace_protocol {
            SaveWorkspaceProtocol::Rolling | SaveWorkspaceProtocol::True => {
                let workspace_spec = render_added_workspace_spec(
                    config.save_workspace_protocol,
                    workspace_package.version.as_str(),
                    requested_spec,
                );
                return Ok((package_name.to_string(), workspace_spec));
            }
            SaveWorkspaceProtocol::False
                if config.link_workspace_packages != LinkWorkspacePackages::False =>
            {
                let version_spec = if save_exact {
                    workspace_package.version.clone()
                } else {
                    format!("^{}", workspace_package.version)
                };
                return Ok((package_name.to_string(), version_spec));
            }
            SaveWorkspaceProtocol::False => {}
        }
    }

    let registry = config.registry_for_package_name(package_name);
    let version_range = match requested_spec {
        None => {
            if let Some(existing_specifier) =
                find_existing_workspace_direct_dependency_spec(lockfile, package_name)
            {
                existing_specifier
            } else {
                let auth_header =
                    config.auth_header_for_url(&format!("{registry}{package_name}/latest"));
                let latest_version = PackageVersion::fetch_from_registry(
                    package_name,
                    PackageTag::Latest,
                    http_client,
                    &registry,
                    auth_header.as_deref(),
                )
                .await
                .map_err(AddError::ResolvePackage)?;
                latest_version.serialize(save_exact)
            }
        }
        Some(spec) if spec.parse::<PackageTag>().is_ok() => {
            let auth_header =
                config.auth_header_for_url(&format!("{registry}{package_name}/{spec}"));
            PackageVersion::fetch_from_registry(
                package_name,
                spec.parse::<PackageTag>().expect("checked above"),
                http_client,
                &registry,
                auth_header.as_deref(),
            )
            .await
            .map_err(AddError::ResolvePackage)?;
            spec.to_string()
        }
        Some(spec) => {
            let auth_header = config.auth_header_for_url(&format!("{registry}{package_name}"));
            let package = Package::fetch_from_registry(
                package_name,
                http_client,
                &registry,
                auth_header.as_deref(),
            )
            .await
            .map_err(AddError::ResolvePackage)?;
            if package.pinned_version(spec).is_none() {
                return Err(AddError::NoMatchingVersion {
                    package: package_name.to_string(),
                    spec: spec.to_string(),
                });
            }
            spec.to_string()
        }
    };

    Ok((package_name.to_string(), version_range))
}

fn render_added_workspace_spec(
    save_workspace_protocol: SaveWorkspaceProtocol,
    workspace_version: &str,
    requested_spec: Option<&str>,
) -> String {
    match save_workspace_protocol {
        SaveWorkspaceProtocol::Rolling => match requested_spec {
            Some(spec) if spec.starts_with('~') => "workspace:~".to_string(),
            Some(spec) if spec.starts_with('^') => "workspace:^".to_string(),
            Some(spec) if spec.starts_with("workspace:") => spec.to_string(),
            Some(spec) if spec.parse::<PackageTag>().is_ok() => "workspace:^".to_string(),
            Some(_) => "workspace:^".to_string(),
            None => "workspace:^".to_string(),
        },
        SaveWorkspaceProtocol::True | SaveWorkspaceProtocol::False => {
            workspace_spec_with_prefix(workspace_version, requested_spec.unwrap_or("^"))
        }
    }
}

fn workspace_spec_with_prefix(workspace_version: &str, spec: &str) -> String {
    let normalized = spec.strip_prefix("workspace:").unwrap_or(spec);
    if normalized.starts_with('~') {
        return format!("workspace:~{workspace_version}");
    }
    if normalized.starts_with('^') {
        return format!("workspace:^{workspace_version}");
    }
    if normalized == "*" || normalized.parse::<PackageTag>().is_ok() {
        return format!("workspace:^{workspace_version}");
    }
    if normalized.is_empty() {
        return format!("workspace:^{workspace_version}");
    }
    format!("workspace:{workspace_version}")
}

fn find_existing_workspace_direct_dependency_spec(
    lockfile: Option<&Lockfile>,
    package_name: &str,
) -> Option<String> {
    let lockfile = lockfile?;
    match &lockfile.project_snapshot {
        pacquet_lockfile::RootProjectSnapshot::Single(snapshot) => {
            find_direct_dependency_spec_in_snapshot(snapshot, package_name)
        }
        pacquet_lockfile::RootProjectSnapshot::Multi(snapshot) => snapshot
            .importers
            .values()
            .find_map(|snapshot| find_direct_dependency_spec_in_snapshot(snapshot, package_name)),
    }
}

fn find_direct_dependency_spec_in_snapshot(
    snapshot: &pacquet_lockfile::ProjectSnapshot,
    package_name: &str,
) -> Option<String> {
    [DependencyGroup::Prod, DependencyGroup::Optional, DependencyGroup::Dev]
        .into_iter()
        .flat_map(|group| snapshot.get_map_by_group(group))
        .flatten()
        .find(|(name, spec)| {
            name.to_string() == package_name
                && !matches!(spec.version, pacquet_lockfile::ResolvedDependencyVersion::Link(_))
        })
        .map(|(_, spec)| spec.specifier.clone())
}

async fn resolve_npm_alias_target_spec(
    config: &Npmrc,
    http_client: &ThrottledClient,
    target_name: &str,
    requested_spec: Option<&str>,
    save_exact: bool,
) -> Result<String, AddError> {
    let registry = config.registry_for_package_name(target_name);
    let resolved = match requested_spec {
        None => {
            let auth_header =
                config.auth_header_for_url(&format!("{registry}{target_name}/latest"));
            let latest_version = PackageVersion::fetch_from_registry(
                target_name,
                PackageTag::Latest,
                http_client,
                &registry,
                auth_header.as_deref(),
            )
            .await
            .map_err(AddError::ResolvePackage)?;
            latest_version.serialize(save_exact)
        }
        Some(spec) if spec.parse::<PackageTag>().is_ok() => {
            let tag = spec.parse::<PackageTag>().expect("checked above");
            let auth_header =
                config.auth_header_for_url(&format!("{registry}{target_name}/{spec}"));
            let resolved_version = PackageVersion::fetch_from_registry(
                target_name,
                spec.parse::<PackageTag>().expect("checked above"),
                http_client,
                &registry,
                auth_header.as_deref(),
            )
            .await
            .map_err(AddError::ResolvePackage)?;
            match tag {
                PackageTag::Latest => resolved_version.serialize(save_exact),
                PackageTag::Version(_) => spec.to_string(),
            }
        }
        Some(spec) => {
            let auth_header = config.auth_header_for_url(&format!("{registry}{target_name}"));
            let package = Package::fetch_from_registry(
                target_name,
                http_client,
                &registry,
                auth_header.as_deref(),
            )
            .await
            .map_err(AddError::ResolvePackage)?;
            if package.pinned_version(spec).is_none() {
                return Err(AddError::NoMatchingVersion {
                    package: target_name.to_string(),
                    spec: spec.to_string(),
                });
            }
            spec.to_string()
        }
    };
    Ok(resolved)
}

fn resolve_local_path_spec(
    project_dir: &Path,
    package: &str,
) -> Result<Option<(String, String)>, AddError> {
    let (protocol, path_str) = if let Some(path) = package.strip_prefix("file:") {
        ("file:", path)
    } else if let Some(path) = package.strip_prefix("link:") {
        ("link:", path)
    } else {
        ("link:", package)
    };

    let path = Path::new(path_str);
    let candidate = if path.is_absolute() { path.to_path_buf() } else { project_dir.join(path) };
    if !candidate.exists() {
        return Ok(None);
    }

    let manifest_path = candidate.join("package.json");
    let name = if manifest_path.is_file() {
        let manifest = PackageManifest::from_path(manifest_path.clone()).map_err(|error| {
            AddError::LoadLocalDependencyManifest(manifest_path.display().to_string(), error)
        })?;
        let Some(name) = manifest.value().get("name").and_then(|value| value.as_str()) else {
            return Err(AddError::LocalDependencyManifestNameMissing {
                path: manifest_path.display().to_string(),
            });
        };
        name.to_string()
    } else {
        candidate
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| AddError::LocalDependencyPathNotFound { path: package.to_string() })?
            .to_string()
    };

    let spec = if path.is_absolute() {
        format!("{protocol}{}", candidate.to_string_lossy().replace('\\', "/"))
    } else {
        let separator = std::path::MAIN_SEPARATOR.to_string();
        let normalized = path_str.replace(['/', '\\'], &separator);
        let normalized = normalized.strip_prefix(&format!(".{separator}")).unwrap_or(&normalized);
        format!("{protocol}{normalized}")
    };
    Ok(Some((name, spec)))
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

fn parse_npm_alias_spec(package: &str) -> Option<(&str, &str, Option<&str>)> {
    let marker = package.find("@npm:")?;
    if marker == 0 {
        return None;
    }
    let alias_name = &package[..marker];
    let target = &package[(marker + "@npm:".len())..];
    let (target_name, requested_spec) = split_package_spec(target);
    if alias_name.is_empty() || target_name.is_empty() {
        return None;
    }
    Some((alias_name, target_name, requested_spec))
}

#[cfg(test)]
mod tests {
    use super::{parse_npm_alias_spec, resolve_local_path_spec, split_package_spec};
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn split_package_spec_keeps_plain_names() {
        assert_eq!(split_package_spec("fastify"), ("fastify", None));
        assert_eq!(split_package_spec("@scope/pkg"), ("@scope/pkg", None));
    }

    #[test]
    fn split_package_spec_extracts_requested_spec() {
        assert_eq!(split_package_spec("fastify@1.2.3"), ("fastify", Some("1.2.3")));
        assert_eq!(split_package_spec("fastify@latest"), ("fastify", Some("latest")));
        assert_eq!(split_package_spec("@scope/pkg@^1.0.0"), ("@scope/pkg", Some("^1.0.0")));
        assert_eq!(
            split_package_spec("say-hi@github:zkochan/hi#main"),
            ("say-hi", Some("github:zkochan/hi#main"))
        );
        assert_eq!(
            split_package_spec("@scope/pkg@workspace:*"),
            ("@scope/pkg", Some("workspace:*"))
        );
    }

    #[test]
    fn parse_npm_alias_spec_extracts_alias_and_target() {
        assert_eq!(
            parse_npm_alias_spec("hello-alias@npm:is-number@7.0.0"),
            Some(("hello-alias", "is-number", Some("7.0.0")))
        );
        assert_eq!(
            parse_npm_alias_spec("hello-alias@npm:is-number"),
            Some(("hello-alias", "is-number", None))
        );
        assert_eq!(
            parse_npm_alias_spec("@scope/alias@npm:@scope/pkg@^1.0.0"),
            Some(("@scope/alias", "@scope/pkg", Some("^1.0.0")))
        );
        assert_eq!(
            parse_npm_alias_spec("hello-alias@npm:@pnpm.e2e/hello-world-js-bin@1.0.0"),
            Some(("hello-alias", "@pnpm.e2e/hello-world-js-bin", Some("1.0.0")))
        );
        assert_eq!(
            parse_npm_alias_spec("hello-alias@npm:@pnpm.e2e/hello-world-js-bin"),
            Some(("hello-alias", "@pnpm.e2e/hello-world-js-bin", None))
        );
    }

    #[test]
    fn resolve_local_relative_path_spec() {
        let root = tempdir().unwrap();
        let app = root.path().join("app");
        let lib = root.path().join("lib");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&lib).unwrap();
        fs::write(
            lib.join("package.json"),
            serde_json::json!({
                "name": "@repo/lib",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .unwrap();

        let result = resolve_local_path_spec(&app, "../lib")
            .expect("local path should resolve")
            .expect("local path should be detected");
        assert_eq!(result.0, "@repo/lib");
        #[cfg(windows)]
        assert_eq!(result.1, r"link:..\lib");
        #[cfg(not(windows))]
        assert_eq!(result.1, "link:../lib");
    }

    #[test]
    fn resolve_local_file_protocol_path_without_manifest_uses_directory_name() {
        let root = tempdir().unwrap();
        let app = root.path().join("app");
        let pkg = app.join("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("index.js"), "module.exports = 'pkg';\n").unwrap();

        let result = resolve_local_path_spec(&app, "file:./pkg")
            .expect("local path should resolve")
            .expect("local path should be detected");
        assert_eq!(result.0, "pkg");
        assert_eq!(result.1, "file:pkg");
    }

    #[test]
    fn resolve_local_link_protocol_path_strips_current_dir_prefix() {
        let root = tempdir().unwrap();
        let app = root.path().join("app");
        let pkg = app.join("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            pkg.join("package.json"),
            serde_json::json!({
                "name": "pkg",
                "version": "1.0.0"
            })
            .to_string(),
        )
        .unwrap();

        let result = resolve_local_path_spec(&app, "link:./pkg")
            .expect("local path should resolve")
            .expect("local path should be detected");
        assert_eq!(result.0, "pkg");
        assert_eq!(result.1, "link:pkg");
    }
}
