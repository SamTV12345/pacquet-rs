use std::env::temp_dir;

use clap::Parser;
use pacquet_registry_mock::{MockInstanceOptions, PreparedRegistryInfo};
use portpicker::pick_unused_port;
use reqwest::Client;
use tokio::time::Duration;

/// Launch a single mocked registry server to be used in tests.
///
/// This step is optional, but would help in machine with few CPU cores.
#[derive(Debug, Parser)]
enum Cli {
    /// Start a single mocked registry server.
    #[clap(alias = "prepare")]
    Launch,
    /// Terminate the launched mocked registry server.
    #[clap(alias = "stop")]
    End,
}

#[tokio::main]
async fn main() {
    match Cli::parse() {
        Cli::Launch => launch().await,
        Cli::End => end(),
    };
}

async fn launch() {
    let stdout = temp_dir().join("pacquet-registry-mock-prepared.stdout.log");
    let stderr = temp_dir().join("pacquet-registry-mock-prepared.stderr.log");
    let options = MockInstanceOptions {
        client: &Client::new(),
        port: pick_unused_port().expect("pick an unused port"),
        stdout: Some(&stdout),
        stderr: Some(&stderr),
        max_retries: 40,
        retry_delay: Duration::from_millis(1000),
        request_timeout: Duration::from_secs(5),
    };
    let saved_info = PreparedRegistryInfo::launch(options).await;
    dbg!(&saved_info, &stdout, &stderr);
}

fn end() {
    let deleted_info = PreparedRegistryInfo::end();
    dbg!(&deleted_info);
}
