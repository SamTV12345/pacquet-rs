use crate::cli_args::registry_client::RegistryClient;
use clap::Args;
use pacquet_npmrc::Npmrc;
use std::time::Instant;

#[derive(Debug, Args, Default)]
pub struct PingArgs;

impl PingArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let client = RegistryClient::new(npmrc);
        let registry = client.default_registry();
        let url = format!("{registry}/-/ping");
        let start = Instant::now();

        let response = client.get(&url)?;
        let elapsed = start.elapsed();
        let status = response.status();

        println!("Ping to {registry}");
        println!("  HTTP status: {status}");
        println!("  Response time: {}ms", elapsed.as_millis());
        Ok(())
    }
}
