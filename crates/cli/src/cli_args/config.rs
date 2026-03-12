use clap::{Args, Subcommand};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Set the config key to the value provided.
    Set(ConfigSetCommandArgs),
    /// Print the config value for the provided key.
    Get(ConfigGetCommandArgs),
    /// Remove the config key from the config file.
    Delete(ConfigDeleteCommandArgs),
    /// Show all the config settings.
    List(ConfigListCommandArgs),
}

#[derive(Debug, Clone, Copy)]
enum ConfigLocation {
    Global,
    Project,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: Option<ConfigCommand>,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// Config key to print. When omitted, lists all config settings.
    key: Option<String>,

    /// Show config settings in JSON format when listing all settings.
    #[arg(long)]
    json: bool,

    /// Set the configuration in the global config file.
    #[arg(short = 'g', long)]
    global: bool,

    /// Select whether project or global config should be targeted.
    #[arg(long, value_parser = ["project", "global"])]
    location: Option<String>,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Config key to set, or a `key=value` pair.
    key: String,

    /// Config value. Optional when `key=value` syntax is used.
    value: Option<String>,

    /// Set the configuration in the global config file.
    #[arg(short = 'g', long)]
    global: bool,

    /// Select whether project or global config should be targeted.
    #[arg(long, value_parser = ["project", "global"])]
    location: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ConfigTargetArgs {
    /// Set the configuration in the global config file.
    #[arg(short = 'g', long)]
    global: bool,

    /// Select whether project or global config should be targeted.
    #[arg(long, value_parser = ["project", "global"])]
    location: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConfigGetCommandArgs {
    #[clap(flatten)]
    target: ConfigTargetArgs,

    /// Config key to print. When omitted, lists all config settings.
    key: Option<String>,

    /// Show config settings in JSON format when listing all settings.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct ConfigSetCommandArgs {
    #[clap(flatten)]
    target: ConfigTargetArgs,

    /// Config key to set, or a `key=value` pair.
    key: String,

    /// Config value. Optional when `key=value` syntax is used.
    value: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConfigDeleteCommandArgs {
    #[clap(flatten)]
    target: ConfigTargetArgs,

    /// Config key to remove.
    key: String,
}

#[derive(Debug, Args)]
pub struct ConfigListCommandArgs {
    #[clap(flatten)]
    target: ConfigTargetArgs,

    /// Show config settings in JSON format.
    #[arg(long)]
    json: bool,
}

impl ConfigArgs {
    pub fn run(self, dir: &Path, npmrc: &Npmrc) -> miette::Result<()> {
        let command =
            self.command.ok_or_else(|| miette::miette!("Please specify the subcommand"))?;

        match command {
            ConfigCommand::List(args) => {
                let _location =
                    config_location(args.target.global, args.target.location.as_deref())?;
                print_list(npmrc, args.json)?;
            }
            ConfigCommand::Get(args) => {
                let _location =
                    config_location(args.target.global, args.target.location.as_deref())?;
                if let Some(key) = args.key {
                    print_get(npmrc, &key)?;
                } else {
                    print_list(npmrc, args.json)?;
                }
            }
            ConfigCommand::Set(args) => {
                let location =
                    config_location(args.target.global, args.target.location.as_deref())?;
                let key_and_value = std::iter::once(args.key).chain(args.value).collect::<Vec<_>>();
                let (key, value) = parse_set_args(&key_and_value)?;
                set_config_value(target_config_path(dir, location)?, &key, Some(&value))?;
            }
            ConfigCommand::Delete(args) => {
                let location =
                    config_location(args.target.global, args.target.location.as_deref())?;
                set_config_value(target_config_path(dir, location)?, &args.key, None)?;
            }
        }

        Ok(())
    }
}

impl GetArgs {
    pub fn run(self, dir: &Path, npmrc: &Npmrc) -> miette::Result<()> {
        ConfigArgs {
            command: Some(ConfigCommand::Get(ConfigGetCommandArgs {
                target: ConfigTargetArgs { global: self.global, location: self.location },
                key: self.key,
                json: self.json,
            })),
        }
        .run(dir, npmrc)
    }
}

impl SetArgs {
    pub fn run(self, dir: &Path, npmrc: &Npmrc) -> miette::Result<()> {
        ConfigArgs {
            command: Some(ConfigCommand::Set(ConfigSetCommandArgs {
                target: ConfigTargetArgs { global: self.global, location: self.location },
                key: self.key,
                value: self.value,
            })),
        }
        .run(dir, npmrc)
    }
}

fn config_location(global: bool, location: Option<&str>) -> miette::Result<ConfigLocation> {
    match location {
        Some("global") => Ok(ConfigLocation::Global),
        Some("project") => Ok(ConfigLocation::Project),
        Some(_) => miette::bail!("location must be one of: project, global"),
        None if global => Ok(ConfigLocation::Global),
        None => Ok(ConfigLocation::Global),
    }
}

fn target_config_path(dir: &Path, location: ConfigLocation) -> miette::Result<PathBuf> {
    match location {
        ConfigLocation::Project => Ok(dir.join(".npmrc")),
        ConfigLocation::Global => {
            let home = env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .or_else(home::home_dir)
                .ok_or_else(|| miette::miette!("could not detect home directory"))?;
            Ok(home.join(".npmrc"))
        }
    }
}

fn print_list(npmrc: &Npmrc, json: bool) -> miette::Result<()> {
    let settings = normalized_raw_settings(npmrc);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&settings)
                .into_diagnostic()
                .wrap_err("serialize config")?
        );
        return Ok(());
    }

