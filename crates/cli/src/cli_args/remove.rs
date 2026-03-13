use crate::State;
use crate::cli_args::install::parse_install_reporter;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::{
    Install, current_lockfile_for_installers, format_prefixed_summary_stats,
    format_summary_dependency_line_with_prefix,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct RemoveDependencyOptions {
    /// Remove the dependency only from dependencies.
    #[clap(short = 'P', long)]
    save_prod: bool,
    /// Remove the dependency only from devDependencies.
    #[clap(short = 'D', long)]
    save_dev: bool,
    /// Remove the dependency only from optionalDependencies.
    #[clap(short = 'O', long)]
    save_optional: bool,
}

impl RemoveDependencyOptions {
    fn dependency_groups(&self) -> Vec<DependencyGroup> {
        let &RemoveDependencyOptions { save_prod, save_dev, save_optional } = self;
        if save_prod || save_dev || save_optional {
            return std::iter::empty()
                .chain(save_prod.then_some(DependencyGroup::Prod))
                .chain(save_dev.then_some(DependencyGroup::Dev))
                .chain(save_optional.then_some(DependencyGroup::Optional))
                .collect();
        }

        vec![
            DependencyGroup::Prod,
            DependencyGroup::Dev,
            DependencyGroup::Optional,
            DependencyGroup::Peer,
        ]
    }
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Package names to remove.
    pub packages: Vec<String>,
    /// --save-prod, --save-dev, --save-optional
    #[clap(flatten)]
    pub dependency_options: RemoveDependencyOptions,
    /// Select only matching workspace projects (by package name or workspace-relative path).
    #[clap(long = "filter")]
    pub filter: Vec<String>,
    /// Remove dependencies from every workspace project recursively (including workspace root).
    #[clap(short = 'r', long)]
    pub recursive: bool,
    /// Reporter name.
    #[clap(long)]
    pub reporter: Option<String>,
    /// Disable pnpm hooks defined in .pnpmfile.cjs.
    #[clap(long)]
    pub ignore_pnpmfile: bool,
    /// Use hooks from the specified pnpmfile instead of <lockfileDir>/.pnpmfile.cjs.
    #[clap(long)]
    pub pnpmfile: Option<PathBuf>,
}

impl RemoveArgs {
    pub async fn run(self, mut state: State) -> miette::Result<()> {
        let start_time = std::time::Instant::now();
        let RemoveArgs {
            packages,
            dependency_options,
            filter,
            recursive,
            reporter,
            ignore_pnpmfile,
            pnpmfile,
        } = self;
        if packages.is_empty() {
            miette::bail!("At least one dependency name should be specified for removal");
        }
        let reporter = parse_install_reporter(reporter.as_deref())?;

        let State {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
            resolved_packages,
        } = &mut state;

        let groups = dependency_options.dependency_groups();
        if !filter.is_empty() || recursive {
            let mut targets = BTreeMap::<String, PathBuf>::new();
            if lockfile_dir.join("pnpm-workspace.yaml").is_file() {
                let root_manifest = lockfile_dir.join("package.json");
                if root_manifest.is_file() {
                    targets.insert(".".to_string(), root_manifest);
                }
            }
            for (name, info) in workspace_packages.iter() {
                let importer_id = to_lockfile_importer_id(lockfile_dir, &info.root_dir);
                let matches = if filter.is_empty() {
                    true
                } else {
                    filter.iter().any(|selector| {
                        let normalized = selector.trim_start_matches("./").replace('\\', "/");
                        normalized == importer_id || normalized == name.as_str()
                    })
                };
                if recursive || matches {
                    targets.insert(importer_id, info.root_dir.join("package.json"));
                }
            }
            if !filter.is_empty() {
                targets.retain(|importer_id, manifest_path| {
                    let normalized_path = importer_id.trim_start_matches("./").replace('\\', "/");
                    filter.iter().any(|selector| {
                        let normalized = selector.trim_start_matches("./").replace('\\', "/");
                        normalized == normalized_path
                            || normalized == *importer_id
                            || (importer_id == "."
                                && root_package_name(lockfile_dir.join("package.json").as_path())
                                    .is_some_and(|name| normalized == name))
                            || workspace_packages.iter().any(|(name, info)| {
                                to_lockfile_importer_id(lockfile_dir, &info.root_dir)
                                    == *importer_id
                                    && normalized == name.as_str()
                            })
                            || (manifest_path == manifest.path() && normalized == ".")
                    })
                });
            }
            if targets.is_empty() {
                return Ok(());
            }

            let multiple_targets = targets.len() > 1;
            let selected_importers = targets.keys().cloned().collect::<HashSet<_>>();
            let mut skipped_dep_paths = HashSet::<String>::new();
            let mut current_lockfile = lockfile.clone();
            for (importer_id, manifest_path) in targets {
                let mut target_manifest = PackageManifest::from_path(manifest_path.clone())
                    .wrap_err_with(|| {
                        format!("load workspace manifest: {}", manifest_path.display())
                    })?;
                let project_dir = target_manifest
                    .path()
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| lockfile_dir.to_path_buf());
                let target_config = config_for_project(config, &project_dir).leak();
                let removed_entries = collect_removed_entries(&target_manifest, &packages, &groups);

                let removed_any = apply_remove_to_manifest(
                    &packages,
                    &groups,
                    &mut target_manifest,
                    false,
                    target_config,
                )?;
                if !removed_any {
                    continue;
                }

                let skipped = Install {
                    tarball_mem_cache,
                    resolved_packages,
                    http_client,
                    config: target_config,
                    manifest: &target_manifest,
                    lockfile: current_lockfile.as_ref(),
                    lockfile_dir,
                    lockfile_importer_id: &importer_id,
                    workspace_packages,
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
                    pnpmfile: pnpmfile.as_deref(),
                    ignore_pnpmfile,
                    reporter_prefix: multiple_targets.then_some(importer_id.as_str()),
                    reporter,
                    print_summary: false,
                    manage_progress_reporter: true,
                }
                .run()
                .await
                .wrap_err("reinstall after removal")?;
                skipped_dep_paths.extend(skipped);

                if reporter != pacquet_package_manager::InstallReporter::Silent {
                    print_remove_summary(
                        &removed_entries,
                        start_time.elapsed().as_millis(),
                        multiple_targets.then_some(importer_id.as_str()),
                    );
                }

                current_lockfile = if config.lockfile {
                    Lockfile::load_from_dir(lockfile_dir)
                        .wrap_err("reload lockfile after workspace remove")?
                } else {
                    None
                };
            }

            if config.lockfile
                && let Some(lockfile) = current_lockfile.as_ref()
            {
                let current_lockfile = current_lockfile_for_installers(
                    lockfile,
                    &selected_importers,
                    &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
                    &skipped_dep_paths,
                );
                current_lockfile
                    .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
                    .wrap_err("write node_modules/.pnpm/lock.yaml after remove")?;
            }

            return Ok(());
        }

        let removed_entries = collect_removed_entries(manifest, &packages, &groups);
        apply_remove_to_manifest(&packages, &groups, manifest, true, config)?;

        let skipped = Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            lockfile: lockfile.as_ref(),
            lockfile_dir,
            lockfile_importer_id,
            workspace_packages,
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
            pnpmfile: pnpmfile.as_deref(),
            ignore_pnpmfile,
            reporter_prefix: None,
            reporter,
            print_summary: false,
            manage_progress_reporter: true,
        }
        .run()
        .await
        .wrap_err("reinstall after removal")?;

        if config.lockfile {
            let current_lockfile =
                Lockfile::load_from_dir(lockfile_dir).wrap_err("reload lockfile after removal")?;
            if let Some(lockfile) = current_lockfile.as_ref() {
                let current_lockfile = current_lockfile_for_installers(
                    lockfile,
                    &HashSet::from([lockfile_importer_id.clone()]),
                    &[DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
                    &skipped.into_iter().collect::<HashSet<_>>(),
                );
                current_lockfile
                    .save_to_path(&config.virtual_store_dir.join("lock.yaml"))
                    .wrap_err("write node_modules/.pnpm/lock.yaml after remove")?;
            }
        }

        if reporter != pacquet_package_manager::InstallReporter::Silent {
            print_remove_summary(&removed_entries, start_time.elapsed().as_millis(), None);
        }
        Ok(())
    }
}

