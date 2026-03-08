use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_executor::{ExecuteLifecycleScript, execute_lifecycle_script};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use std::{
    env as std_env,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// A pre-defined package script.
    pub command: String,

    /// Any additional arguments passed after the script name.
    pub args: Vec<String>,

    /// Avoid exiting with a non-zero exit code when the script is undefined.
    #[clap(long)]
    pub if_present: bool,
}

impl RunArgs {
    /// Execute the subcommand.
    pub fn run(self, manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
        run_named_script(manifest_path, &self.command, &self.args, self.if_present, false, config)
    }
}

pub fn run_test(manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
    run_named_script(manifest_path, "test", &[], false, false, config)
}

pub fn run_start(manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
    run_named_script(manifest_path, "start", &[], false, true, config)
}

pub fn run_named_script(
    manifest_path: PathBuf,
    script_name: &str,
    passed_thru_args: &[String],
    if_present: bool,
    start_fallback: bool,
    config: &Npmrc,
) -> miette::Result<()> {
    let manifest = PackageManifest::from_path(manifest_path.clone())
        .wrap_err("getting the package.json in current directory")?;
    let package_dir =
        manifest_path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let init_cwd = std_env::current_dir().into_diagnostic().wrap_err("get current directory")?;

    let script =
        resolve_script_text(&manifest, &package_dir, script_name, if_present, start_fallback)?;
    let Some(script) = script else {
        return Ok(());
    };

    if config.enable_pre_post_scripts {
        let pre_name = format!("pre{script_name}");
        if !script.contains(&pre_name)
            && let Some(pre_script) = manifest.script(&pre_name, true)?
        {
            execute_script(
                &manifest_path,
                &package_dir,
                &init_cwd,
                &pre_name,
                pre_script,
                &[],
                config,
            )?;
        }
    }

    execute_script(
        &manifest_path,
        &package_dir,
        &init_cwd,
        script_name,
        &script,
        passed_thru_args,
        config,
    )?;

    if config.enable_pre_post_scripts {
        let post_name = format!("post{script_name}");
        if !script.contains(&post_name)
            && let Some(post_script) = manifest.script(&post_name, true)?
        {
            execute_script(
                &manifest_path,
                &package_dir,
                &init_cwd,
                &post_name,
                post_script,
                &[],
                config,
            )?;
        }
    }

    Ok(())
}

fn resolve_script_text(
    manifest: &PackageManifest,
    package_dir: &Path,
    script_name: &str,
    if_present: bool,
    start_fallback: bool,
) -> miette::Result<Option<String>> {
    if let Some(script) = manifest.script(script_name, true)? {
        return Ok(Some(script.to_string()));
    }

    if start_fallback && script_name == "start" {
        let server_js = package_dir.join("server.js");
        if server_js.is_file() {
            return Ok(Some("node server.js".to_string()));
        }
        miette::bail!("Missing script start or file server.js");
    }

    if if_present {
        Ok(None)
    } else {
        manifest
            .script(script_name, false)
            .map(|value| value.map(str::to_string))
            .map_err(Into::into)
    }
}

fn execute_script(
    manifest_path: &std::path::Path,
    package_dir: &std::path::Path,
    init_cwd: &std::path::Path,
    script_name: &str,
    script: &str,
    args: &[String],
    config: &Npmrc,
) -> miette::Result<()> {
    execute_lifecycle_script(ExecuteLifecycleScript {
        pkg_root: package_dir,
        package_json_path: manifest_path,
        script_name,
        script,
        args,
        script_shell: config.script_shell.as_deref(),
        shell_emulator: config.shell_emulator,
        init_cwd,
    })
    .wrap_err_with(|| format!("executing script `{script_name}`"))?;
    Ok(())
}
