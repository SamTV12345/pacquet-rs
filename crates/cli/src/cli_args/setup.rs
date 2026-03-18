use crate::cli_args::bin::global_bin_dir;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Override existing launcher files.
    #[arg(short = 'f', long)]
    force: bool,
}

impl SetupArgs {
    pub fn run(self) -> miette::Result<()> {
        let target_dir = setup_home_dir()?;
        fs::create_dir_all(&target_dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", target_dir.display()))?;
        write_launcher_scripts(&target_dir, self.force)?;
        println!("{}", render_setup_output(&target_dir));
        Ok(())
    }
}

fn setup_home_dir() -> miette::Result<PathBuf> {
    if let Some(home) = env::var_os("PNPM_HOME").or_else(|| env::var_os("PACQUET_HOME")) {
        return Ok(PathBuf::from(home));
    }

    #[cfg(windows)]
    {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(local_app_data).join("pnpm"));
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(home) = home::home_dir() {
            return Ok(home.join(".local").join("share").join("pnpm"));
        }
    }

    global_bin_dir()
}

fn write_launcher_scripts(target_dir: &Path, force: bool) -> miette::Result<()> {
    let exe_path = env::current_exe().into_diagnostic().wrap_err("resolve current executable")?;
    let launchers = launcher_specs(&exe_path);
    for (file_name, contents) in launchers {
        write_launcher_file(&target_dir.join(file_name), &contents, force)?;
    }
    Ok(())
}

fn write_launcher_file(path: &Path, contents: &str, force: bool) -> miette::Result<()> {
    if path.exists() && !force {
        let existing = fs::read_to_string(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", path.display()))?;
        if existing == contents {
            return Ok(());
        }
        miette::bail!(
            "{} already exists with different contents. Re-run with --force to overwrite it.",
            path.display()
        );
    }

    fs::write(path, contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("stat {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .into_diagnostic()
            .wrap_err_with(|| format!("chmod {}", path.display()))?;
    }

    Ok(())
}

fn launcher_specs(exe_path: &Path) -> Vec<(String, String)> {
    let exe = exe_path.display().to_string();

    #[cfg(windows)]
    {
        vec![
            ("pnpm.cmd".to_string(), format!("@echo off\r\n\"{exe}\" %*\r\n")),
            ("pnpm.ps1".to_string(), format!("& \"{exe}\" @args\r\n")),
            ("pnpx.cmd".to_string(), format!("@echo off\r\n\"{exe}\" dlx %*\r\n")),
            ("pnpx.ps1".to_string(), format!("& \"{exe}\" dlx @args\r\n")),
        ]
    }

    #[cfg(not(windows))]
    {
        vec![
            ("pnpm".to_string(), format!("#!/bin/sh\nexec \"{exe}\" \"$@\"\n")),
            ("pnpx".to_string(), format!("#!/bin/sh\nexec \"{exe}\" dlx \"$@\"\n")),
        ]
    }
}

fn render_setup_output(target_dir: &Path) -> String {
    #[cfg(windows)]
    {
        format!(
            "Created launcher scripts in {}\nSet PNPM_HOME to {}\nAdd PNPM_HOME to PATH, for example:\nsetx PNPM_HOME \"{}\"\nsetx PATH \"%PNPM_HOME%;%PATH%\"",
            target_dir.display(),
            target_dir.display(),
            target_dir.display()
        )
    }

    #[cfg(not(windows))]
    {
        format!(
            "Created launcher scripts in {}\nExport PNPM_HOME and add it to PATH, for example:\nexport PNPM_HOME=\"{}\"\nexport PATH=\"$PNPM_HOME:$PATH\"",
            target_dir.display(),
            target_dir.display()
        )
    }
}
