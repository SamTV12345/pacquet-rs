use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Open the bug tracker page for a package.
#[derive(Debug, Args)]
pub struct BugsArgs {
    /// Package name (defaults to current project).
    package: Option<String>,
}

impl BugsArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        let manifest = load_package_manifest(&dir, self.package.as_deref())?;
        let url = extract_bugs_url(&manifest)
            .or_else(|| extract_repo_url(&manifest).map(|u| format!("{u}/issues")))
            .ok_or_else(|| miette::miette!("No bugs URL found in package.json"))?;
        println!("{url}");
        open_url(&url);
        Ok(())
    }
}

fn extract_bugs_url(value: &Value) -> Option<String> {
    value.get("bugs").and_then(|bugs| {
        bugs.as_str()
            .map(ToString::to_string)
            .or_else(|| bugs.get("url").and_then(Value::as_str).map(ToString::to_string))
    })
}

pub(crate) fn extract_repo_url(value: &Value) -> Option<String> {
    value.get("repository").and_then(|repo| {
        let raw = repo
            .as_str()
            .map(ToString::to_string)
            .or_else(|| repo.get("url").and_then(Value::as_str).map(ToString::to_string))?;
        Some(normalize_git_url(&raw))
    })
}

fn normalize_git_url(url: &str) -> String {
    let url = url
        .trim_start_matches("git+")
        .trim_start_matches("git://")
        .trim_start_matches("ssh://git@")
        .trim_end_matches(".git");
    if url.starts_with("github.com")
        || url.starts_with("gitlab.com")
        || url.starts_with("bitbucket.org")
    {
        format!("https://{url}")
    } else if url.starts_with("git@") {
        let url = url.trim_start_matches("git@").replace(':', "/");
        format!("https://{url}")
    } else {
        url.to_string()
    }
}

pub(crate) fn load_package_manifest(dir: &Path, package: Option<&str>) -> miette::Result<Value> {
    let manifest_path = match package {
        Some(name) => {
            let mut path = dir.join("node_modules");
            for segment in name.split('/') {
                path.push(segment);
            }
            path.join("package.json")
        }
        None => dir.join("package.json"),
    };
    if !manifest_path.is_file() {
        miette::bail!("Cannot find {}", manifest_path.display());
    }
    let content = fs::read_to_string(&manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")
}

pub(crate) fn open_url(url: &str) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/c", "start", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };
}

pub(crate) use extract_repo_url as get_repo_url;
