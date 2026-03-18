use crate::cli_args::run::run_named_script;
use clap::Args;
use flate2::{Compression, write::GzEncoder};
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct PackArgs {
    /// Do everything pack would do except writing the tarball.
    #[arg(long)]
    dry_run: bool,

    /// Directory in which pacquet pack will save tarballs.
    #[arg(long = "pack-destination")]
    pack_destination: Option<PathBuf>,

    /// Custom output path for the tarball. Supports %s and %v placeholders.
    #[arg(long)]
    out: Option<String>,

    /// Print tarball details and contents as JSON.
    #[arg(long)]
    json: bool,

    /// Skip lifecycle scripts.
    #[arg(long)]
    ignore_scripts: bool,
}

#[derive(Debug, Serialize)]
struct PackJson {
    name: String,
    version: String,
    filename: String,
    files: Vec<PackFileJson>,
}

#[derive(Debug, Serialize)]
struct PackFileJson {
    path: String,
}

impl PackArgs {
    pub fn run(self, manifest_path: PathBuf, config: &Npmrc) -> miette::Result<()> {
        let PackArgs { dry_run, pack_destination, out, json, ignore_scripts } = self;
        if pack_destination.is_some() && out.is_some() {
            miette::bail!("Cannot use --pack-destination and --out together");
        }

        if !ignore_scripts {
            let _ = run_named_script(manifest_path.clone(), "prepack", &[], true, false, config);
            let _ = run_named_script(manifest_path.clone(), "prepare", &[], true, false, config);
        }

        let manifest = PackageManifest::from_path(manifest_path.clone())
            .wrap_err("load package.json for pack")?;
        let package_name = manifest
            .value()
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| miette::miette!("Package name is not defined in package.json"))?;
        let version = manifest
            .value()
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| miette::miette!("Package version is not defined in package.json"))?;
        let package_dir =
            manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));

        let tarball_name = resolve_tarball_name(package_name, version, out.as_deref())?;
        let destination_dir = match (pack_destination, out.as_deref()) {
            (Some(path), _) => {
                if path.is_absolute() {
                    path
                } else {
                    package_dir.join(path)
                }
            }
            (None, Some(path)) => {
                let path = PathBuf::from(
                    path.replace("%s", &normalize_package_name(package_name))
                        .replace("%v", version),
                );
                path.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(|parent| {
                        if parent.is_absolute() {
                            parent.to_path_buf()
                        } else {
                            package_dir.join(parent)
                        }
                    })
                    .unwrap_or_else(|| package_dir.clone())
            }
            (None, None) => package_dir.clone(),
        };
        let tarball_path = destination_dir.join(&tarball_name);

        let mut files = collect_pack_files(&package_dir, &tarball_path)?;
        files.sort();

        if !dry_run {
            fs::create_dir_all(&destination_dir)
                .into_diagnostic()
                .wrap_err_with(|| format!("create {}", destination_dir.display()))?;
            write_tarball(&package_dir, &tarball_path, &files)?;
        }

        if !ignore_scripts {
            let _ = run_named_script(manifest_path.clone(), "postpack", &[], true, false, config);
        }

        let relative_tarball = tarball_path
            .strip_prefix(&package_dir)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| tarball_path.clone());
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&PackJson {
                    name: package_name.to_string(),
                    version: version.to_string(),
                    filename: relative_tarball.display().to_string(),
                    files: files
                        .iter()
                        .map(|path| PackFileJson {
                            path: path.display().to_string().replace('\\', "/"),
                        })
                        .collect(),
                })
                .into_diagnostic()
                .wrap_err("serialize pack json output")?
            );
        } else {
            println!("{}", relative_tarball.display());
        }

        Ok(())
    }
}

fn resolve_tarball_name(
    package_name: &str,
    version: &str,
    out: Option<&str>,
) -> miette::Result<String> {
    if let Some(out) = out {
        let expanded =
            out.replace("%s", &normalize_package_name(package_name)).replace("%v", version);
        let path = PathBuf::from(expanded);
        return Ok(path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| miette::miette!("Invalid --out path"))?
            .to_string());
    }
    Ok(format!("{}-{version}.tgz", normalize_package_name(package_name)))
}

fn normalize_package_name(package_name: &str) -> String {
    package_name.replace('@', "").replace('/', "-")
}

fn collect_pack_files(package_dir: &Path, tarball_path: &Path) -> miette::Result<Vec<PathBuf>> {
    fn walk(
        dir: &Path,
        package_dir: &Path,
        tarball_path: &Path,
        files: &mut Vec<PathBuf>,
    ) -> miette::Result<()> {
        for entry in fs::read_dir(dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("read {}", dir.display()))?
        {
            let entry = entry.into_diagnostic().wrap_err("read pack entry")?;
            let path = entry.path();
            let file_type = entry.file_type().into_diagnostic().wrap_err("read pack file type")?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();

            if matches!(file_name.as_ref(), "node_modules" | ".git" | "target") {
                continue;
            }
            if path == tarball_path {
                continue;
            }
            if file_type.is_dir() {
                walk(&path, package_dir, tarball_path, files)?;
                continue;
            }
            if file_type.is_file() {
                files.push(path.strip_prefix(package_dir).unwrap_or(&path).to_path_buf());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(package_dir, package_dir, tarball_path, &mut files)?;
    Ok(files)
}

fn write_tarball(package_dir: &Path, tarball_path: &Path, files: &[PathBuf]) -> miette::Result<()> {
    let tarball = fs::File::create(tarball_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("create {}", tarball_path.display()))?;
    let encoder = GzEncoder::new(tarball, Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for relative_path in files {
        builder
            .append_path_with_name(
                package_dir.join(relative_path),
                Path::new("package").join(relative_path),
            )
            .into_diagnostic()
            .wrap_err_with(|| format!("append {}", relative_path.display()))?;
    }

    builder.into_inner().into_diagnostic()?.finish().into_diagnostic()?;
    Ok(())
}
