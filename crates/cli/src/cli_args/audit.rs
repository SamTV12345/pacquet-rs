use crate::cli_args::registry_client::RegistryClient;
use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_lockfile::{DependencyPathSpecifier, Lockfile, PackageSnapshot};
use pacquet_npmrc::Npmrc;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args, Default)]
pub struct AuditArgs {
    /// Output audit report in JSON format.
    #[arg(long)]
    json: bool,

    /// Only print advisories with severity greater than or equal to the provided one.
    #[arg(long = "audit-level")]
    audit_level: Option<String>,

    /// Only audit devDependencies.
    #[arg(short = 'D', long)]
    dev: bool,

    /// Only audit dependencies and optionalDependencies.
    #[arg(short = 'P', long = "prod")]
    prod: bool,

    /// Don't audit optionalDependencies.
    #[arg(long = "no-optional")]
    no_optional: bool,

    /// Use exit code 0 if the registry responds with an error.
    #[arg(long = "ignore-registry-errors")]
    ignore_registry_errors: bool,

    /// Add overrides to package.json to force non-vulnerable versions.
    #[arg(long)]
    fix: bool,
}

// ── severity helpers ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Low,
    Moderate,
    High,
    Critical,
}

impl Severity {
    fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Some(Self::Info),
            "low" => Some(Self::Low),
            "moderate" => Some(Self::Moderate),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Moderate => "moderate",
            Severity::High => "high",
            Severity::Critical => "critical",
        };
        f.write_str(s)
    }
}

// ── advisory structs ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Advisory {
    id: u64,
    severity: Severity,
    vulnerable_versions: String,
    patched_versions: String,
    module_name: String,
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    cves: Vec<String>,
}

// ── lockfile helpers ──────────────────────────────────────────────────

/// Which categories a package belongs to, derived from the lockfile
/// `dev` / `optional` flags on `PackageSnapshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepKind {
    Prod,
    Dev,
    Optional,
}

fn dep_kind(snapshot: &PackageSnapshot) -> DepKind {
    if snapshot.dev == Some(true) {
        DepKind::Dev
    } else if snapshot.optional == Some(true) {
        DepKind::Optional
    } else {
        DepKind::Prod
    }
}

/// Extract `(name, version, DepKind)` tuples from the lockfile packages map.
fn extract_packages(lockfile: &Lockfile) -> Vec<(String, String, DepKind)> {
    let packages = match &lockfile.packages {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut result = Vec::with_capacity(packages.len());
    for (dep_path, snapshot) in packages {
        let name = dep_path.package_name().to_string();

        // Prefer the explicit `version` field on the snapshot.
        // Fall back to the version embedded in the dependency path for
        // registry packages.
        let version = snapshot
            .version
            .clone()
            .or_else(|| {
                if let DependencyPathSpecifier::Registry(spec) = &dep_path.package_specifier {
                    Some(spec.suffix.version().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        if version.is_empty() {
            continue;
        }

        result.push((name, version, dep_kind(snapshot)));
    }
    result
}

/// Build the JSON body for the bulk advisory endpoint:
/// `{ "pkg": ["1.0.0"], "other": ["2.0.0", "3.0.0"], … }`
fn build_bulk_body(packages: &[(String, String, DepKind)]) -> Value {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, version, _) in packages {
        map.entry(name.clone()).or_default().push(version.clone());
    }
    // Deduplicate versions.
    for versions in map.values_mut() {
        versions.sort();
        versions.dedup();
    }
    serde_json::to_value(map).unwrap_or_default()
}

// ── display helpers ───────────────────────────────────────────────────

fn print_table(advisories: &[&Advisory]) {
    if advisories.is_empty() {
        println!("found 0 vulnerabilities");
        return;
    }

    let sep = "─".repeat(70);
    for adv in advisories {
        println!("{sep}");
        println!("{:<18} {}", format!("{} severity", adv.severity), adv.title);
        println!("{:<18} {}", "Package", adv.module_name);
        println!("{:<18} {}", "Vulnerable range", adv.vulnerable_versions);
        if !adv.patched_versions.is_empty() {
            println!("{:<18} {}", "Patched in", adv.patched_versions);
        }
        if !adv.cves.is_empty() {
            println!("{:<18} {}", "CVEs", adv.cves.join(", "));
        }
        if !adv.url.is_empty() {
            println!("{:<18} {}", "More info", adv.url);
        }
    }
    println!("{sep}");
    let count = advisories.len();
    let word = if count == 1 { "vulnerability" } else { "vulnerabilities" };
    println!("found {count} {word}");
}

fn print_json(advisories: &[&Advisory]) {
    let arr: Vec<Value> = advisories
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id,
                "severity": a.severity.to_string(),
                "vulnerable_versions": a.vulnerable_versions,
                "patched_versions": a.patched_versions,
                "module_name": a.module_name,
                "title": a.title,
                "url": a.url,
                "cves": a.cves,
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "advisories": arr,
            "metadata": {
                "vulnerabilities": {
                    "total": arr.len()
                }
            }
        }))
        .unwrap_or_default()
    );
}

