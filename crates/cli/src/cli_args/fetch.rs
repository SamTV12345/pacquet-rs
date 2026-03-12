use crate::cli_args::install::parse_install_reporter;
use crate::state::find_workspace_root;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{Lockfile, ProjectSnapshot, RootProjectSnapshot};
use pacquet_network::{RegistryTlsConfig, ThrottledClient, ThrottledClientOptions};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::{
    InstallFrozenLockfile, ResolvedPackages, finish_progress_reporter, start_progress_reporter,
};
use pacquet_package_manifest::DependencyGroup;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn network_tls_configs(config: &Npmrc) -> std::collections::HashMap<String, RegistryTlsConfig> {
    config
        .ssl_configs
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                RegistryTlsConfig {
                    ca: value.ca.clone(),
                    cert: value.cert.clone(),
                    key: value.key.clone(),
                },
            )
        })
        .collect()
}

#[derive(Debug, Default, Args)]
pub struct FetchDependencyOptions {
    /// Development packages will not be fetched.
    #[arg(short = 'P', long)]
    pub prod: bool,
    /// Only development packages will be fetched.
    #[arg(short = 'D', long)]
    pub dev: bool,
}

impl FetchDependencyOptions {
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let has_both = self.prod == self.dev;
        let has_prod = has_both || self.prod;
        let has_dev = has_both || self.dev;
        let has_optional = has_prod;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Prod))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Debug, Args)]
pub struct FetchArgs {
    #[clap(flatten)]
    dependency_options: FetchDependencyOptions,
    /// Reporter name.
    #[clap(long)]
    reporter: Option<String>,
    /// Disable pnpm hooks defined in .pnpmfile.cjs.
    #[clap(long)]
    ignore_pnpmfile: bool,
    /// Use hooks from the specified pnpmfile instead of <lockfileDir>/.pnpmfile.cjs.
    #[clap(long)]
    pnpmfile: Option<PathBuf>,
}

impl FetchArgs {
    pub async fn run(self, dir: PathBuf, config: &'static Npmrc) -> miette::Result<()> {
        let reporter = parse_install_reporter(self.reporter.as_deref())?;
        let ignore_pnpmfile = self.ignore_pnpmfile;
        let pnpmfile = self.pnpmfile;
        let lockfile_dir = find_workspace_root(&dir).unwrap_or(dir);
        let lockfile = Lockfile::load_from_dir(&lockfile_dir)
            .wrap_err_with(|| format!("load lockfile from {}", lockfile_dir.display()))?
            .ok_or_else(|| {
                miette::miette!("No pnpm-lock.yaml found in {}", lockfile_dir.display())
            })?;
        let project_snapshot =
            select_fetch_project_snapshot(&lockfile.project_snapshot, &lockfile_dir)?;

        let staging_dir =
            tempdir().into_diagnostic().wrap_err("create temporary fetch workspace")?;
        let mut temp_config = config.clone();
        temp_config.modules_dir = staging_dir.path().join("node_modules");
        temp_config.virtual_store_dir = temp_config.modules_dir.join(".pnpm");
        temp_config.lockfile = false;
        let temp_config = temp_config.leak();

        let http_client = ThrottledClient::new_with_options(
            config.network_concurrency as usize,
            ThrottledClientOptions {
                request_timeout_ms: Some(config.fetch_timeout),
                strict_ssl: config.strict_ssl,
                ca_certs: config.ca.clone(),
                registry_tls_configs: network_tls_configs(config),
                https_proxy: config.https_proxy.clone(),
                http_proxy: config.http_proxy.clone(),
                no_proxy: config.no_proxy.clone(),
            },
        );
        let resolved_packages = ResolvedPackages::new();
        let dependency_groups = self.dependency_options.dependency_groups().collect::<Vec<_>>();
        let direct_dependencies =
            project_snapshot.dependencies_by_groups(dependency_groups.iter().copied()).count();
        if reporter != pacquet_package_manager::InstallReporter::Silent {
            println!("Importing packages to virtual store");
            println!("Already up to date");
        }
        start_progress_reporter(direct_dependencies, true, reporter, None);

        InstallFrozenLockfile {
            http_client: &http_client,
            resolved_packages: &resolved_packages,
            config: temp_config,
            project_snapshot,
            packages: lockfile.packages.as_ref(),
            lockfile_dir: &lockfile_dir,
            dependency_groups: dependency_groups.iter().copied(),
            offline: false,
            force: false,
            pnpmfile: pnpmfile.as_deref(),
            ignore_pnpmfile,
        }
        .run()
        .await;
        let _ = finish_progress_reporter(true);

        Ok(())
    }
}

fn select_fetch_project_snapshot<'a>(
    project_snapshot: &'a RootProjectSnapshot,
    lockfile_dir: &Path,
) -> miette::Result<&'a ProjectSnapshot> {
    match project_snapshot {
        RootProjectSnapshot::Single(snapshot) => Ok(snapshot),
        RootProjectSnapshot::Multi(snapshot) => snapshot.importers.get(".").ok_or_else(|| {
            miette::miette!(
                "No workspace root importer `.` found in lockfile at {}",
                lockfile_dir.display()
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn fetch_dependency_options_match_pnpm_fetch_semantics() {
        let groups = |opts: FetchDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        assert_eq!(
            groups(FetchDependencyOptions { prod: false, dev: false }),
            vec![DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional]
        );
        assert_eq!(
            groups(FetchDependencyOptions { prod: true, dev: false }),
            vec![DependencyGroup::Prod, DependencyGroup::Optional]
        );
        assert_eq!(
            groups(FetchDependencyOptions { prod: false, dev: true }),
            vec![DependencyGroup::Dev]
        );
        assert_eq!(
            groups(FetchDependencyOptions { prod: true, dev: true }),
            vec![DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional]
        );
    }
}
