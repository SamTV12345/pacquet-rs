use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

#[derive(Debug, Args)]
pub struct StarArgs {
    /// Package(s) to star.
    packages: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UnstarArgs {
    /// Package(s) to unstar.
    packages: Vec<String>,
}

#[derive(Debug, Args, Default)]
pub struct StarsArgs;

impl StarArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        for package in &self.packages {
            toggle_star(package, true, npmrc)?;
        }
        Ok(())
    }
}

impl UnstarArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        for package in &self.packages {
            toggle_star(package, false, npmrc)?;
        }
        Ok(())
    }
}

impl StarsArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = npmrc.registry.trim_end_matches('/');
        let url = format!("{registry}/-/whoami");
        let auth = npmrc
            .auth_header_for_url(&url)
            .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))?;
        let output = Command::new("curl")
            .args(["-s", "-H", &format!("Authorization: {auth}"), &url])
            .output()
            .into_diagnostic()
            .wrap_err("whoami")?;
        let body = String::from_utf8_lossy(&output.stdout);
        let whoami: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;
        let username = whoami
            .get("username")
            .and_then(Value::as_str)
            .ok_or_else(|| miette::miette!("Unable to determine username"))?;

        let stars_url = format!("{registry}/-/_view/starredByUser?key=\"{username}\"");
        let output = Command::new("curl")
            .args(["-s", "-H", &format!("Authorization: {auth}"), &stars_url])
            .output()
            .into_diagnostic()
            .wrap_err("fetch stars")?;
        let body = String::from_utf8_lossy(&output.stdout);
        let stars: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;
        if let Some(rows) = stars.get("rows").and_then(Value::as_array) {
            if rows.is_empty() {
                println!("You have not starred any packages.");
            } else {
                for row in rows {
                    if let Some(name) = row.get("value").and_then(Value::as_str) {
                        println!("{name}");
                    }
                }
            }
        } else {
            println!("You have not starred any packages.");
        }
        Ok(())
    }
}

fn toggle_star(package: &str, star: bool, npmrc: &Npmrc) -> miette::Result<()> {
    let registry = npmrc.registry_for_package_name(package);
    let registry = registry.trim_end_matches('/');
    let url = format!("{registry}/{package}");
    let auth = npmrc
        .auth_header_for_url(&url)
        .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))?;

    // Fetch current document
    let output = Command::new("curl")
        .args([
            "-s",
            "-H",
            &format!("Authorization: {auth}"),
            "-H",
            "Accept: application/json",
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("fetch package")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let mut doc: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;

    // Get username from whoami
    let whoami_url = format!("{}/{}", npmrc.registry.trim_end_matches('/'), "-/whoami");
    let whoami_output = Command::new("curl")
        .args(["-s", "-H", &format!("Authorization: {auth}"), &whoami_url])
        .output()
        .into_diagnostic()
        .wrap_err("whoami")?;
    let whoami_body = String::from_utf8_lossy(&whoami_output.stdout);
    let whoami: Value = serde_json::from_str(&whoami_body).into_diagnostic().wrap_err("parse")?;
    let username = whoami
        .get("username")
        .and_then(Value::as_str)
        .ok_or_else(|| miette::miette!("Unable to determine username"))?;

    // Extract _id and _rev before mutable borrow
    let doc_id = doc.get("_id").cloned();
    let doc_rev = doc.get("_rev").cloned();

    let users = doc
        .as_object_mut()
        .and_then(|obj| {
            if !obj.contains_key("users") {
                obj.insert("users".to_string(), serde_json::json!({}));
            }
            obj.get_mut("users").and_then(Value::as_object_mut)
        })
        .ok_or_else(|| miette::miette!("cannot modify users"))?;

    if star {
        users.insert(username.to_string(), serde_json::json!(true));
    } else {
        users.remove(username);
    }

    let payload = serde_json::json!({"_id": doc_id, "_rev": doc_rev, "users": users});
    let output = Command::new("curl")
        .args([
            "-s",
            "-X",
            "PUT",
            "-H",
            &format!("Authorization: {auth}"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &serde_json::to_string(&payload).unwrap_or_default(),
            &url,
        ])
        .output()
        .into_diagnostic()
        .wrap_err("update star")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        miette::bail!("Failed to update star: {err}");
    }
    let action = if star { "starred" } else { "unstarred" };
    println!("{action} {package}");
    Ok(())
}
