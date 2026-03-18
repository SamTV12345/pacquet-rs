use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, process};

#[derive(Debug, Args, Default)]
pub struct ServerArgs {
    /// One of start, stop, or status.
    command: Option<String>,

    /// Runs the server in the background.
    #[arg(long)]
    background: bool,

    /// The communication protocol used by the server.
    #[arg(long, default_value = "tcp")]
    protocol: String,

    /// The port number to use when TCP is used for communication.
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerState {
    pid: u32,
    background: bool,
    protocol: String,
    port: Option<u16>,
}

impl ServerArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let state_file = npmrc.store_dir.version_dir().join("server-state.json");
        match self.command.as_deref() {
            Some("start") => start_server(&state_file, self),
            Some("stop") => stop_server(&state_file),
            Some("status") => status_server(&state_file),
            Some(command) => {
                miette::bail!(
                    "\"server {command}\" is not a pacquet command. See \"pacquet help server\"."
                )
            }
            None => Ok(()),
        }
    }
}

fn start_server(state_file: &PathBuf, args: ServerArgs) -> miette::Result<()> {
    if let Some(parent) = state_file.parent() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", parent.display()))?;
    }
    let state = ServerState {
        pid: process::id(),
        background: args.background,
        protocol: args.protocol,
        port: args.port,
    };
    let rendered = serde_json::to_string_pretty(&state)
        .into_diagnostic()
        .wrap_err("serialize server state")?;
    fs::write(state_file, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", state_file.display()))?;
    println!("Store server started");
    Ok(())
}

fn stop_server(state_file: &PathBuf) -> miette::Result<()> {
    if !state_file.is_file() {
        println!("No server is running");
        return Ok(());
    }
    fs::remove_file(state_file)
        .into_diagnostic()
        .wrap_err_with(|| format!("remove {}", state_file.display()))?;
    println!("Store server stopped");
    Ok(())
}

fn status_server(state_file: &PathBuf) -> miette::Result<()> {
    if !state_file.is_file() {
        println!("No server is running");
        return Ok(());
    }
    let content = fs::read_to_string(state_file)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", state_file.display()))?;
    let state: ServerState =
        serde_json::from_str(&content).into_diagnostic().wrap_err("parse server state")?;
    println!(
        "Store server is running (pid={}, protocol={}, port={})",
        state.pid,
        state.protocol,
        state.port.map(|port| port.to_string()).unwrap_or_else(|| "auto".to_string())
    );
    Ok(())
}
