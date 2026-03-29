use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

/// Display registry information about a package.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Package name (optionally with version: pkg@version).
    package: String,

    /// Output in JSON format.
    #[arg(long)]
    json: bool,
}

impl InfoArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let (name, version) = parse_name_version(&self.package);
        let registry = npmrc.registry_for_package_name(&name);
        let registry = registry.trim_end_matches('/');
        let url = match &version {
            Some(v) => format!("{registry}/{name}/{v}"),
            None => format!("{registry}/{name}"),
        };

        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-H", "Accept: application/json"]);
        if let Some(auth) = npmrc.auth_header_for_url(&url) {
            cmd.args(["-H", &format!("Authorization: {auth}")]);
        }
        cmd.arg(&url);

        let output = cmd.output().into_diagnostic().wrap_err("fetch package info")?;
        if !output.status.success() {
            miette::bail!("Failed to fetch package info for {}", self.package);
        }

        let body = String::from_utf8_lossy(&output.stdout);
        let value: Value =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse registry response")?;

        if value.get("error").is_some() {
            let message = value.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            miette::bail!("{message}");
        }

        if self.json {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            return Ok(());
        }

        print_package_info(&value, version.is_some());
        Ok(())
    }
}

fn parse_name_version(spec: &str) -> (String, Option<String>) {
    if let Some(rest) = spec.strip_prefix('@') {
        if let Some((scope_and_name, ver)) = rest.rsplit_once('@') {
            return (format!("@{scope_and_name}"), Some(ver.to_string()));
        }
        return (spec.to_string(), None);
    }
    if let Some((name, ver)) = spec.rsplit_once('@')
        && !name.is_empty() {
            return (name.to_string(), Some(ver.to_string()));
        }
    (spec.to_string(), None)
}

fn print_package_info(value: &Value, is_version: bool) {
    if is_version {
        print_version_info(value);
    } else {
        print_full_info(value);
    }
}

fn print_version_info(value: &Value) {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("?");
    let version = value.get("version").and_then(Value::as_str).unwrap_or("?");
    let description = value.get("description").and_then(Value::as_str).unwrap_or("");
    let license = value.get("license").and_then(Value::as_str).unwrap_or("UNLICENSED");
    let homepage = value.get("homepage").and_then(Value::as_str);

    println!(
        "{name}@{version} | {license} | deps: {} | versions: ?",
        value.get("dependencies").and_then(Value::as_object).map_or(0, |d| d.len())
    );
    if !description.is_empty() {
        println!("{description}");
    }
    if let Some(hp) = homepage {
        println!("{hp}");
    }
    println!();
    if let Some(deps) = value.get("dependencies").and_then(Value::as_object) {
        println!("dependencies:");
        for (dep_name, dep_version) in deps {
            println!("  {dep_name}: {}", dep_version.as_str().unwrap_or("?"));
        }
    }
    if let Some(bin) = value.get("bin") {
        println!("\nbin: {}", serde_json::to_string(bin).unwrap_or_default());
    }
}

fn print_full_info(value: &Value) {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("?");
    let description = value.get("description").and_then(Value::as_str).unwrap_or("");
    let dist_tags = value.get("dist-tags").and_then(Value::as_object);
    let latest = dist_tags.and_then(|t| t.get("latest")).and_then(Value::as_str).unwrap_or("?");
    let license = value.get("license").and_then(Value::as_str).unwrap_or("UNLICENSED");
    let homepage = value.get("homepage").and_then(Value::as_str);

    println!("{name}@{latest} | {license}");
    if !description.is_empty() {
        println!("{description}");
    }
    if let Some(hp) = homepage {
        println!("{hp}");
    }
    println!();
    if let Some(tags) = dist_tags {
        println!("dist-tags:");
        for (tag, ver) in tags {
            println!("  {tag}: {}", ver.as_str().unwrap_or("?"));
        }
    }
    if let Some(versions) = value.get("versions").and_then(Value::as_object) {
        let version_list: Vec<&str> = versions.keys().map(String::as_str).collect();
        let last_ten: Vec<&&str> = version_list.iter().rev().take(10).collect();
        println!("\nversions ({} total):", version_list.len());
        for v in last_ten.into_iter().rev() {
            println!("  {v}");
        }
    }
    if let Some(maintainers) = value.get("maintainers").and_then(Value::as_array) {
        println!("\nmaintainers:");
        for m in maintainers {
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                let email = m.get("email").and_then(Value::as_str).unwrap_or("");
                if email.is_empty() {
                    println!("  {name}");
                } else {
                    println!("  {name} <{email}>");
                }
            }
        }
    }
}
