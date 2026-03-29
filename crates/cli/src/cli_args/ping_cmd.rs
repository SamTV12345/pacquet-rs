use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::{process::Command, time::Instant};

#[derive(Debug, Args, Default)]
pub struct PingArgs;

impl PingArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = npmrc.registry.trim_end_matches('/');
        let url = format!("{registry}/-/ping");
        let start = Instant::now();

        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-o", "/dev/null", "-w", "%{http_code}"]);
        if let Some(auth) = npmrc.auth_header_for_url(&url) {
            cmd.args(["-H", &format!("Authorization: {auth}")]);
        }
        cmd.arg(&url);

        let output = cmd.output().into_diagnostic().wrap_err("ping registry")?;
        let elapsed = start.elapsed();
        let status = String::from_utf8_lossy(&output.stdout);

        println!("Ping to {registry}");
        println!("  HTTP status: {status}");
        println!("  Response time: {}ms", elapsed.as_millis());
        Ok(())
    }
}
