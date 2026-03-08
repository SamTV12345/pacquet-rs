use crate::{ParsePkgNameSuffixError, ParsePkgVerPeerError, PkgNameSuffix, PkgVerPeer};
use sha2::{Digest, Sha256};

/// Syntax: `{name}@{version}({peers})`
///
/// Example: `react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)`
///
/// **NOTE:** The suffix isn't guaranteed to be correct. It is only assumed to be.
pub type PkgNameVerPeer = PkgNameSuffix<PkgVerPeer>;

/// Error when parsing [`PkgNameVerPeer`] from a string.
pub type ParsePkgNameVerPeerError = ParsePkgNameSuffixError<ParsePkgVerPeerError>;

impl PkgNameVerPeer {
    /// Construct the name of the corresponding subdirectory in the virtual store directory.
    pub fn to_virtual_store_name(&self) -> String {
        const MAX_LENGTH_WITHOUT_HASH: usize = 120;

        let mut filename = dep_path_to_filename_unescaped(&self.to_string());
        filename = filename
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
            filename = collapse_peer_suffix_parens(&filename);
        }

        let requires_hash = filename.len() > MAX_LENGTH_WITHOUT_HASH
            || (filename != filename.to_lowercase() && !filename.starts_with("file+"));
        if !requires_hash {
            return filename;
        }

        let hash = create_short_hash_hex(&filename);
        let keep = MAX_LENGTH_WITHOUT_HASH.saturating_sub(33);
        let prefix = truncate_chars(&filename, keep);
        format!("{prefix}_{hash}")
    }
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

fn collapse_peer_suffix_parens(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0_usize;
    while index < chars.len() {
        if chars[index] == ')' && chars.get(index + 1) == Some(&'(') {
            output.push('_');
            index += 2;
            continue;
        }
        match chars[index] {
            '(' | ')' => output.push('_'),
            other => output.push(other),
        }
        index += 1;
    }
    output
}

fn create_short_hash_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let hex = format!("{digest:x}");
    hex[..32].to_string()
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    input.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn name_peer_ver(name: &str, peer_ver: &str) -> PkgNameVerPeer {
        let peer_ver = peer_ver.to_string().parse().unwrap();
        PkgNameVerPeer::new(name.parse().unwrap(), peer_ver)
    }

    #[test]
    fn parse() {
        fn case(input: &'static str, expected: PkgNameVerPeer) {
            eprintln!("CASE: {input:?}");
            let received: PkgNameVerPeer = input.parse().unwrap();
            assert_eq!(&received, &expected);
        }

        case(
            "react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
            name_peer_ver(
                "react-json-view",
                "1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
            ),
        );
        case("react-json-view@1.21.3", name_peer_ver("react-json-view", "1.21.3"));
        case(
            "@algolia/autocomplete-core@1.9.3(@algolia/client-search@4.18.0)(algoliasearch@4.18.0)(search-insights@2.6.0)",
            name_peer_ver(
                "@algolia/autocomplete-core",
                "1.9.3(@algolia/client-search@4.18.0)(algoliasearch@4.18.0)(search-insights@2.6.0)",
            ),
        );
        case(
            "@algolia/autocomplete-core@1.9.3",
            name_peer_ver("@algolia/autocomplete-core", "1.9.3"),
        );
    }

    #[test]
    fn to_virtual_store_name() {
        fn case(input: &'static str, expected: &'static str) {
            eprintln!("CASE: {input:?}");
            let name_ver_peer: PkgNameVerPeer = input.parse().unwrap();
            dbg!(&name_ver_peer);
            let received = name_ver_peer.to_virtual_store_name();
            assert_eq!(received, expected);
        }

        case("ts-node@10.9.1", "ts-node@10.9.1");
        case(
            "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            "ts-node@10.9.1_@types+node@18.7.19_typescript@5.1.6",
        );
        case(
            "@babel/plugin-proposal-object-rest-spread@7.12.1",
            "@babel+plugin-proposal-object-rest-spread@7.12.1",
        );
        case(
            "@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
            "@babel+plugin-proposal-object-rest-spread@7.12.1_@babel+core@7.12.9",
        );
    }
}
