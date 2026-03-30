use crate::{ParsePkgNameError, ParsePkgNameVerPeerError, PkgName, PkgNameVerPeer};
use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// Dependency path is the key of the `packages` map.
///
/// Specification: <https://github.com/pnpm/spec/blob/master/lockfile/6.0.md#packages>
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "&'de str", into = "String")]
pub struct DependencyPath {
    pub custom_registry: Option<String>,
    pub package_specifier: DependencyPathSpecifier,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DependencyPathSpecifier {
    Registry(PkgNameVerPeer),
    LocalFile(LocalFilePackageSpecifier),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalFilePackageSpecifier {
    pub name: PkgName,
    pub reference: String,
}

/// Error when parsing [`DependencyPath`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseDependencyPathError {
    #[display("Invalid syntax")]
    InvalidSyntax,
    #[display("Failed to parse specifier: {_0}")]
    ParsePackageSpecifierFailure(ParseDependencyPathSpecifierError),
}

#[derive(Debug, Display, Error)]
pub enum ParseDependencyPathSpecifierError {
    #[display("Failed to parse registry package specifier: {_0}")]
    ParseRegistrySpecifierFailure(ParsePkgNameVerPeerError),
    #[display("Failed to parse local file package name: {_0}")]
    ParseLocalFileNameFailure(ParsePkgNameError),
    #[display("Local file dependency path must start with file:")]
    InvalidLocalFileReference,
}

impl DependencyPath {
    pub fn registry(custom_registry: Option<String>, package_specifier: PkgNameVerPeer) -> Self {
        Self {
            custom_registry,
            package_specifier: DependencyPathSpecifier::Registry(package_specifier),
        }
    }

    pub fn local_file(name: PkgName, reference: String) -> Self {
        Self {
            custom_registry: None,
            package_specifier: DependencyPathSpecifier::LocalFile(LocalFilePackageSpecifier {
                name,
                reference,
            }),
        }
    }

    pub fn package_name(&self) -> &PkgName {
        self.package_specifier.name()
    }

    pub fn to_virtual_store_name(&self) -> String {
        self.package_specifier.to_virtual_store_name()
    }

    pub fn registry_specifier(&self) -> Option<&PkgNameVerPeer> {
        self.package_specifier.registry_specifier()
    }

    pub fn local_file_reference(&self) -> Option<&str> {
        self.package_specifier.local_file_reference()
    }
}

impl DependencyPathSpecifier {
    pub fn name(&self) -> &PkgName {
        match self {
            DependencyPathSpecifier::Registry(specifier) => &specifier.name,
            DependencyPathSpecifier::LocalFile(specifier) => &specifier.name,
        }
    }

    pub fn to_virtual_store_name(&self) -> String {
        match self {
            DependencyPathSpecifier::Registry(specifier) => specifier.to_virtual_store_name(),
            DependencyPathSpecifier::LocalFile(specifier) => {
                dep_path_to_filename(&format!("{}@{}", specifier.name, specifier.reference))
            }
        }
    }

    pub fn registry_specifier(&self) -> Option<&PkgNameVerPeer> {
        match self {
            DependencyPathSpecifier::Registry(specifier) => Some(specifier),
            DependencyPathSpecifier::LocalFile(_) => None,
        }
    }

    pub fn local_file_reference(&self) -> Option<&str> {
        match self {
            DependencyPathSpecifier::Registry(_) => None,
            DependencyPathSpecifier::LocalFile(specifier) => Some(specifier.reference.as_str()),
        }
    }
}

impl fmt::Display for DependencyPathSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyPathSpecifier::Registry(specifier) => write!(f, "{specifier}"),
            DependencyPathSpecifier::LocalFile(specifier) => {
                write!(f, "{}@{}", specifier.name, specifier.reference)
            }
        }
    }
}

impl FromStr for DependencyPathSpecifier {
    type Err = ParseDependencyPathSpecifierError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(specifier) = value.parse::<PkgNameVerPeer>() {
            return Ok(Self::Registry(specifier));
        }

        let (name, reference_without_prefix) = value.split_once("@file:").ok_or_else(|| {
            ParseDependencyPathSpecifierError::ParseRegistrySpecifierFailure(
                value.parse::<PkgNameVerPeer>().unwrap_err(),
            )
        })?;
        let name = name
            .parse::<PkgName>()
            .map_err(ParseDependencyPathSpecifierError::ParseLocalFileNameFailure)?;
        let reference = format!("file:{reference_without_prefix}");
        if !reference.starts_with("file:") {
            return Err(ParseDependencyPathSpecifierError::InvalidLocalFileReference);
        }
        Ok(Self::LocalFile(LocalFilePackageSpecifier { name, reference }))
    }
}

impl fmt::Display for DependencyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.custom_registry.as_deref() {
            Some(custom_registry) => write!(f, "{custom_registry}/{}", self.package_specifier),
            None => write!(f, "/{}", self.package_specifier),
        }
    }
}