// ── core implementation ───────────────────────────────────────────────

impl AuditArgs {
    pub fn run(self, dir: PathBuf, npmrc: &Npmrc) -> miette::Result<()> {
        if !dir.join("pnpm-lock.yaml").is_file() {
            miette::bail!("No pnpm-lock.yaml found: Cannot audit a project without a lockfile");
        }

        let lockfile = Lockfile::load_from_dir(&dir)
            .wrap_err("load pnpm-lock.yaml")?
            .ok_or_else(|| miette::miette!("pnpm-lock.yaml could not be loaded"))?;

        let all_packages = extract_packages(&lockfile);

        // Filter by --dev / --prod / --no-optional.
        let filtered: Vec<(String, String, DepKind)> = all_packages
            .into_iter()
            .filter(|(_, _, kind)| {
                if self.dev && !self.prod {
                    *kind == DepKind::Dev
                } else if self.prod && !self.dev {
                    *kind == DepKind::Prod || *kind == DepKind::Optional
                } else {
                    true
                }
            })
            .filter(|(_, _, kind)| if self.no_optional { *kind != DepKind::Optional } else { true })
            .collect();

        if filtered.is_empty() {
            println!("found 0 vulnerabilities");
            return Ok(());
        }

        let body = build_bulk_body(&filtered);
        let client = RegistryClient::new(npmrc);
        let registry = client.default_registry();
        let url = format!("{registry}/-/npm/v1/security/advisories/bulk");

        let response = client.post_json_anonymous(&url, &body);

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                if self.ignore_registry_errors {
                    return Ok(());
                }
                return Err(e);
            }
        };

        let status = response.status();
        if !status.is_success() {
            if self.ignore_registry_errors {
                return Ok(());
            }
            miette::bail!("Registry returned HTTP {status}");
        }

        let response_text =
            response.text().into_diagnostic().wrap_err("read advisory response body")?;

        // The response is keyed by package name, each value is an array of advisories.
        let raw: HashMap<String, Vec<Advisory>> = serde_json::from_str(&response_text)
            .into_diagnostic()
            .wrap_err("parse advisory response")?;

        // Collect names we actually care about (after dep-kind filtering).
        let wanted_names: HashSet<&str> =
            filtered.iter().map(|(name, _, _)| name.as_str()).collect();

        let mut all_advisories: Vec<Advisory> = raw
            .into_iter()
            .filter(|(name, _)| wanted_names.contains(name.as_str()))
            .flat_map(|(_, advs)| advs)
            .collect();

        // Filter by --audit-level.
        let min_severity = self
            .audit_level
            .as_deref()
            .map(Severity::from_str_loose)
            .unwrap_or(Some(Severity::Info))
            .unwrap_or(Severity::Info);

        all_advisories.retain(|a| a.severity >= min_severity);
        all_advisories.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.id.cmp(&b.id)));

        if self.fix {
            return self.run_fix_native(&dir, &all_advisories);
        }

        let refs: Vec<&Advisory> = all_advisories.iter().collect();
        if self.json {
            print_json(&refs);
        } else {
            print_table(&refs);
        }

        if !all_advisories.is_empty() {
            let count = all_advisories.len();
            let word = if count == 1 { "vulnerability" } else { "vulnerabilities" };
            miette::bail!("{count} {word} found");
        }

        Ok(())
    }

    fn run_fix_native(&self, dir: &Path, advisories: &[Advisory]) -> miette::Result<()> {
        let overrides = create_overrides_from_advisories(advisories);
        if overrides.is_empty() {
            println!("No fixable vulnerabilities found");
            return Ok(());
        }

        let manifest_path = dir.join("package.json");
        write_overrides(&manifest_path, &overrides)?;

        println!("Added overrides to package.json:");
        for (selector, patched) in &overrides {
            println!("  {selector}: {patched}");
        }
        println!("\nRun `pacquet install` to apply the overrides.");
        Ok(())
    }
}

