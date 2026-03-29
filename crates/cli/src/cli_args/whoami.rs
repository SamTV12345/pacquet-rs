use crate::cli_args::registry_client::RegistryClient;
use clap::Args;
use pacquet_npmrc::Npmrc;

#[derive(Debug, Args, Default)]
pub struct WhoamiArgs;

impl WhoamiArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let client = RegistryClient::new(npmrc);
        let registry = client.default_registry();
        let url = format!("{registry}/-/whoami");
        client.require_auth(&url)?;

        let value = client.get_json(&url)?;
        let username = value
            .get("username")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| miette::miette!("unexpected whoami response: missing username"))?;
        println!("{username}");
        Ok(())
    }
}
