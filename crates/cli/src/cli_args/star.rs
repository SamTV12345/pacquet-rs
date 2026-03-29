use clap::Args;
use pacquet_npmrc::Npmrc;
use serde_json::Value;

use crate::cli_args::registry_client::RegistryClient;

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
        let client = RegistryClient::new(npmrc);
        let registry = client.default_registry();
        let whoami_url = format!("{registry}/-/whoami");
        let whoami = client.get_json(&whoami_url)?;
        let username = whoami
            .get("username")
            .and_then(Value::as_str)
            .ok_or_else(|| miette::miette!("Unable to determine username"))?;

        let stars_url = format!("{registry}/-/_view/starredByUser?key=\"{username}\"");
        let stars = client.get_json(&stars_url)?;
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
    let client = RegistryClient::new(npmrc);
    let registry = client.registry_url(package);
    let url = format!("{registry}/{package}");

    // Fetch current document
    let mut doc = client.get_json(&url)?;

    // Get username from whoami
    let default_registry = client.default_registry();
    let whoami_url = format!("{default_registry}/-/whoami");
    let whoami = client.get_json(&whoami_url)?;
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
    client.put_json(&url, &payload)?;
    let action = if star { "starred" } else { "unstarred" };
    println!("{action} {package}");
    Ok(())
}
