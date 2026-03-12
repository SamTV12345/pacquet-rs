use crate::State;
use crate::cli_args::install::parse_install_reporter;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_executor::{ExecuteCommand, execute_command};
use pacquet_fs::{is_symlink_or_junction, symlink_dir, symlink_or_junction_target};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::Add;
use pacquet_package_manifest::DependencyGroup;
use std::{
    collections::{BTreeSet, hash_map::DefaultHasher},
    ffi::OsString,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, Args)]
pub struct DlxArgs {
    /// The package(s) to install before running the command.
    #[clap(long = "package")]
    pub package: Vec<String>,

    /// Run the command through the system shell.
    #[clap(short = 'c', long = "shell-mode")]
    pub shell_mode: bool,

    /// Reporter name.
    #[clap(long)]
    pub reporter: Option<String>,

    /// The command or package spec to run.
    pub command: String,

    /// Arguments passed to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl DlxArgs {
    pub async fn run(self, dir: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        let DlxArgs { package, shell_mode, reporter, command, args } = self;
        let reporter = parse_install_reporter(reporter.as_deref())?;
        let install_packages =
            if package.is_empty() { vec![command.clone()] } else { package.clone() };
        let temp_project_dir = prepare_dlx_workspace(npmrc, &install_packages)?;
        let manifest_path = temp_project_dir.join("package.json");

        if !manifest_path.exists() {
            write_dlx_manifest(&manifest_path)?;

            let temp_config = dlx_config_for_project(npmrc, &temp_project_dir);
            let mut state =
                State::init(manifest_path.clone(), temp_config).wrap_err("initialize dlx state")?;

            Add {
                tarball_mem_cache: &state.tarball_mem_cache,
                resolved_packages: &state.resolved_packages,
                http_client: &state.http_client,
                config: state.config,
                manifest: &mut state.manifest,
                lockfile: state.lockfile.as_ref(),
                lockfile_dir: &state.lockfile_dir,
                lockfile_importer_id: &state.lockfile_importer_id,
                workspace_packages: &state.workspace_packages,
                list_dependency_groups: || std::iter::once(DependencyGroup::Prod),
                packages: &install_packages,
                save_exact: false,
                workspace_only: false,
                reporter,
            }
            .run()
            .await
            .wrap_err("prepare temporary dlx environment")?;
        }

        let temp_config = dlx_config_for_project(npmrc, &temp_project_dir);
        let state = State::init(manifest_path.clone(), temp_config)
            .wrap_err("reload dlx state after cache preparation")?;

        let run_command = if package.is_empty() {
            resolve_default_bin_name(&state.manifest, &state.config.modules_dir)?
        } else {
            command
        };
        let extra_env = [(OsString::from("npm_command"), OsString::from("dlx"))];
        execute_command(ExecuteCommand {
            pkg_root: &temp_project_dir,
            current_dir: Some(&dir),
            program: &run_command,
            args: &args,
            extra_env: &extra_env,
            shell_mode,
        })
        .into_diagnostic()
        .wrap_err("execute dlx command")?;
        Ok(())
    }
}

