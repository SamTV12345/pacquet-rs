use clap::Args;
use miette::IntoDiagnostic;
use std::env;

#[derive(Debug, Args, Default)]
pub struct DoctorArgs;

impl DoctorArgs {
    pub fn run(self) -> miette::Result<()> {
        let dir = env::current_dir().into_diagnostic()?;
        let mut issues = Vec::new();

        if dir.join("node_modules").is_dir() && !dir.join("pnpm-lock.yaml").is_file() {
            issues.push("`node_modules` exists but `pnpm-lock.yaml` is missing".to_string());
        }
        if dir.join("pnpm-lock.yaml").is_file() && !dir.join("package.json").is_file() {
            issues.push("`pnpm-lock.yaml` exists but `package.json` is missing".to_string());
        }

        if issues.is_empty() {
            println!("No known issues were detected.");
        } else {
            for issue in issues {
                println!("WARN: {issue}");
            }
        }
        Ok(())
    }
}
