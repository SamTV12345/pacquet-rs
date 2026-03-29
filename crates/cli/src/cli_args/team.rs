use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::process::Command;

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
    let registry = npmrc.registry.trim_end_matches('/');
    let (scope, team_name) =
        team.split_once(':').ok_or_else(|| miette::miette!("Team must be in format scope:team"))?;
    let url = match user {
        Some(_) => format!("{registry}/-/team/{scope}/{team_name}/user"),
        None => format!("{registry}/-/team/{scope}/{team_name}"),
    };
    let auth =
        npmrc.auth_header_for_url(&url).ok_or_else(|| miette::miette!("Not authenticated."))?;
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-X", method, "-H", &format!("Authorization: {auth}")]);
    if let Some(u) = user {
        let payload = serde_json::json!({"user": u});
        cmd.args([
            "-H",
            "Content-Type: application/json",
            "-d",
            &serde_json::to_string(&payload).unwrap_or_default(),
        ]);
    }
    cmd.arg(&url);
    let output = cmd.output().into_diagnostic().wrap_err("team operation")?;
    if !output.status.success() {
        miette::bail!("Team operation failed");
    }
    match user {
        Some(u) => println!("{verb} team {team}: {u}"),
        None => println!("{verb} team {team}"),
    }
    Ok(())
}

fn list_team(team: &str, npmrc: &Npmrc) -> miette::Result<()> {
    let registry = npmrc.registry.trim_end_matches('/');
    let url = if team.contains(':') {
        let (scope, team_name) = team.split_once(':').unwrap();
        format!("{registry}/-/team/{scope}/{team_name}/user")
    } else {
        format!("{registry}/-/org/{team}/team")
    };
    let auth =
        npmrc.auth_header_for_url(&url).ok_or_else(|| miette::miette!("Not authenticated."))?;
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
        .wrap_err("list team")?;
    let body = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&body).into_diagnostic().wrap_err("parse")?;
    if let Some(arr) = value.as_array() {
        for item in arr {
            println!("{}", item.as_str().unwrap_or("?"));
        }
    }
    Ok(())
}