fn apply_remove_to_manifest(
    packages: &[String],
    groups: &[DependencyGroup],
    manifest: &mut PackageManifest,
    fail_when_missing: bool,
    config: &Npmrc,
) -> miette::Result<bool> {
    let mut missing = Vec::<String>::new();
    let mut removed_any = false;
    let manifest_path = manifest.path().to_path_buf();
    let mut manifest_json: Value = fs::read_to_string(&manifest_path)
        .into_diagnostic()
        .wrap_err("read package.json before removal")
        .and_then(|content| {
            serde_json::from_str(&content)
                .into_diagnostic()
                .wrap_err("parse package.json before removal")
        })?;

    for package_name in packages {
        let removed = remove_dependency_from_manifest_json(
            &mut manifest_json,
            package_name,
            groups.iter().copied(),
        )
        .wrap_err_with(|| format!("remove {package_name} from package.json"))?;
        if !removed {
            missing.push(package_name.to_string());
            continue;
        }

        removed_any = true;
        let direct_dep_path = config.modules_dir.join(package_name);
        let _ = remove_existing_path(&direct_dep_path);
    }

    if fail_when_missing && !missing.is_empty() {
        miette::bail!("Cannot remove missing dependency: {}", missing.join(", "));
    }

    fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest_json)
                .into_diagnostic()
                .wrap_err("serialize package.json after removal")?
        ),
    )
    .into_diagnostic()
    .wrap_err("save package.json")?;
    *manifest =
        PackageManifest::from_path(manifest_path).wrap_err("reload package.json after removal")?;

    Ok(removed_any)
}

