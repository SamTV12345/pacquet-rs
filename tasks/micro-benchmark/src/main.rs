use std::{collections::HashMap, fs, path::Path};

use clap::Parser;
use criterion::{Criterion, Throughput};
use mockito::ServerGuard;
use pacquet_lockfile::Lockfile;
use pacquet_network::ThrottledClient;
use pacquet_store_dir::StoreDir;
use pacquet_tarball::DownloadTarballToStore;
use pipe_trait::Pipe;
use project_root::get_project_root;
use ssri::Integrity;
use tempfile::tempdir;

#[derive(Debug, Parser)]
struct CliArgs {
    #[clap(long)]
    save_baseline: Option<String>,
}

/// Benchmark: Download and extract a tarball to the content-addressable store.
fn bench_tarball(c: &mut Criterion, server: &mut ServerGuard, fixtures_folder: &Path) {
    let mut group = c.benchmark_group("tarball");
    let file = fs::read(fixtures_folder.join("@fastify+error-3.3.0.tgz")).unwrap();
    server.mock("GET", "/@fastify+error-3.3.0.tgz").with_status(201).with_body(&file).create();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let url = &format!("{0}/@fastify+error-3.3.0.tgz", server.url());
    let package_integrity: Integrity = "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==".parse().expect("parse integrity string");

    group.throughput(Throughput::Bytes(file.len() as u64));
    group.bench_function("download_dependency", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempdir().unwrap();
            let store_dir =
                dir.path().to_path_buf().pipe(StoreDir::from).pipe(Box::new).pipe(Box::leak);
            let http_client = ThrottledClient::new_from_cpu_count();

            let cas_map = DownloadTarballToStore {
                http_client: &http_client,
                store_dir,
                package_id: "@fastify/error@3.3.0",
                package_integrity: &package_integrity,
                package_unpacked_size: Some(16697),
                auth_header: None,
                package_url: url,
                offline: false,
                force: false,
            }
            .run_without_mem_cache()
            .await
            .unwrap();
            cas_map.len()
        });
    });

    group.finish();
}

/// Benchmark: Parse a registry package metadata JSON response.
fn bench_registry_parse(c: &mut Criterion, server: &mut ServerGuard) {
    let mut group = c.benchmark_group("registry");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    // Create a realistic registry response with multiple versions
    let registry_body = serde_json::json!({
        "name": "is-number",
        "dist-tags": { "latest": "7.0.0" },
        "versions": {
            "5.0.0": {
                "name": "is-number",
                "version": "5.0.0",
                "dist": { "tarball": "https://registry.npmjs.org/is-number/-/is-number-5.0.0.tgz", "integrity": "sha512-fake1==" }
            },
            "6.0.0": {
                "name": "is-number",
                "version": "6.0.0",
                "dist": { "tarball": "https://registry.npmjs.org/is-number/-/is-number-6.0.0.tgz", "integrity": "sha512-fake2==" }
            },
            "7.0.0": {
                "name": "is-number",
                "version": "7.0.0",
                "dist": { "tarball": "https://registry.npmjs.org/is-number/-/is-number-7.0.0.tgz", "integrity": "sha512-fake3==" }
            }
        }
    });
    let body_bytes = serde_json::to_vec(&registry_body).unwrap();
    server
        .mock("GET", "/is-number")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body_bytes)
        .create();

    let url = server.url();
    let registry = format!("{url}/");

    group.throughput(Throughput::Bytes(body_bytes.len() as u64));
    group.bench_function("fetch_and_parse_metadata", |b| {
        b.to_async(&rt).iter(|| async {
            let http_client = ThrottledClient::new_from_cpu_count();
            let package = pacquet_registry::Package::fetch_from_registry(
                "is-number",
                &http_client,
                &registry,
                None,
            )
            .await
            .unwrap();
            package.pinned_version("^7.0.0").unwrap().version.clone()
        });
    });

    group.bench_function("version_resolution", |b| {
        // Pre-fetch the package once, then benchmark resolution only
        let package: pacquet_registry::Package =
            serde_json::from_value(registry_body.clone()).unwrap();
        b.iter(|| {
            let v = package.pinned_version("^6.0.0").unwrap();
            v.version.major
        });
    });

    group.finish();
}

/// Benchmark: Parse and serialize a lockfile (pnpm-lock.yaml).
fn bench_lockfile(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfile");

    // Use the integrated benchmark fixture as a realistic lockfile
    let root = get_project_root().unwrap();
    let lockfile_path = root.join("tasks/integrated-benchmark/src/fixtures/pnpm-lock.yaml");
    if !lockfile_path.exists() {
        eprintln!("Skipping lockfile benchmarks: fixture not found at {}", lockfile_path.display());
        return;
    }

    let lockfile_bytes = fs::metadata(&lockfile_path).unwrap().len();
    group.throughput(Throughput::Bytes(lockfile_bytes));

    group.bench_function("load_lockfile_from_path", |b| {
        b.iter(|| {
            let lockfile = Lockfile::load_from_path(&lockfile_path).unwrap().unwrap();
            lockfile.lockfile_version.major
        });
    });

    group.bench_function("load_and_save_roundtrip", |b| {
        let dir = tempdir().unwrap();
        b.iter(|| {
            let lockfile = Lockfile::load_from_path(&lockfile_path).unwrap().unwrap();
            lockfile.save_to_dir(dir.path()).unwrap();
        });
    });

    group.finish();
}

/// Benchmark: Content-addressable store operations (write + read index).
fn bench_store_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("store");

    let content = br#"{"name":"bench-pkg","version":"1.0.0","main":"index.js"}"#;

    group.throughput(Throughput::Bytes(content.len() as u64));

    group.bench_function("write_cas_file", |b| {
        let dir = tempdir().unwrap();
        let store_dir = StoreDir::new(dir.path());
        b.iter(|| {
            let (path, _hash) = store_dir.write_cas_file(content, false).unwrap();
            path
        });
    });

    group.bench_function("write_and_read_index", |b| {
        let dir = tempdir().unwrap();
        let store_dir = StoreDir::new(dir.path());
        let integrity = ssri::IntegrityOpts::new()
            .algorithm(ssri::Algorithm::Sha512)
            .chain(b"bench-tarball-content")
            .result();
        let index = pacquet_store_dir::PackageFilesIndex {
            name: Some("bench-pkg".to_string()),
            version: Some("1.0.0".to_string()),
            requires_build: Some(false),
            files: HashMap::from([(
                "package.json".to_string(),
                pacquet_store_dir::PackageFileInfo {
                    checked_at: Some(0),
                    integrity: "sha512-fake==".to_string(),
                    mode: 0o644,
                    size: Some(content.len() as u64),
                },
            )]),
            side_effects: None,
        };
        b.iter(|| {
            store_dir.write_index_file(&integrity, "bench-pkg@1.0.0", &index).unwrap();
            store_dir.read_index_file(&integrity, "bench-pkg@1.0.0").unwrap()
        });
    });

    group.finish();
}

pub fn main() -> Result<(), String> {
    let mut server = mockito::Server::new();
    let CliArgs { save_baseline } = CliArgs::parse();
    let root = get_project_root().unwrap();
    let fixtures_folder = root.join("tasks/micro-benchmark/fixtures");

    let mut criterion = Criterion::default().without_plots();
    if let Some(baseline) = save_baseline {
        criterion = criterion.save_baseline(baseline);
    }

    bench_tarball(&mut criterion, &mut server, &fixtures_folder);
    bench_registry_parse(&mut criterion, &mut server);
    bench_lockfile(&mut criterion);
    bench_store_operations(&mut criterion);

    Ok(())
}
