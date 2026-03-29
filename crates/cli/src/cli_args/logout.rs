use clap::Args;
use miette::IntoDiagnostic;
use pacquet_npmrc::Npmrc;
use std::{fs, path::PathBuf};

/// Log out from a registry.
#[derive(Debug, Args, Default)]
pub struct LogoutArgs {
    /// Base URL of the registry.
    #[arg(long)]
    registry: Option<String>,
}

impl LogoutArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = self.registry.as_deref().unwrap_or(&npmrc.registry);
        let registry = registry.trim_end_matches('/');
        let registry_key = registry.trim_start_matches("https:").trim_start_matches("http:");
        let prefix = format!("{registry_key}:_authToken=");

        let npmrc_path =
            home::home_dir().map(|h| h.join(".npmrc")).unwrap_or_else(|| PathBuf::from(".npmrc"));

        if !npmrc_path.is_file() {
            println!("Not logged in to {registry}");
            return Ok(());
        }

        let content = fs::read_to_string(&npmrc_path).into_diagnostic()?;
        let original_len = content.lines().count();
        let lines: Vec<&str> = content.lines().filter(|l| !l.starts_with(&prefix)).collect();
        let new_content = lines.join("\n") + "\n";

        if lines.len() < original_len {
            fs::write(&npmrc_path, new_content).into_diagnostic()?;
            println!("Logged out from {registry}");
        } else {
            println!("Not logged in to {registry}");
        }
        Ok(())
    }
}
