use crate::{
    State,
    cli_args::install::{InstallArgs, InstallDependencyOptions, parse_install_reporter},
    state::find_workspace_root,
};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::InstallReporter;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::tempdir;

#[derive(Debug, Args)]
pub struct DedupeArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: InstallDependencyOptions,

    /// Check if dedupe would change the lockfile without mutating the workspace.
    #[clap(long)]
    pub check: bool,

    /// Skip lifecycle scripts while deduping.
    #[clap(long)]
    pub ignore_scripts: bool,

    /// Skip staleness checks for cached metadata and prefer local metadata when possible.
    #[clap(long)]
    pub prefer_offline: bool,

    /// Disallow network requests and use only locally available lockfile/store data.
    #[clap(long)]
    pub offline: bool,

    /// Reporter name.
    #[clap(long)]
    pub reporter: Option<String>,
}

impl DedupeArgs {
    pub async fn run(self, dir: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        let reporter = self.reporter_mode()?;
        if self.check {
            return run_dedupe_check(self, dir, npmrc, reporter).await;
        }

        let workspace_root = find_workspace_root(&dir).unwrap_or_else(|| dir.clone());
        let deduped_lockfile = generate_deduped_lockfile(&self, &dir, npmrc, reporter).await?;
        fs::write(workspace_root.join("pnpm-lock.yaml"), deduped_lockfile)
            .into_diagnostic()
            .wrap_err("write deduped pnpm-lock.yaml")?;

        let state =
            State::init(dir.join("package.json"), npmrc).wrap_err("initialize dedupe state")?;
        self.install_args(false, false, reporter).run(state).await
    }

    fn install_args(
        &self,
        lockfile_only: bool,
        no_prefer_frozen_lockfile: bool,
        reporter: InstallReporter,
    ) -> InstallArgs {
        InstallArgs {
            dependency_options: InstallDependencyOptions {
                prod: self.dependency_options.prod,
                dev: self.dependency_options.dev,
                no_optional: self.dependency_options.no_optional,
            },
            frozen_lockfile: false,
            prefer_frozen_lockfile: false,
            no_prefer_frozen_lockfile,
            fix_lockfile: false,
            ignore_scripts: self.ignore_scripts,
            lockfile_only,
            force: false,
            resolution_only: false,
            reporter: Some(
                match reporter {
                    InstallReporter::Default => "default",
                    InstallReporter::AppendOnly => "append-only",
                    InstallReporter::Silent => "silent",
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

    fn reporter_mode(&self) -> miette::Result<InstallReporter> {
        parse_install_reporter(self.reporter.as_deref())
    }
}

async fn run_dedupe_check(
    args: DedupeArgs,
    dir: PathBuf,
    npmrc: &'static Npmrc,
    reporter: InstallReporter,
) -> miette::Result<()> {
    let workspace_root = find_workspace_root(&dir).unwrap_or_else(|| dir.clone());
    let relative_dir = dir.strip_prefix(&workspace_root).unwrap_or(Path::new(""));
    let temp_root =
        tempdir().into_diagnostic().wrap_err("create temporary dedupe check workspace")?;
    copy_workspace_for_check(&workspace_root, temp_root.path())?;

    let temp_dir = temp_root.path().join(relative_dir);
    let temp_config = npmrc.clone().leak();
    let state = State::init(temp_dir.join("package.json"), temp_config)
        .wrap_err("initialize dedupe check state")?;

    let original_lockfile = read_optional_file(&workspace_root.join("pnpm-lock.yaml"))?;
    let temp_lockfile =
        Some(generate_deduped_lockfile_in_temp(&args, temp_root.path(), state, reporter).await?);

    if original_lockfile != temp_lockfile {
        miette::bail!("Dedupe --check found changes to the lockfile");
    }

    Ok(())
}

async fn generate_deduped_lockfile(
    args: &DedupeArgs,
    dir: &Path,
    npmrc: &'static Npmrc,
    reporter: InstallReporter,
) -> miette::Result<String> {
    let workspace_root = find_workspace_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let relative_dir = dir.strip_prefix(&workspace_root).unwrap_or(Path::new(""));
    let temp_root = tempdir().into_diagnostic().wrap_err("create temporary dedupe workspace")?;
    copy_workspace_for_check(&workspace_root, temp_root.path())?;
    let temp_dir = temp_root.path().join(relative_dir);
    let temp_config = npmrc.clone().leak();
    let state = State::init(temp_dir.join("package.json"), temp_config)
        .wrap_err("initialize temporary dedupe state")?;
    generate_deduped_lockfile_in_temp(args, temp_root.path(), state, reporter).await
}

async fn generate_deduped_lockfile_in_temp(
    args: &DedupeArgs,
    temp_root: &Path,
    state: State,
    reporter: InstallReporter,
) -> miette::Result<String> {
    let temp_lockfile_path = temp_root.join("pnpm-lock.yaml");
    if temp_lockfile_path.exists() {
        fs::remove_file(&temp_lockfile_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("remove {}", temp_lockfile_path.display()))?;
    }

    args.install_args(true, true, reporter).run(state).await?;
    fs::read_to_string(temp_root.join("pnpm-lock.yaml"))
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", temp_root.join("pnpm-lock.yaml").display()))
}

fn read_optional_file(path: &Path) -> miette::Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        fs::read_to_string(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", path.display()))?,
    ))
}

fn copy_workspace_for_check(source: &Path, dest: &Path) -> miette::Result<()> {
    copy_workspace_dir_recursive(source, dest, Path::new(""))
}

fn copy_workspace_dir_recursive(source: &Path, dest: &Path, relative: &Path) -> miette::Result<()> {
    let current_source = source.join(relative);
    let current_dest = dest.join(relative);
    fs::create_dir_all(&current_dest)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", current_dest.display()))?;

    for entry in fs::read_dir(&current_source)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", current_source.display()))?
    {
        let entry = entry.into_diagnostic().wrap_err("read dedupe check workspace entry")?;
        let file_type = entry.file_type().into_diagnostic().wrap_err("read entry type")?;
        let entry_relative = relative.join(entry.file_name());
        if should_skip_copy(&entry_relative) {
            continue;
        }

        let entry_path = entry.path();
        let destination = dest.join(&entry_relative);
        if file_type.is_dir() {
            copy_workspace_dir_recursive(source, dest, &entry_relative)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("create {}", parent.display()))?;
            }
            fs::copy(&entry_path, &destination).into_diagnostic().wrap_err_with(|| {
                format!("copy {} -> {}", entry_path.display(), destination.display())
            })?;
        }
    }

    Ok(())
}

fn should_skip_copy(relative: &Path) -> bool {
    relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| matches!(name, "node_modules" | ".git" | "target"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_check_copy_skips_node_modules_and_git() {
        assert!(should_skip_copy(Path::new("node_modules/foo")));
        assert!(should_skip_copy(Path::new(".git/config")));
        assert!(should_skip_copy(Path::new("packages/app/target/debug")));
        assert!(!should_skip_copy(Path::new("packages/app/package.json")));
    }
}
