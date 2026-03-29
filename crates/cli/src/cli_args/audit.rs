use clap::Args;
use miette::{Context, IntoDiagnostic};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args, Default)]
pub struct AuditArgs {
    /// Output audit report in JSON format.
    #[arg(long)]
    json: bool,

    /// Only print advisories with severity greater than or equal to the provided one.
    #[arg(long = "audit-level")]
    audit_level: Option<String>,

    /// Only audit devDependencies.
    #[arg(short = 'D', long)]
    dev: bool,

    /// Only audit dependencies and optionalDependencies.
    #[arg(short = 'P', long = "prod")]
    prod: bool,

    /// Don't audit optionalDependencies.
    #[arg(long = "no-optional")]
    no_optional: bool,

    /// Use exit code 0 if the registry responds with an error.
    #[arg(long = "ignore-registry-errors")]
    ignore_registry_errors: bool,

    /// Add overrides to package.json to force non-vulnerable versions.
    #[arg(long)]
    fix: bool,
}

impl AuditArgs {
    pub fn run(self, dir: PathBuf) -> miette::Result<()> {
        if !dir.join("pnpm-lock.yaml").is_file() {
            miette::bail!("No pnpm-lock.yaml found: Cannot audit a project without a lockfile");
        }

        if self.fix {
            return self.run_fix(&dir);
        }

        let mut command = Command::new("npm");
        command.arg("audit");
        if self.json {
            command.arg("--json");
        }
        if let Some(level) = &self.audit_level {
            command.args(["--audit-level", level]);
        }
        if self.dev && !self.prod {
            command.args(["--omit", "prod"]);
        } else if self.prod && !self.dev {
            command.args(["--omit", "dev"]);
        }
        if self.no_optional {
            command.args(["--omit", "optional"]);
        }
        command.current_dir(dir);

        let status = command.status().into_diagnostic().wrap_err("run npm audit")?;
        if !status.success() && !self.ignore_registry_errors {
            miette::bail!("audit reported issues");
        }
        Ok(())
    }

    fn run_fix(&self, dir: &Path) -> miette::Result<()> {
        let mut command = Command::new("npm");
        command.args(["audit", "--json"]);
        if self.dev && !self.prod {
            command.args(["--omit", "prod"]);
        } else if self.prod && !self.dev {
            command.args(["--omit", "dev"]);
        }
        if self.no_optional {
            command.args(["--omit", "optional"]);
        }
        command.current_dir(dir);

        let output = command.output().into_diagnostic().wrap_err("run npm audit --json")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let report: AuditReport =
            serde_json::from_str(&stdout).into_diagnostic().wrap_err("parse audit report")?;

        let overrides = create_overrides(&report);
        if overrides.is_empty() {
            println!("No fixable vulnerabilities found");
            return Ok(());
        }

        let manifest_path = dir.join("package.json");
        write_overrides(&manifest_path, &overrides)?;

        println!("Added overrides to package.json:");
        for (selector, patched) in &overrides {
            println!("  {selector}: {patched}");
        }
        println!("\nRun `pacquet install` to apply the overrides.");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct AuditReport {
    #[serde(default)]
    advisories: BTreeMap<String, AuditAdvisory>,
}

#[derive(Debug, Deserialize)]
struct AuditAdvisory {
    module_name: String,
    vulnerable_versions: String,
    patched_versions: String,
}

fn create_overrides(report: &AuditReport) -> BTreeMap<String, String> {
    report
        .advisories
        .values()
        .filter(|advisory| {
            advisory.vulnerable_versions != ">=0.0.0"
                && advisory.patched_versions != "<0.0.0"
                && !advisory.patched_versions.is_empty()
        })
        .map(|advisory| {
            (
                format!("{}@{}", advisory.module_name, advisory.vulnerable_versions),
                advisory.patched_versions.clone(),
            )
        })
        .collect()
}

fn write_overrides(
    manifest_path: &Path,
    overrides: &BTreeMap<String, String>,
) -> miette::Result<()> {
    let content = fs::read_to_string(manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let mut value: Value =
        serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;

    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    let pnpm = root.entry("pnpm").or_insert_with(|| Value::Object(Map::new()));
    let pnpm = pnpm
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;
    let existing_overrides = pnpm.entry("overrides").or_insert_with(|| Value::Object(Map::new()));
    let existing_overrides = existing_overrides
        .as_object_mut()
        .ok_or_else(|| miette::miette!("pnpm.overrides must be an object"))?;

    for (selector, patched) in overrides {
        existing_overrides.insert(selector.clone(), Value::String(patched.clone()));
    }

    let rendered = serde_json::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("serialize package.json")?;
    fs::write(manifest_path, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", manifest_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_overrides_filters_unfixable() {
        let report: AuditReport = serde_json::from_value(serde_json::json!({
            "advisories": {
                "1": {
                    "module_name": "axios",
                    "vulnerable_versions": "<=0.18.0",
                    "patched_versions": ">=0.18.1"
                },
                "2": {
                    "module_name": "unfixable",
                    "vulnerable_versions": ">=0.0.0",
                    "patched_versions": "<0.0.0"
                },
                "3": {
                    "module_name": "no-patch",
                    "vulnerable_versions": "<1.0.0",
                    "patched_versions": ""
                }
            }
        }))
        .unwrap();

        let overrides = create_overrides(&report);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides.get("axios@<=0.18.0").unwrap(), ">=0.18.1");
    }
}