// ── fix helpers (shared) ──────────────────────────────────────────────

fn create_overrides_from_advisories(advisories: &[Advisory]) -> BTreeMap<String, String> {
    advisories
        .iter()
        .filter(|a| {
            a.vulnerable_versions != ">=0.0.0"
                && a.patched_versions != "<0.0.0"
                && !a.patched_versions.is_empty()
        })
        .map(|a| {
            (format!("{}@{}", a.module_name, a.vulnerable_versions), a.patched_versions.clone())
        })
        .collect()
}

fn write_overrides(
    manifest_path: &Path,
    overrides: &BTreeMap<String, String>,
) -> miette::Result<()> {
    let content = fs::read_to_string(manifest_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let mut value: Value =
        serde_json::from_str(&content).into_diagnostic().wrap_err("parse package.json")?;

    let root = value
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json root must be an object"))?;
    let pnpm = root.entry("pnpm").or_insert_with(|| Value::Object(Map::new()));
    let pnpm = pnpm
        .as_object_mut()
        .ok_or_else(|| miette::miette!("package.json pnpm field must be an object"))?;
    let existing_overrides = pnpm.entry("overrides").or_insert_with(|| Value::Object(Map::new()));
    let existing_overrides = existing_overrides
        .as_object_mut()
        .ok_or_else(|| miette::miette!("pnpm.overrides must be an object"))?;

    for (selector, patched) in overrides {
        existing_overrides.insert(selector.clone(), Value::String(patched.clone()));
    }

    let rendered = serde_json::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("serialize package.json")?;
    fs::write(manifest_path, format!("{rendered}\n"))
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", manifest_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct AuditAdvisoryLegacy {
        module_name: String,
        vulnerable_versions: String,
        patched_versions: String,
    }

    /// Used by the native --fix path. Mirrors the old npm-based report shape.
    #[derive(Debug, Deserialize)]
    struct AuditReport {
        #[serde(default)]
        advisories: BTreeMap<String, AuditAdvisoryLegacy>,
    }

    fn create_overrides(report: &AuditReport) -> BTreeMap<String, String> {
        report
            .advisories
            .values()
            .filter(|advisory| {
                advisory.vulnerable_versions != ">=0.0.0"
                    && advisory.patched_versions != "<0.0.0"
                    && !advisory.patched_versions.is_empty()
            })
            .map(|advisory| {
                (
                    format!("{}@{}", advisory.module_name, advisory.vulnerable_versions),
                    advisory.patched_versions.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn create_overrides_filters_unfixable() {
        let report: AuditReport = serde_json::from_value(serde_json::json!({
            "advisories": {
                "1": {
                    "module_name": "axios",
                    "vulnerable_versions": "<=0.18.0",
                    "patched_versions": ">=0.18.1"
                },
                "2": {
                    "module_name": "unfixable",
                    "vulnerable_versions": ">=0.0.0",
                    "patched_versions": "<0.0.0"
                },
                "3": {
                    "module_name": "no-patch",
                    "vulnerable_versions": "<1.0.0",
                    "patched_versions": ""
                }
            }
        }))
        .unwrap();

        let overrides = create_overrides(&report);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides.get("axios@<=0.18.0").unwrap(), ">=0.18.1");
    }

    #[test]
    fn create_overrides_from_advisories_filters_unfixable() {
        let advisories = vec![
            Advisory {
                id: 1,
                severity: Severity::High,
                module_name: "axios".to_string(),
                vulnerable_versions: "<=0.18.0".to_string(),
                patched_versions: ">=0.18.1".to_string(),
                title: "SSRF".to_string(),
                url: String::new(),
                cves: Vec::new(),
            },
            Advisory {
                id: 2,
                severity: Severity::Low,
                module_name: "unfixable".to_string(),
                vulnerable_versions: ">=0.0.0".to_string(),
                patched_versions: "<0.0.0".to_string(),
                title: "bad".to_string(),
                url: String::new(),
                cves: Vec::new(),
            },
            Advisory {
                id: 3,
                severity: Severity::Moderate,
                module_name: "no-patch".to_string(),
                vulnerable_versions: "<1.0.0".to_string(),
                patched_versions: String::new(),
                title: "no fix".to_string(),
                url: String::new(),
                cves: Vec::new(),
            },
        ];

        let overrides = create_overrides_from_advisories(&advisories);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides.get("axios@<=0.18.0").unwrap(), ">=0.18.1");
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Low < Severity::Moderate);
        assert!(Severity::Moderate < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }
}
