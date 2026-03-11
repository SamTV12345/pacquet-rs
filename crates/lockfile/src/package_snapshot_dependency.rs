use crate::{DependencyPath, PkgNameVerPeer, PkgVerPeer};
use derive_more::{Display, From, TryInto};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Value of [`PackageSnapshot::dependencies`](crate::PackageSnapshot::dependencies).
#[derive(Debug, Display, Clone, PartialEq, Eq, From, TryInto)]
pub enum PackageSnapshotDependency {
    PkgVerPeer(PkgVerPeer),
    DependencyPath(DependencyPath),
    PkgNameVerPeer(PkgNameVerPeer),
    Link(String),
}

impl Serialize for PackageSnapshotDependency {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PackageSnapshotDependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;

        if value.starts_with("file:") || value.starts_with("link:") {
            return Ok(Self::Link(value));
        }

        if let Ok(pkg_ver_peer) = value.parse::<PkgVerPeer>() {
            return Ok(Self::PkgVerPeer(pkg_ver_peer));
        }

        if let Ok(dependency_path) = value.parse::<DependencyPath>() {
            return Ok(Self::DependencyPath(dependency_path));
        }

        if let Ok(pkg_name_ver_peer) = value.parse::<PkgNameVerPeer>() {
            return Ok(Self::PkgNameVerPeer(pkg_name_ver_peer));
        }

        Err(de::Error::custom(format!("invalid package snapshot dependency: {value}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;

    #[test]
    fn deserialize_to_correct_variants() {
        macro_rules! case {
            ($input:expr => $output:ident) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let snapshot_dependency: PackageSnapshotDependency =
                    serde_yaml::from_str(input).unwrap();
                dbg!(&snapshot_dependency);
                assert!(matches!(&snapshot_dependency, PackageSnapshotDependency::$output(_)));
            }};
        }

        case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => PkgVerPeer);
        case!("0.27.1" => PkgVerPeer);
        case!("1.21.3(react@17.0.2)" => PkgVerPeer);
        case!("1.21.3-rc.0(react@17.0.2)" => PkgVerPeer);
        case!("1.21.3" => PkgVerPeer);
        case!("1.21.3-rc.0" => PkgVerPeer);
        case!("string-width@4.2.3" => PkgNameVerPeer);
        case!("'@scope/pkg@1.2.3'" => PkgNameVerPeer);
        case!("debug@4.4.3(supports-color@8.1.1)" => PkgNameVerPeer);
        case!("file:../local-pkg" => Link);
        case!("link:../local-pkg" => Link);
        case!("file:packages/project-1(peer-provider@file:packages/peer-provider)" => Link);
        case!("link:../project-1(peer-provider@file:../peer-provider)" => Link);
        case!("/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3-rc.0(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3" => DependencyPath);
        case!("/react-json-view@1.21.3-rc.0" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3-rc.0(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3-rc.0" => DependencyPath);
        case!("/@docusaurus/react-loadable@5.5.2(react@17.0.2)" => DependencyPath);
        case!("/@docusaurus/react-loadable@5.5.2" => DependencyPath);
        case!("registry.npmjs.com/@docusaurus/react-loadable@5.5.2(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/@docusaurus/react-loadable@5.5.2" => DependencyPath);
    }

    #[test]
    fn string_matches_yaml() {
        fn case(input: &'static str) {
            eprintln!("CASE: {input:?}");
            let snapshot_dependency: PackageSnapshotDependency =
                serde_yaml::from_str(input).unwrap();
            dbg!(&snapshot_dependency);
            let received = snapshot_dependency.to_string().pipe(serde_yaml::Value::String);
            let expected: serde_yaml::Value = serde_yaml::from_str(input).unwrap();
            assert_eq!(&received, &expected);
        }

        case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("0.27.1");
        case("1.21.3(react@17.0.2)");
        case("1.21.3-rc.0(react@17.0.2)");
        case("1.21.3");
        case("1.21.3-rc.0");
        case("string-width@4.2.3");
        case("'@scope/pkg@1.2.3'");
        case("debug@4.4.3(supports-color@8.1.1)");
        case("file:../local-pkg");
        case("link:../local-pkg");
        case("file:packages/project-1(peer-provider@file:packages/peer-provider)");
        case("link:../project-1(peer-provider@file:../peer-provider)");
        case("/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("/react-json-view@1.21.3(react@17.0.2)");
        case("/react-json-view@1.21.3-rc.0(react@17.0.2)");
        case("/react-json-view@1.21.3");
        case("/react-json-view@1.21.3-rc.0");
        case(
            "registry.npmjs.com/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
        );
        case("registry.npmjs.com/react-json-view@1.21.3(react@17.0.2)");
        case("registry.npmjs.com/react-json-view@1.21.3-rc.0(react@17.0.2)");
        case("registry.npmjs.com/react-json-view@1.21.3");
        case("registry.npmjs.com/react-json-view@1.21.3-rc.0");
        case("/@docusaurus/react-loadable@5.5.2(react@17.0.2)");
        case("/@docusaurus/react-loadable@5.5.2");
        case("registry.npmjs.com/@docusaurus/react-loadable@5.5.2(react@17.0.2)");
        case("registry.npmjs.com/@docusaurus/react-loadable@5.5.2");
    }

    #[test]
    fn serialize_link_without_yaml_tag() {
        let value = PackageSnapshotDependency::Link("link:packages/project-1".to_string());
        let serialized = serde_yaml::to_string(&value).expect("serialize yaml");
        assert_eq!(serialized.trim(), "link:packages/project-1");
    }
}