fn prepare_dlx_workspace(npmrc: &Npmrc, packages: &[String]) -> miette::Result<PathBuf> {
    let dlx_cache_dir = npmrc.cache_dir.join("dlx").join(dlx_cache_key(npmrc, packages));
    let cache_link = dlx_cache_dir.join("pkg");
    if let Some(existing) = get_valid_cached_dlx_dir(&cache_link, npmrc.dlx_cache_max_age)? {
        return Ok(existing);
    }

    fs::create_dir_all(&dlx_cache_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", dlx_cache_dir.display()))?;
    let project_dir = dlx_cache_dir.join(prepare_dir_name());
    fs::create_dir_all(&project_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", project_dir.display()))?;
    replace_cache_link(&cache_link, &project_dir)?;
    Ok(project_dir)
}

fn write_dlx_manifest(manifest_path: &Path) -> miette::Result<()> {
    fs::write(
        manifest_path,
        serde_json::json!({
            "private": true
        })
        .to_string(),
    )
    .into_diagnostic()
    .wrap_err_with(|| format!("write {}", manifest_path.display()))
}

fn dlx_config_for_project(npmrc: &Npmrc, project_dir: &Path) -> &'static Npmrc {
    let mut next = npmrc.clone();
    next.modules_dir = project_dir.join("node_modules");
    next.virtual_store_dir = next.modules_dir.join(".pnpm");
    next.symlink = true;
    next.leak()
}

fn dlx_cache_key(npmrc: &Npmrc, packages: &[String]) -> String {
    let mut packages = packages.to_vec();
    packages.sort();
    let mut hasher = DefaultHasher::new();
    packages.hash(&mut hasher);
    npmrc.registry.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn prepare_dir_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    format!("{nanos:x}-{:x}", std::process::id())
}

fn get_valid_cached_dlx_dir(
    cache_link: &Path,
    max_age_minutes: u64,
) -> miette::Result<Option<PathBuf>> {
    if max_age_minutes == 0 || !cache_link.exists() {
        return Ok(None);
    }

    let target = if is_symlink_or_junction(cache_link).unwrap_or(false) {
        symlink_or_junction_target(cache_link)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", cache_link.display()))?
    } else {
        cache_link.to_path_buf()
    };
    if !target.exists() {
        return Ok(None);
    }

    let modified_at = fs::metadata(cache_link)
        .or_else(|_| fs::metadata(&target))
        .into_diagnostic()
        .wrap_err_with(|| format!("stat {}", cache_link.display()))?
        .modified()
        .into_diagnostic()
        .wrap_err_with(|| format!("read modified time for {}", cache_link.display()))?;
    let age_limit = Duration::from_secs(max_age_minutes.saturating_mul(60));
    let is_fresh =
        SystemTime::now().duration_since(modified_at).unwrap_or_else(|_| Duration::from_secs(0))
            <= age_limit;
    Ok(is_fresh.then_some(target))
}

fn replace_cache_link(cache_link: &Path, project_dir: &Path) -> miette::Result<()> {
    if cache_link.exists() {
        remove_cache_link(cache_link)?;
    }
    symlink_dir(project_dir, cache_link)
        .into_diagnostic()
        .wrap_err_with(|| format!("link {} -> {}", cache_link.display(), project_dir.display()))
}

fn remove_cache_link(cache_link: &Path) -> miette::Result<()> {
    if is_symlink_or_junction(cache_link).unwrap_or(false) {
        fs::remove_dir(cache_link)
            .into_diagnostic()
            .wrap_err_with(|| format!("remove {}", cache_link.display()))?;
        return Ok(());
    }
    if cache_link.is_dir() {
        fs::remove_dir_all(cache_link)
            .into_diagnostic()
            .wrap_err_with(|| format!("remove {}", cache_link.display()))?;
        return Ok(());
    }
    if cache_link.exists() {
        fs::remove_file(cache_link)
            .into_diagnostic()
            .wrap_err_with(|| format!("remove {}", cache_link.display()))?;
    }
    Ok(())
}

fn resolve_default_bin_name(
    manifest: &pacquet_package_manifest::PackageManifest,
    modules_dir: &Path,
) -> miette::Result<String> {
    let mut dependency_names = manifest
        .dependencies([DependencyGroup::Prod])
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    dependency_names.sort();
    let Some(package_name) = dependency_names.first() else {
        miette::bail!("dlx was unable to find the installed dependency in \"dependencies\"");
    };

    let bin_dir = modules_dir.join(".bin");
    let bin_names = collect_bin_names(&bin_dir)?;
    if bin_names.is_empty() {
        miette::bail!("No binaries found in {package_name}");
    }
    if bin_names.len() == 1 {
        return Ok(bin_names.into_iter().next().expect("checked above"));
    }

    let default_bin_name = package_name.rsplit('/').next().unwrap_or(package_name);
    if bin_names.contains(default_bin_name) {
        return Ok(default_bin_name.to_string());
    }

    let available = bin_names.into_iter().collect::<Vec<_>>();
    miette::bail!(
        "Could not determine executable to run. {package_name} has multiple binaries: {}",
        available.join(", ")
    )
}

fn collect_bin_names(bin_dir: &Path) -> miette::Result<BTreeSet<String>> {
    if !bin_dir.is_dir() {
        return Ok(BTreeSet::new());
    }

    let mut names = BTreeSet::new();
    for entry in fs::read_dir(bin_dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", bin_dir.display()))?
    {
        let entry = entry.into_diagnostic().wrap_err("read .bin entry")?;
        let file_type = entry.file_type().into_diagnostic().wrap_err("read .bin entry type")?;
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if let Some(normalized) = normalize_bin_file_name(&file_name) {
            names.insert(normalized);
        }
    }
    Ok(names)
}

fn normalize_bin_file_name(file_name: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let lower = file_name.to_ascii_lowercase();
        if let Some(stripped) = lower.strip_suffix(".cmd") {
            return Some(stripped.to_string());
        }
        if let Some(stripped) = lower.strip_suffix(".ps1") {
            return Some(stripped.to_string());
        }
        if lower.ends_with(".exe") {
            return None;
        }
        Some(file_name.to_string())
    }

    #[cfg(not(windows))]
    {
        Some(file_name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_windows_bin_wrapper_extensions() {
        #[cfg(windows)]
        {
            assert_eq!(normalize_bin_file_name("hello.cmd"), Some("hello".to_string()));
            assert_eq!(normalize_bin_file_name("hello.ps1"), Some("hello".to_string()));
        }

        #[cfg(not(windows))]
        {
            assert_eq!(normalize_bin_file_name("hello"), Some("hello".to_string()));
        }
    }

    #[test]
    fn dlx_cache_key_is_stable_for_package_order() {
        assert_eq!(
            dlx_cache_key(&Npmrc::new(), &["b".to_string(), "a".to_string()]),
            dlx_cache_key(&Npmrc::new(), &["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn dlx_cache_key_changes_with_registry() {
        let mut left = Npmrc::new();
        left.registry = "https://registry-a.example/".to_string();
        let mut right = Npmrc::new();
        right.registry = "https://registry-b.example/".to_string();

        assert_ne!(
            dlx_cache_key(&left, &["foo@1.0.0".to_string()]),
            dlx_cache_key(&right, &["foo@1.0.0".to_string()])
        );
    }
}