fn collect_removed_entries(
    manifest: &PackageManifest,
    packages: &[String],
    groups: &[DependencyGroup],
) -> Vec<(&'static str, String, String)> {
    groups
        .iter()
        .flat_map(|group| {
            let header = match group {
                DependencyGroup::Prod => "dependencies",
                DependencyGroup::Dev => "devDependencies",
                DependencyGroup::Optional => "optionalDependencies",
                DependencyGroup::Peer => "peerDependencies",
            };
            manifest
                .dependencies(std::iter::once(*group))
                .filter(|(name, _)| packages.iter().any(|package| package == name))
                .map(move |(name, spec)| (header, name.to_string(), spec.to_string()))
        })
        .collect()
}

fn print_remove_summary(
    entries: &[(&'static str, String, String)],
    elapsed_ms: u128,
    reporter_prefix: Option<&str>,
) {
    use std::collections::BTreeMap;
    use std::io::Write;

    if entries.is_empty() {
        return;
    }

    if let Some(prefix) = reporter_prefix {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", format_prefixed_summary_stats(prefix, 0, entries.len()));
        return;
    }

    let mut grouped = BTreeMap::<&str, Vec<(String, String)>>::new();
    for (header, name, spec) in entries {
        grouped.entry(header).or_default().push((name.clone(), spec.clone()));
    }

    let total_packages = entries.len();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "Packages: -{total_packages}");
    let _ = writeln!(out, "{}", "-".repeat(total_packages.min(80)));
    let _ = writeln!(out);

    for (header, deps) in grouped {
        let _ = writeln!(out, "{header}:");
        for (name, spec) in deps {
            let _ =
                writeln!(out, "{}", format_summary_dependency_line_with_prefix('-', &name, &spec));
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "Done in {elapsed_ms}ms using pacquet v{}", env!("CARGO_PKG_VERSION"));
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
    let content = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&content).ok()?;
    value.get("name").and_then(Value::as_str).map(ToString::to_string)
}

fn remove_existing_path(path: &Path) -> miette::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    if metadata.file_type().is_symlink() {
        if metadata.is_dir() {
            fs::remove_dir(path).into_diagnostic()?;
        } else {
            fs::remove_file(path).into_diagnostic()?;
        }
        return Ok(());
    }

    if metadata.is_dir() {
        fs::remove_dir_all(path).into_diagnostic()?;
        return Ok(());
    }

    fs::remove_file(path).into_diagnostic()?;
    Ok(())
}

fn remove_dependency_from_manifest_json(
    manifest_json: &mut Value,
    name: &str,
    groups: impl IntoIterator<Item = DependencyGroup>,
) -> miette::Result<bool> {
    let Some(root) = manifest_json.as_object_mut() else {
        miette::bail!("package.json root should be an object");
    };

    let mut removed = false;
    for group in groups {
        let dependency_type: &str = group.into();
        let mut remove_field = false;
        if let Some(field) = root.get_mut(dependency_type) {
            if let Some(dependencies) = field.as_object_mut() {
                removed |= dependencies.remove(name).is_some();
                remove_field = dependencies.is_empty();
            } else {
                miette::bail!("package.json `{dependency_type}` should be an object");
            }
        }
        if remove_field {
            root.remove(dependency_type);
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn dependency_options_to_dependency_groups() {
        use DependencyGroup::{Dev, Optional, Peer, Prod};
        let groups = |opts: RemoveDependencyOptions| opts.dependency_groups();

        assert_eq!(
            groups(RemoveDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: false
            }),
            vec![Prod, Dev, Optional, Peer]
        );
        assert_eq!(
            groups(RemoveDependencyOptions {
                save_prod: true,
                save_dev: false,
                save_optional: false
            }),
            vec![Prod]
        );
        assert_eq!(
            groups(RemoveDependencyOptions {
                save_prod: false,
                save_dev: true,
                save_optional: false
            }),
            vec![Dev]
        );
        assert_eq!(
            groups(RemoveDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: true
            }),
            vec![Optional]
        );
        assert_eq!(
            groups(RemoveDependencyOptions {
                save_prod: true,
                save_dev: true,
                save_optional: false
            }),
            vec![Prod, Dev]
        );
    }
}
