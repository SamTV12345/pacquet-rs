use crate::state::{collect_workspace_state_projects, find_workspace_root};
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_package_manifest::PackageManifest;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args, Default)]
pub struct PublishArgs {
    /// Directory or tarball to publish.
    target: Option<PathBuf>,

    /// Does everything a publish would do except actually publishing to the registry.
    #[arg(long)]
    dry_run: bool,

    /// Show information in JSON format.
    #[arg(long)]
    json: bool,

    /// Registers the published package with the given tag.
    #[arg(long)]
    tag: Option<String>,

    /// Tells the registry whether this package should be published as public or restricted.
    #[arg(long)]
    access: Option<String>,

    /// Ignores publish related lifecycle scripts.
    #[arg(long)]
    ignore_scripts: bool,

    /// Publish all packages from the workspace.
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Continue even if the current version may already exist.
    #[arg(long)]
    force: bool,

    /// Skip git checks (clean tree, publish branch, up-to-date remote).
    #[arg(long = "no-git-checks")]
    no_git_checks: bool,

    /// Save a summary of published packages to pnpm-publish-summary.json.
    #[arg(long = "report-summary")]
    report_summary: bool,

    /// One-time password for 2FA.
    #[arg(long)]
    otp: Option<String>,
}

impl PublishArgs {
    pub fn run(self, dir: &Path, manifest_path: PathBuf) -> miette::Result<()> {
        if self.recursive {
            return self.run_recursive(dir);
        }

        self.publish_single_package(dir, &manifest_path, None)
    }

    fn run_recursive(&self, dir: &Path) -> miette::Result<()> {
        let workspace_root = find_workspace_root(dir).ok_or_else(|| {
            miette::miette!("No pnpm-workspace.yaml found: --recursive requires a workspace")
        })?;

        let projects = collect_workspace_state_projects(&workspace_root);
        if projects.is_empty() {
            miette::bail!("No workspace packages found");
        }

        let publishable: Vec<_> = projects
            .iter()
            .filter(|project| {
                let manifest_path = project.root_dir.join("package.json");
                let Ok(manifest) = PackageManifest::from_path(manifest_path) else {
                    return false;
                };
                let value = manifest.value();
                let has_name = value.get("name").and_then(serde_json::Value::as_str).is_some();
                let has_version =
                    value.get("version").and_then(serde_json::Value::as_str).is_some();
                let is_private =
                    value.get("private").and_then(serde_json::Value::as_bool).unwrap_or(false);
                has_name && has_version && !is_private
            })
            .collect();

        if publishable.is_empty() {
            println!("No publishable packages found in workspace");
            return Ok(());
        }

        let mut summary = Vec::<serde_json::Value>::new();
        let mut failed = Vec::<String>::new();

        for project in &publishable {
            let manifest_path = project.root_dir.join("package.json");
            let name = project.name.as_deref().unwrap_or("unknown");
            let version = project.version.as_deref().unwrap_or("0.0.0");

            println!("Publishing {name}@{version}...");

            match self.publish_single_package(&project.root_dir, &manifest_path, Some(name)) {
                Ok(()) => {
                    if self.report_summary {
                        summary.push(serde_json::json!({
                            "name": name,
                            "version": version,
                            "status": "success"
                        }));
                    }
                }
                Err(err) => {
                    if self.force {
                        eprintln!("  Warning: failed to publish {name}@{version}: {err}");
                        if self.report_summary {
                            summary.push(serde_json::json!({
                                "name": name,
                                "version": version,
                                "status": "failure",
                                "error": err.to_string()
                            }));
                        }
                    } else {
                        failed.push(format!("{name}@{version}"));
                        if self.report_summary {
                            summary.push(serde_json::json!({
                                "name": name,
                                "version": version,
                                "status": "failure",
                                "error": err.to_string()
                            }));
                        }
                    }
                }
            }
        }

        if self.report_summary {
            let summary_json = serde_json::to_string_pretty(&serde_json::json!({
                "publishedPackages": summary
            }))
            .into_diagnostic()
            .wrap_err("serialize publish summary")?;
            let summary_path = dir.join("pnpm-publish-summary.json");
            std::fs::write(&summary_path, format!("{summary_json}\n"))
                .into_diagnostic()
                .wrap_err_with(|| format!("write {}", summary_path.display()))?;
            println!("Summary written to {}", summary_path.display());
        }

        if !failed.is_empty() && !self.force {
            miette::bail!("Failed to publish: {}", failed.join(", "));
        }

        Ok(())
    }

