use pacquet_lockfile::PackageSnapshot;
use pacquet_registry::PackageVersion;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Installability {
    Install,
    SkipOptional,
}

pub(crate) fn check_package_version_installability(
    package: &PackageVersion,
    optional: bool,
) -> Installability {
    let package_id = format!("{}@{}", package.name, package.version);
    installability_for_values(
        package.engines.as_ref(),
        package.cpu.as_deref(),
        package.os.as_deref(),
        package.libc.as_deref(),
        &package_id,
        optional,
        true,
    )
}

pub(crate) fn should_skip_optional_package_version(package: &PackageVersion) -> bool {
    let package_id = format!("{}@{}", package.name, package.version);
    matches!(
        installability_for_values(
            package.engines.as_ref(),
            package.cpu.as_deref(),
            package.os.as_deref(),
            package.libc.as_deref(),
            &package_id,
            true,
            false,
        ),
        Installability::SkipOptional
    )
}

pub(crate) fn check_package_snapshot_installability(
    package_id: &str,
    package: &PackageSnapshot,
    optional: bool,
) -> Installability {
    installability_for_values(
        package.engines.as_ref(),
        package.cpu.as_deref(),
        package.os.as_deref(),
        package.libc.as_deref(),
        package_id,
        optional,
        true,
    )
}

fn installability_for_values(
    engines: Option<&HashMap<String, String>>,
    cpu: Option<&[String]>,
    os: Option<&[String]>,
    libc: Option<&[String]>,
    package_id: &str,
    optional: bool,
    emit: bool,
) -> Installability {
    if let Some(message) = platform_warning(package_id, cpu, os, libc) {
        return emit_result(package_id, optional, &message, emit);
    }
    if let Some(message) = engine_warning(package_id, engines) {
        return emit_result(package_id, optional, &message, emit);
    }
    Installability::Install
}

fn emit_result(package_id: &str, optional: bool, message: &str, emit: bool) -> Installability {
    if emit {
        crate::progress_reporter::warn(message);
        if optional {
            crate::progress_reporter::info(&format!(
                "{package_id} is an optional dependency and failed compatibility check. Excluding it from installation."
            ));
        }
    }
    if optional { Installability::SkipOptional } else { Installability::Install }
}

fn platform_warning(
    package_id: &str,
    cpu: Option<&[String]>,
    os: Option<&[String]>,
    libc: Option<&[String]>,
) -> Option<String> {
    let default_cpu = ["any".to_string()];
    let default_os = ["any".to_string()];
    let default_libc = ["any".to_string()];
    let wanted_cpu = cpu.unwrap_or(&default_cpu);
    let wanted_os = os.unwrap_or(&default_os);
    let wanted_libc = libc.unwrap_or(&default_libc);

    let current_os = current_os();
    let current_cpu = current_cpu();
    let current_libc = current_libc();
    let os_ok = check_list(&[current_os], wanted_os);
    let cpu_ok = check_list(&[current_cpu], wanted_cpu);
    let libc_ok =
        if current_libc == "unknown" { true } else { check_list(&[current_libc], wanted_libc) };
    if os_ok && cpu_ok && libc_ok {
        return None;
    }

    Some(format!(
        "Unsupported platform for {package_id}: wanted {{\"cpu\":{},\"os\":{},\"libc\":{}}} (current: {{\"os\":\"{}\",\"cpu\":\"{}\",\"libc\":\"{}\"}})",
        serde_json::to_string(wanted_cpu).expect("serialize cpu"),
        serde_json::to_string(wanted_os).expect("serialize os"),
        serde_json::to_string(wanted_libc).expect("serialize libc"),
        current_os,
        current_cpu,
        current_libc
    ))
}

fn engine_warning(package_id: &str, engines: Option<&HashMap<String, String>>) -> Option<String> {
    let engines = engines?;
    let current_node = current_node_version()?;
    let wanted_node = engines.get("node")?;
    let range = wanted_node.parse::<node_semver::Range>().ok()?;
    if current_node.satisfies(&range) {
        return None;
    }
    Some(format!(
        "Unsupported engine for {package_id}: wanted: {{\"node\":\"{wanted_node}\"}} (current: {{\"node\":\"{}\"}})",
        current_node
    ))
}

fn check_list(current_values: &[&str], wanted: &[String]) -> bool {
    if wanted.len() == 1 && wanted[0] == "any" {
        return true;
    }
    let wanted = wanted.iter().filter(|value| !value.is_empty()).collect::<Vec<_>>();
    let mut match_found = false;
    let mut blocked = 0usize;
    for value in current_values {
        for item in &wanted {
            if let Some(negated) = item.strip_prefix('!') {
                if negated == *value {
                    return false;
                }
                blocked += 1;
            } else if item.as_str() == *value {
                match_found = true;
            }
        }
    }
    match_found || blocked == wanted.len()
}

fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn current_cpu() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        other => other,
    }
}

fn current_libc() -> &'static str {
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        "musl"
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        "glibc"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "unknown"
    }
    #[cfg(all(target_os = "linux", not(any(target_env = "musl", target_env = "gnu"))))]
    {
        "unknown"
    }
}

fn current_node_version() -> Option<&'static node_semver::Version> {
    static NODE_VERSION: OnceLock<Option<node_semver::Version>> = OnceLock::new();
    NODE_VERSION
        .get_or_init(|| {
            let output = std::process::Command::new("node").arg("--version").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let version = String::from_utf8(output.stdout).ok()?;
            version.trim().trim_start_matches('v').parse::<node_semver::Version>().ok()
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_lockfile::{LockfileResolution, PackageSnapshot, RegistryResolution};

    #[test]
    fn skips_optional_on_unsupported_os() {
        let package = PackageVersion {
            name: "pkg".to_string(),
            version: "1.0.0".parse().unwrap(),
            dist: Default::default(),
            dependencies: None,
            optional_dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
            engines: None,
            cpu: None,
            os: Some(vec!["definitely-not-this-os".to_string()]),
            libc: None,
            deprecated: None,
            bin: None,
            homepage: None,
            repository: None,
        };
        assert_eq!(
            check_package_version_installability(&package, true),
            Installability::SkipOptional
        );
    }

    #[test]
    fn skips_optional_snapshot_on_unsupported_cpu() {
        let snapshot = PackageSnapshot {
            resolution: LockfileResolution::Registry(RegistryResolution {
                integrity: "sha512-Bw==".parse().unwrap(),
            }),
            id: None,
            name: Some("pkg".to_string()),
            version: Some("1.0.0".to_string()),
            engines: None,
            cpu: Some(vec!["definitely-not-this-cpu".to_string()]),
            os: None,
            libc: None,
            deprecated: None,
            has_bin: None,
            prepare: None,
            requires_build: None,
            bundled_dependencies: None,
            peer_dependencies: None,
            peer_dependencies_meta: None,
            dependencies: None,
            optional_dependencies: None,
            transitive_peer_dependencies: None,
            dev: None,
            optional: Some(true),
        };
        assert_eq!(
            check_package_snapshot_installability("pkg@1.0.0", &snapshot, true),
            Installability::SkipOptional
        );
    }

    #[test]
    fn allows_blacklist_only_when_current_not_listed() {
        assert!(check_list(&["linux"], &["!darwin".to_string()]));
        assert!(!check_list(&["linux"], &["!linux".to_string()]));
    }

    #[test]
    fn normalizes_windows_os_name_like_pnpm() {
        #[cfg(windows)]
        assert_eq!(current_os(), "win32");

        #[cfg(not(windows))]
        assert_ne!(current_os(), "windows");
    }
}
