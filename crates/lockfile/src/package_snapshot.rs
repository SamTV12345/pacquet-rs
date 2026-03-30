use crate::{LockfileResolution, PackageSnapshotDependency, PkgName};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Deserialize a field that can be either a single string or an array of strings.
/// pnpm lockfiles sometimes write `libc: glibc` instead of `libc: [glibc]`.
pub(crate) fn string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct StringOrVec;

    impl<'de> de::Visitor<'de> for StringOrVec {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or a sequence of strings")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Some(vec![value.to_string()]))
        }

        fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Some(vec![value]))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut vec = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                vec.push(item);
            }
            Ok(Some(vec))
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockfilePeerDependencyMetaValue {
    optional: bool,
}

// Reference: https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-file/src/sortLockfileKeys.ts#L5
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSnapshot {
    pub resolution: LockfileResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>, // TODO: name and version are required on non-default registry, create a struct for it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>, // TODO: name and version are required on non-default registry, create a struct for it

    #[serde(skip_serializing_if = "Option::is_none")]
    pub engines: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "string_or_vec")]
    pub cpu: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "string_or_vec")]
    pub os: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "string_or_vec")]
    pub libc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_bin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_build: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundled_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMetaValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<PkgName, PackageSnapshotDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<HashMap<String, String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub transitive_peer_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}
