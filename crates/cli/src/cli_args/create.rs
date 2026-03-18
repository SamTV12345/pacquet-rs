use crate::cli_args::dlx::DlxArgs;
use clap::Args;
use pacquet_npmrc::Npmrc;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// A list of package names that are allowed to run postinstall scripts during installation.
    #[arg(long = "allow-build")]
    _allow_build: Vec<String>,

    /// The starter kit name or create-* package.
    package_name: String,

    /// Arguments forwarded to the starter.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    package_args: Vec<String>,
}

impl CreateArgs {
    pub async fn run(self, dir: PathBuf, npmrc: &'static Npmrc) -> miette::Result<()> {
        let CreateArgs { _allow_build, package_name, package_args } = self;
        let package_name = convert_to_create_name(&package_name);
        DlxArgs {
            package: vec![],
            shell_mode: false,
            reporter: None,
            command: package_name,
            args: package_args,
        }
        .run(dir, npmrc)
        .await
    }
}

const CREATE_PREFIX: &str = "create-";

fn convert_to_create_name(package_name: &str) -> String {
    if let Some(stripped) = package_name.strip_prefix('@') {
        let preferred_version_position = stripped.find('@').map(|index| index + 1);
        let (scoped_name, preferred_version) = if let Some(position) = preferred_version_position {
            (&package_name[..position], &package_name[position..])
        } else {
            (package_name, "")
        };
        let mut parts = scoped_name.splitn(2, '/');
        let scope = parts.next().expect("scoped package should contain scope");
        let scoped_package = parts.next().unwrap_or_default();
        if scoped_package.is_empty() {
            return format!("{scope}/create{preferred_version}");
        }
        return format!("{scope}/{}{}", ensure_create_prefixed(scoped_package), preferred_version);
    }
    ensure_create_prefixed(package_name)
}

fn ensure_create_prefixed(package_name: &str) -> String {
    if package_name.starts_with(CREATE_PREFIX) {
        package_name.to_string()
    } else {
        format!("{CREATE_PREFIX}{package_name}")
    }
}

#[cfg(test)]
mod tests {
    use super::convert_to_create_name;
    use pretty_assertions::assert_eq;

    #[test]
    fn convert_plain_package_name_to_create_name() {
        assert_eq!(convert_to_create_name("vite"), "create-vite");
        assert_eq!(convert_to_create_name("create-vite"), "create-vite");
    }

    #[test]
    fn convert_scoped_package_name_to_create_name() {
        assert_eq!(convert_to_create_name("@scope"), "@scope/create");
        assert_eq!(convert_to_create_name("@scope/vite"), "@scope/create-vite");
        assert_eq!(convert_to_create_name("@scope/vite@latest"), "@scope/create-vite@latest");
    }
}
