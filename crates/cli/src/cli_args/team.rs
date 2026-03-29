use clap::{Args, Subcommand};
use pacquet_npmrc::Npmrc;

use crate::cli_args::registry_client::RegistryClient;

#[derive(Debug, Args)]
pub struct TeamArgs {
    #[clap(subcommand)]
    command: TeamCommand,
}

#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    /// Create a new team.
    Create { team: String },
    /// Destroy a team.
    Destroy { team: String },
    /// Add a user to a team.
    Add { team: String, user: String },
    /// Remove a user from a team.
    Rm { team: String, user: String },
    /// List teams or team members.
    #[clap(alias = "list")]
    Ls { team: String },
}

impl TeamArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        match self.command {
            TeamCommand::Create { team } => team_action("PUT", &team, None, npmrc, "Created"),
            TeamCommand::Destroy { team } => team_action("DELETE", &team, None, npmrc, "Destroyed"),
            TeamCommand::Add { team, user } => {
                team_action("PUT", &team, Some(&user), npmrc, "Added user to")
            }
            TeamCommand::Rm { team, user } => {
                team_action("DELETE", &team, Some(&user), npmrc, "Removed user from")
            }
            TeamCommand::Ls { team } => list_team(&team, npmrc),
        }
    }
}

fn team_action(
    method: &str,
    team: &str,
    user: Option<&str>,
    npmrc: &Npmrc,
    verb: &str,
) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let (scope, team_name) =
        team.split_once(':').ok_or_else(|| miette::miette!("Team must be in format scope:team"))?;
    let url = match user {
        Some(_) => format!("{registry}/-/team/{scope}/{team_name}/user"),
        None => format!("{registry}/-/team/{scope}/{team_name}"),
    };
    client.require_auth(&url)?;

    let resp = match (method, user) {
        ("PUT", Some(u)) => {
            let payload = serde_json::json!({"user": u});
            client.put_json(&url, &payload)?
        }
        ("PUT", None) => {
            let payload = serde_json::json!({});
            client.put_json(&url, &payload)?
        }
        ("DELETE", _) => client.delete(&url)?,
        _ => miette::bail!("Unsupported method: {method}"),
    };

    if !resp.status().is_success() {
        miette::bail!("Team operation failed");
    }
    match user {
        Some(u) => println!("{verb} team {team}: {u}"),
        None => println!("{verb} team {team}"),
    }
    Ok(())
}

fn list_team(team: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let client = RegistryClient::new(npmrc);
    let registry = npmrc.registry.trim_end_matches('/');
    let url = if team.contains(':') {
        let (scope, team_name) = team.split_once(':').unwrap();
        format!("{registry}/-/team/{scope}/{team_name}/user")
    } else {
        format!("{registry}/-/org/{team}/team")
    };
    client.require_auth(&url)?;
    let value = client.get_json(&url)?;
    if let Some(arr) = value.as_array() {
        for item in arr {
            println!("{}", item.as_str().unwrap_or("?"));
        }
    }
    Ok(())
}
