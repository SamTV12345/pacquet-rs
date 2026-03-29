use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Args, Default)]
pub struct WhoamiArgs;

#[derive(Deserialize)]
struct WhoamiResponse {
    username: String,
}

impl WhoamiArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = npmrc.registry.trim_end_matches('/');
        let url = format!("{registry}/-/whoami");
        let auth = npmrc
            .auth_header_for_url(&url)
            .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))?;

        let output = Command::new("curl")
            .args(["-s", "-H"])
            .arg(format!("Authorization: {auth}"))
            .arg(&url)
            .output()
            .into_diagnostic()
            .wrap_err("request /-/whoami")?;
        let body = String::from_utf8_lossy(&output.stdout);
        let response: WhoamiResponse =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse whoami response")?;
        println!("{}", response.username);
        Ok(())
    }
}