    fn publish_single_package(
        &self,
        dir: &Path,
        manifest_path: &Path,
        display_name: Option<&str>,
    ) -> miette::Result<()> {
        let manifest = PackageManifest::from_path(manifest_path.to_path_buf())
            .wrap_err("load package.json")?;
        let package_name =
            manifest.value().get("name").and_then(serde_json::Value::as_str).unwrap_or("package");
        let version =
            manifest.value().get("version").and_then(serde_json::Value::as_str).unwrap_or("0.0.0");

        // Rewrite workspace: protocol references before publishing
        let original_content = std::fs::read_to_string(manifest_path)
            .into_diagnostic()
            .wrap_err("read package.json for workspace rewriting")?;
        let needs_restore = self.rewrite_workspace_protocols(dir, manifest_path)?;

        let publish_result = self.run_npm_publish(dir, display_name, package_name, version);

        // Restore original package.json if we modified it
        if needs_restore {
            std::fs::write(manifest_path, &original_content)
                .into_diagnostic()
                .wrap_err("restore original package.json after publish")?;
        }

        publish_result
    }

    /// Rewrites `workspace:` protocol references in dependency fields of package.json.
    /// Returns `true` if the file was modified and needs to be restored after publish.
    fn rewrite_workspace_protocols(
        &self,
        dir: &Path,
        manifest_path: &Path,
    ) -> miette::Result<bool> {
        let content = std::fs::read_to_string(manifest_path)
            .into_diagnostic()
            .wrap_err("read package.json")?;
        let mut manifest: serde_json::Value =
            serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;

        let dep_fields =
            ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];

        let mut modified = false;

        // Collect workspace package versions by looking up the workspace root
        let workspace_root = find_workspace_root(dir);
        let workspace_versions: std::collections::HashMap<String, String> =
            if let Some(ref ws_root) = workspace_root {
                collect_workspace_state_projects(ws_root)
                    .into_iter()
                    .filter_map(|p| {
                        let name = p.name?;
                        let version = p.version?;
                        Some((name, version))
                    })
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

        for field in &dep_fields {
            let Some(deps) = manifest.get_mut(*field).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            for (dep_name, dep_value) in deps.iter_mut() {
                let Some(spec) = dep_value.as_str() else {
                    continue;
                };
                if !spec.starts_with("workspace:") {
                    continue;
                }
                let suffix = &spec["workspace:".len()..];
                let resolved_version = workspace_versions.get(dep_name.as_str()).cloned();
                let new_spec = match suffix {
                    "*" => {
                        // workspace:* → exact version
                        resolved_version.unwrap_or_else(|| "0.0.0".to_string())
                    }
                    "^" => {
                        // workspace:^ → ^version
                        let v = resolved_version.unwrap_or_else(|| "0.0.0".to_string());
                        format!("^{v}")
                    }
                    "~" => {
                        // workspace:~ → ~version
                        let v = resolved_version.unwrap_or_else(|| "0.0.0".to_string());
                        format!("~{v}")
                    }
                    other => {
                        // workspace:<explicit> → the explicit spec as-is (e.g. workspace:^1.0.0 → ^1.0.0)
                        other.to_string()
                    }
                };
                *dep_value = serde_json::Value::String(new_spec);
                modified = true;
            }
        }

        if modified {
            let rendered = serde_json::to_string_pretty(&manifest)
                .into_diagnostic()
                .wrap_err("serialize modified package.json")?;
            std::fs::write(manifest_path, format!("{rendered}\n"))
                .into_diagnostic()
                .wrap_err("write modified package.json")?;
        }

        Ok(modified)
    }

    fn run_npm_publish(
        &self,
        dir: &Path,
        display_name: Option<&str>,
        package_name: &str,
        version: &str,
    ) -> miette::Result<()> {
        let mut command = Command::new("npm");
        command.arg("publish");
        if self.dry_run {
            command.arg("--dry-run");
        }
        if self.json {
            command.arg("--json");
        }
        if self.ignore_scripts {
            command.arg("--ignore-scripts");
        }
        if self.force {
            command.arg("--force");
        }
        if let Some(tag) = &self.tag {
            command.args(["--tag", tag]);
        }
        if let Some(access) = &self.access {
            command.args(["--access", access]);
        }
        if let Some(otp) = &self.otp {
            command.args(["--otp", otp]);
        }
        if let Some(target) = &self.target {
            command.arg(resolve_target(dir, target));
        }
        command.current_dir(dir);

        let status = command.status().into_diagnostic().wrap_err("run npm publish")?;
        if !status.success() {
            let name = display_name.unwrap_or(package_name);
            miette::bail!(
                "publish of {name}@{version} failed with exit code {}",
                status.code().unwrap_or(1)
            );
        }

        if self.dry_run && !self.json {
            println!("Prepared publish for {package_name}@{version}");
        }
        Ok(())
    }
}

fn resolve_target(dir: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() { target.to_path_buf() } else { dir.join(target) }
}
