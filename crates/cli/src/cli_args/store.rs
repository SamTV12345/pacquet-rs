use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_network::{RegistryTlsConfig, ThrottledClient, ThrottledClientOptions};
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::InstallPackageFromRegistry;
use pacquet_store_dir::PackageFilesIndex;
use pacquet_tarball::MemCache;
use std::fs::File;
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

#[derive(Debug, Subcommand)]
pub enum StoreCommand {
    /// Checks for modified packages in the store.
    #[clap(alias = "store")]
    Status,
    /// Functionally equivalent to pnpm add, except this adds new packages to the store directly
    /// without modifying any projects or files outside of the store.
    Add(StoreAddArgs),
    /// Removes unreferenced packages from the store.
    /// Unreferenced packages are packages that are not used by any projects on the system.
    /// Packages can become unreferenced after most installation operations, for instance when
    /// dependencies are made redundant.
    Prune,
    /// Returns the path to the active store directory.
    Path,
}

#[derive(Debug, Args)]
pub struct StoreAddArgs {
    /// Packages to add to the store (for example: express@4 typescript@5).
    packages: Vec<String>,
}

impl StoreCommand {
    /// Execute the subcommand.
    pub async fn run<'a>(self, config: impl FnOnce() -> &'a Npmrc) -> miette::Result<()> {
        match self {
            StoreCommand::Status => {
                let config = config();
                let mut modified = Vec::new();

                for path in config.store_dir.index_file_paths() {
                    let Ok(file) = File::open(&path) else {
                        continue;
                    };
                    let Ok(index) = serde_json::from_reader::<_, PackageFilesIndex>(file) else {
                        continue;
                    };

                    let has_missing_file = index.files.values().any(|file_info| {
                        let executable = (file_info.mode & 0o111) != 0;
                        config
                            .store_dir
                            .cas_file_path_by_integrity(&file_info.integrity, executable)
                            .is_none_or(|cas_path| !cas_path.is_file())
                    });
                    if has_missing_file {
                        modified.push(path);
                    }
                }

                if !modified.is_empty() {
                    eprintln!("Modified packages in the store:");
                    for path in modified {
                        eprintln!("  {}", path.display());
                    }
                    miette::bail!("store has modified packages");
                }
            }
            StoreCommand::Add(args) => {
                if args.packages.is_empty() {
                    miette::bail!("Please specify at least one package");
                }
                let config = config();
                let staging_dir = tempdir()
                    .into_diagnostic()
                    .wrap_err("create temporary workspace for store add")?;
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
                let tarball_mem_cache = MemCache::new();

                for package in &args.packages {
                    let (name, version_range) = parse_store_add_package_spec(package);
                    InstallPackageFromRegistry {
                        tarball_mem_cache: &tarball_mem_cache,
                        http_client: &http_client,
                        config: temp_config,
                        node_modules_dir: &temp_config.modules_dir,
                        name,
                        version_range,
                        prefer_offline: false,
                        offline: false,
                        force: false,
                    }
                    .run()
                    .await
                    .wrap_err_with(|| format!("store add failed for package spec `{package}`"))?;
                }
            }
            StoreCommand::Prune => {
                let config = config();
                let project_dir = std::env::current_dir()
                    .into_diagnostic()
                    .wrap_err("resolve current directory for store prune")?;
                config
                    .store_dir
                    .register_project(&project_dir)
                    .wrap_err("register current project for store prune")?;
                config.store_dir.prune().wrap_err("pruning store")?;
            }
            StoreCommand::Path => {
                println!("{}", config().store_dir.display());
            }
        }

        Ok(())
    }
}

fn parse_store_add_package_spec(package: &str) -> (&str, &str) {
    if let Some(marker) = package.find("@npm:")
        && marker > 0
    {
        let name = &package[..marker];
        let version_range = &package[(marker + 1)..];
        return (name, version_range);
    }

    let separator = if let Some(stripped) = package.strip_prefix('@') {
        stripped.rfind('@').map(|index| index + 1)
    } else {
        package.rfind('@')
    };
    match separator {
        Some(index) => {
            let (name, spec) = package.split_at(index);
            let spec = &spec[1..];
            if spec.is_empty() { (package, "latest") } else { (name, spec) }
        }
        None => (package, "latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_store_add_package_spec;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_store_add_package_spec_defaults_to_latest() {
        assert_eq!(parse_store_add_package_spec("fastify"), ("fastify", "latest"));
        assert_eq!(parse_store_add_package_spec("@scope/pkg"), ("@scope/pkg", "latest"));
    }

    #[test]
    fn parse_store_add_package_spec_extracts_explicit_spec() {
        assert_eq!(parse_store_add_package_spec("fastify@4.0.0"), ("fastify", "4.0.0"));
        assert_eq!(parse_store_add_package_spec("@scope/pkg@^1.2.0"), ("@scope/pkg", "^1.2.0"));
    }

    #[test]
    fn parse_store_add_package_spec_keeps_npm_alias_target() {
        assert_eq!(
            parse_store_add_package_spec("hello-alias@npm:is-number@7.0.0"),
            ("hello-alias", "npm:is-number@7.0.0")
        );
    }
}