impl FromStr for DependencyPath {
    type Err = ParseDependencyPathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains("@file:")
            && !s.starts_with('/')
            && let Ok(package_specifier) = s.parse::<DependencyPathSpecifier>()
        {
            return Ok(DependencyPath { custom_registry: None, package_specifier });
        }
        let (custom_registry, package_specifier) =
            s.split_once('/').ok_or(ParseDependencyPathError::InvalidSyntax)?;
        if custom_registry.starts_with('@') {
            return Err(ParseDependencyPathError::InvalidSyntax);
        }
        let custom_registry =
            if custom_registry.is_empty() { None } else { Some(custom_registry.to_string()) };
        let package_specifier = match package_specifier.parse::<DependencyPathSpecifier>() {
            Ok(value) => value,
            Err(_) => {
                let (base, peers) = package_specifier
                    .find('(')
                    .map_or((package_specifier, ""), |index| package_specifier.split_at(index));
                let (name, suffix) =
                    base.rsplit_once('/').ok_or(ParseDependencyPathError::InvalidSyntax)?;
                let normalized = format!("{name}@{suffix}{peers}");
                normalized
                    .parse::<DependencyPathSpecifier>()
                    .map_err(ParseDependencyPathError::ParsePackageSpecifierFailure)?
            }
        };
        Ok(DependencyPath { custom_registry, package_specifier })
    }
}

impl<'a> TryFrom<&'a str> for DependencyPath {
    type Error = ParseDependencyPathError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<DependencyPath> for String {
    fn from(value: DependencyPath) -> Self {
        value.to_string()
    }
}

fn dep_path_to_filename(dep_path: &str) -> String {
    #[cfg(windows)]
    const MAX_LENGTH_WITHOUT_HASH: usize = 60;
    #[cfg(not(windows))]
    const MAX_LENGTH_WITHOUT_HASH: usize = 120;

    let mut filename = dep_path_to_filename_unescaped(dep_path)
        .chars()
        .map(|ch| match ch {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' => '+',
            other => other,
        })
        .collect::<String>();

    if filename.contains('(') {
        if filename.ends_with(')') {
            filename.pop();
        }
        filename = filename.replace(")(", "_").replace(['(', ')'], "_");
    }

    let requires_hash = filename.len() > MAX_LENGTH_WITHOUT_HASH
        || (filename != filename.to_lowercase() && !filename.starts_with("file+"));
    if !requires_hash {
        return filename;
    }

    let hash = create_short_hash_hex(&filename);
    let keep = MAX_LENGTH_WITHOUT_HASH.saturating_sub(33);
    let prefix = filename.chars().take(keep).collect::<String>();
    format!("{prefix}_{hash}")
}

fn dep_path_to_filename_unescaped(dep_path: &str) -> String {
    if dep_path.starts_with("file:") {
        return dep_path.replacen(':', "+", 1);
    }

    let dep_path = dep_path.strip_prefix('/').unwrap_or(dep_path);
    let at_index = dep_path[1..].find('@').map(|idx| idx + 1);
    match at_index {
        Some(index) => format!("{}@{}", &dep_path[..index], &dep_path[index + 1..]),
        None => dep_path.to_string(),
    }
}

fn create_short_hash_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let hex = format!("{digest:x}");
    hex[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn serialize() {
        fn case(input: DependencyPath, output: &'static str) {
            eprintln!("CASE: {input:?}");
            let yaml = serde_yaml::to_string(&input).unwrap();
            assert_eq!(yaml.trim(), output);
        }

        case(DependencyPath::registry(None, "ts-node@10.9.1".parse().unwrap()), "/ts-node@10.9.1");
        case(
            DependencyPath::registry(
                Some("registry.node-modules.io".to_string()),
                "ts-node@10.9.1".parse().unwrap(),
            ),
            "registry.node-modules.io/ts-node@10.9.1",
        );
        case(
            DependencyPath::local_file(
                "local-pkg".parse().unwrap(),
                "file:../local-pkg".to_string(),
            ),
            "/local-pkg@file:../local-pkg",
        );
    }

    #[test]
    fn deserialize() {
        fn case(input: &'static str, expected: DependencyPath) {
            eprintln!("CASE: {input:?}");
            let dependency_path: DependencyPath = serde_yaml::from_str(input).unwrap();
            assert_eq!(dependency_path, expected);
        }

        case("/ts-node@10.9.1", DependencyPath::registry(None, "ts-node@10.9.1".parse().unwrap()));
        case(
            "registry.node-modules.io/ts-node/10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            DependencyPath::registry(
                Some("registry.node-modules.io".to_string()),
                "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)".parse().unwrap(),
            ),
        );
        case(
            "local-pkg@file:../local-pkg",
            DependencyPath::local_file(
                "local-pkg".parse().unwrap(),
                "file:../local-pkg".to_string(),
            ),
        );
        case(
            "/@scope/pkg@file:../local-pkg(peer-a@1.0.0)",
            DependencyPath::local_file(
                "@scope/pkg".parse().unwrap(),
                "file:../local-pkg(peer-a@1.0.0)".to_string(),
            ),
        );
    }

    #[test]
    fn local_file_virtual_store_name_matches_pnpm_filename_rules() {
        let dependency_path = DependencyPath::local_file(
            "local-pkg".parse().unwrap(),
            "file:../local-pkg".to_string(),
        );
        assert_eq!(dependency_path.to_virtual_store_name(), "local-pkg@file+..+local-pkg");
    }

    #[test]
    fn parse_error() {
        let error = "ts-node".parse::<DependencyPath>().unwrap_err();
        assert_eq!(error.to_string(), "Invalid syntax");
        assert!(matches!(error, ParseDependencyPathError::InvalidSyntax));
    }
}