    let mut output = String::new();
    for (key, value) in settings {
        output.push_str(&format!("{key}={value}\n"));
    }
    print!("{output}");
    Ok(())
}

fn print_get(npmrc: &Npmrc, key: &str) -> miette::Result<()> {
    let normalized_key = normalize_config_key(key);
    let value = npmrc.raw_settings().get(&normalized_key).map(String::as_str).unwrap_or_default();
    println!("{value}");
    Ok(())
}

fn normalized_raw_settings(npmrc: &Npmrc) -> BTreeMap<String, String> {
    npmrc.raw_settings().iter().map(|(key, value)| (key.clone(), value.clone())).collect()
}

fn parse_set_args(args: &[String]) -> miette::Result<(String, String)> {
    match args {
        [key, value] => Ok((key.clone(), value.clone())),
        [single] => {
            let Some((key, value)) = single.split_once('=') else {
                miette::bail!("`pacquet config set` requires a key and value");
            };
            Ok((key.to_string(), value.to_string()))
        }
        _ => miette::bail!("`pacquet config set` requires a key and value"),
    }
}

fn set_config_value(config_path: PathBuf, key: &str, value: Option<&str>) -> miette::Result<()> {
    let mut settings = read_ini_like(&config_path)?;
    let normalized_key = normalize_config_key(key);
    match value {
        Some(value) => {
            settings.insert(normalized_key, value.to_string());
        }
        None => {
            settings.remove(&normalized_key);
        }
    }

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("create {}", parent.display()))?;
    }

    if settings.is_empty() {
        if config_path.exists() {
            fs::remove_file(&config_path)
                .into_diagnostic()
                .wrap_err_with(|| format!("remove {}", config_path.display()))?;
        }
        return Ok(());
    }

    let mut content = String::new();
    for (key, value) in settings {
        content.push_str(&format!("{key}={value}\n"));
    }
    fs::write(&config_path, content)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", config_path.display()))
}

fn read_ini_like(path: &Path) -> miette::Result<BTreeMap<String, String>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error)
                .into_diagnostic()
                .wrap_err_with(|| format!("read {}", path.display()));
        }
    };
    let mut settings = BTreeMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        settings.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(settings)
}

fn normalize_config_key(key: &str) -> String {
    if key.starts_with('@') || key.starts_with("//") || key.contains(':') {
        return key.to_string();
    }
    let mut output = String::new();
    for (index, ch) in key.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                output.push('-');
            }
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::normalize_config_key;

    #[test]
    fn normalize_config_key_converts_camel_case_to_kebab_case() {
        assert_eq!(normalize_config_key("storeDir"), "store-dir");
        assert_eq!(normalize_config_key("fetchRetries"), "fetch-retries");
    }

    #[test]
    fn normalize_config_key_preserves_scoped_and_registry_keys() {
        assert_eq!(normalize_config_key("@foo:registry"), "@foo:registry");
        assert_eq!(normalize_config_key("//reg.example/:_authToken"), "//reg.example/:_authToken");
    }
}
