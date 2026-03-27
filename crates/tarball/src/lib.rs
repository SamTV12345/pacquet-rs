use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{Cursor, Read},
    path::PathBuf,
    sync::Arc,
    time::UNIX_EPOCH,
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STD};
use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_fs::file_mode;
use pacquet_network::ThrottledClient;
use pacquet_store_dir::{
    PackageFileInfo, PackageFilesIndex, StoreDir, WriteCasFileError, WriteIndexFileError,
};
use pipe_trait::Pipe;
use ssri::Integrity;
use tar::Archive;
use tokio::sync::{Notify, RwLock};
use tokio::time::{Duration, sleep};
use tracing::instrument;
use zune_inflate::{DeflateDecoder, DeflateOptions, errors::InflateDecodeErrors};

fn pkg_requires_build(
    manifest: Option<&serde_json::Value>,
    files_index: &HashMap<String, PackageFileInfo>,
) -> bool {
    let has_install_scripts = manifest
        .and_then(|manifest| manifest.get("scripts"))
        .and_then(serde_json::Value::as_object)
        .is_some_and(|scripts| {
            ["preinstall", "install", "postinstall"]
                .into_iter()
                .any(|name| scripts.get(name).is_some_and(|value| !value.is_null()))
        });
    let has_binding_gyp = files_index.contains_key("binding.gyp");
    let has_hooks =
        files_index.keys().any(|name| name.starts_with(".hooks/") || name.starts_with(".hooks\\"));
    has_install_scripts || has_binding_gyp || has_hooks
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to fetch {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    pub error: reqwest::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to verify the integrity of {url}: {error}")]
pub struct VerifyChecksumError {
    pub url: String,
    #[error(source)]
    pub error: ssri::Error,
}

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum TarballError {
    #[diagnostic(code(pacquet_tarball::fetch_tarball))]
    FetchTarball(NetworkError),

    #[from(ignore)]
    #[diagnostic(code(pacquet_tarball::io_error))]
    ReadTarballEntries(std::io::Error),

    #[diagnostic(code(pacquet_tarball::verify_checksum_error))]
    Checksum(VerifyChecksumError),

    #[from(ignore)]
    #[display("Failed to decode gzip: {_0}")]
    #[diagnostic(code(pacquet_tarball::decode_gzip))]
    DecodeGzip(InflateDecodeErrors),

    #[from(ignore)]
    #[display("Failed to write cafs: {_0}")]
    #[diagnostic(transparent)]
    WriteCasFile(WriteCasFileError),

    #[from(ignore)]
    #[display("Failed to write tarball index: {_0}")]
    #[diagnostic(transparent)]
    WriteTarballIndexFile(WriteIndexFileError),

    #[from(ignore)]
    #[display("Failed to fetch {url} in offline mode because it is missing from local store cache")]
    #[diagnostic(code(pacquet_tarball::offline_tarball_missing))]
    OfflineTarballMissing { url: String },

    #[from(ignore)]
    #[diagnostic(code(pacquet_tarball::task_join_error))]
    TaskJoin(tokio::task::JoinError),
}

/// Value of the cache.
#[derive(Debug, Clone)]
pub enum CacheValue {
    /// The package is being processed.
    InProgress(Arc<Notify>),
    /// The package is saved.
    Available(Arc<HashMap<String, PathBuf>>),
}

/// Internal in-memory cache of tarballs.
///
/// The key of this hashmap is the url of each tarball.
pub type MemCache = DashMap<String, Arc<RwLock<CacheValue>>>;

#[instrument(skip(gz_data), fields(gz_data_len = gz_data.len()))]
fn decompress_gzip(gz_data: &[u8], unpacked_size: Option<usize>) -> Result<Vec<u8>, TarballError> {
    let mut options = DeflateOptions::default().set_confirm_checksum(false);

    if let Some(size) = unpacked_size {
        options = options.set_size_hint(size);
    }

    DeflateDecoder::new_with_options(gz_data, options)
        .decode_gzip()
        .map_err(TarballError::DecodeGzip)
}

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub store_dir: &'static StoreDir,
    pub package_id: &'a str,
    pub package_integrity: &'a Integrity,
    pub package_unpacked_size: Option<usize>,
    pub auth_header: Option<String>,
    pub package_url: &'a str,
    pub offline: bool,
    pub force: bool,
}

impl<'a> DownloadTarballToStore<'a> {
    fn cas_paths_from_store_index(
        store_dir: &StoreDir,
        package_integrity: &Integrity,
        package_id: &str,
    ) -> Option<HashMap<String, PathBuf>> {
        let index =
            std::panic::catch_unwind(|| store_dir.read_index_file(package_integrity, package_id))
                .ok()
                .flatten()?;
        let v10_dir = store_dir.tmp().parent()?.to_path_buf();
        let files_dir = v10_dir.join("files");

        let mut cas_paths = HashMap::with_capacity(index.files.len());
        for (file_name, file_info) in index.files {
            let integrity = file_info.integrity.strip_prefix("sha512-")?;
            let hash_bytes = BASE64_STD.decode(integrity).ok()?;
            if hash_bytes.len() != 64 {
                return None;
            }

            let mut hex = String::with_capacity(hash_bytes.len() * 2);
            for byte in &hash_bytes {
                let _ = write!(&mut hex, "{byte:02x}");
            }

            let suffix = if file_mode::is_all_exec(file_info.mode) { "-exec" } else { "" };
            let cas_path = files_dir.join(&hex[..2]).join(format!("{}{}", &hex[2..], suffix));
            if !cas_path.is_file() {
                return None;
            }
            cas_paths.insert(file_name, cas_path);
        }

        Some(cas_paths)
    }

    async fn fetch_tarball_with_retry(
        http_client: &ThrottledClient,
        package_url: &str,
        auth_header: Option<&str>,
    ) -> Result<Vec<u8>, reqwest::Error> {
        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 1_u32;
        loop {
            let result = async {
                let response = http_client
                    .run_with_permit_for_url(package_url, |client| {
                        let mut request = client.get(package_url);
                        if let Some(auth_header) = auth_header {
                            request = request.header("authorization", auth_header);
                        }
                        request.send()
                    })
                    .await?
                    .error_for_status()?;
                response.bytes().await.map(|bytes| bytes.to_vec())
            }
            .await;

            match result {
                Ok(body) => return Ok(body),
                Err(error) if attempt < MAX_ATTEMPTS => {
                    let backoff_ms = 100_u64 * u64::from(attempt);
                    tracing::warn!(
                        target: "pacquet::download",
                        ?package_url,
                        attempt,
                        backoff_ms,
                        "Transient tarball fetch error, retrying: {error}"
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Execute the subroutine with an in-memory cache.
    pub async fn run_with_mem_cache(
        self,
        mem_cache: &'a MemCache,
    ) -> Result<Arc<HashMap<String, PathBuf>>, TarballError> {
        let &DownloadTarballToStore { package_url, .. } = &self;
        loop {
            // QUESTION: I see no copying from existing store_dir, is there such mechanism?
            // TODO: If it's not implemented yet, implement it
            if let Some(cache_lock) = mem_cache.get(package_url) {
                let notify = match &*cache_lock.write().await {
                    CacheValue::Available(cas_paths) => {
                        return Ok(Arc::clone(cas_paths));
                    }
                    CacheValue::InProgress(notify) => Arc::clone(notify),
                };

                tracing::info!(target: "pacquet::download", ?package_url, "Wait for cache");
                notify.notified().await;
                match &*cache_lock.read().await {
                    CacheValue::Available(cas_paths) => {
                        return Ok(Arc::clone(cas_paths));
                    }
                    CacheValue::InProgress(_) => {
                        mem_cache.remove(package_url);
                        continue;
                    }
                }
            } else {
                let notify = Arc::new(Notify::new());
                let cache_lock = notify
                    .pipe_ref(Arc::clone)
                    .pipe(CacheValue::InProgress)
                    .pipe(RwLock::new)
                    .pipe(Arc::new);
                if mem_cache.insert(package_url.to_string(), Arc::clone(&cache_lock)).is_some() {
                    tracing::warn!(target: "pacquet::download", ?package_url, "Race condition detected when writing to cache");
                }
                match self.run_without_mem_cache().await {
                    Ok(cas_paths) => {
                        let cas_paths = cas_paths.pipe(Arc::new);
                        let mut cache_write = cache_lock.write().await;
                        *cache_write = CacheValue::Available(Arc::clone(&cas_paths));
                        drop(cache_write);
                        notify.notify_waiters();
                        return Ok(cas_paths);
                    }
                    Err(error) => {
                        mem_cache.remove(package_url);
                        notify.notify_waiters();
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Execute the subroutine without an in-memory cache.
    pub async fn run_without_mem_cache(&self) -> Result<HashMap<String, PathBuf>, TarballError> {
        let &DownloadTarballToStore {
            http_client,
            store_dir,
            package_id,
            package_integrity,
            package_unpacked_size,
            ref auth_header,
            package_url,
            offline,
            force,
            ..
        } = self;

        if !force
            && let Some(cas_paths) =
                Self::cas_paths_from_store_index(store_dir, package_integrity, package_id)
        {
            tracing::info!(
                target: "pacquet::download",
                ?package_url,
                "Reused package from store index cache"
            );
            return Ok(cas_paths);
        }

        if offline {
            return Err(TarballError::OfflineTarballMissing { url: package_url.to_string() });
        }

        tracing::info!(target: "pacquet::download", ?package_url, "New cache");

        let response =
            Self::fetch_tarball_with_retry(http_client, package_url, auth_header.as_deref())
                .await
                .map_err(|error| {
                    TarballError::FetchTarball(NetworkError { url: package_url.to_string(), error })
                })?;

        tracing::info!(target: "pacquet::download", ?package_url, "Download completed");

        // TODO: Cloning here is less than desirable, there are 2 possible solutions for this problem:
        // 1. Use an Arc and convert this line to Arc::clone.
        // 2. Replace ssri with base64 and serde magic (which supports Copy).
        let package_integrity = package_integrity.clone();
        let package_id = package_id.to_string();

        #[derive(Debug, From)]
        enum TaskError {
            Checksum(ssri::Error),
            Other(TarballError),
        }
        let cas_paths = tokio::task::spawn(async move {
            package_integrity.check(&response).map_err(TaskError::Checksum)?;

            // TODO: move tarball extraction to its own function
            // TODO: test it
            // TODO: test the duplication of entries

            let mut archive = decompress_gzip(&response, package_unpacked_size)
                .map_err(TaskError::Other)?
                .pipe(Cursor::new)
                .pipe(Archive::new);

            let entries = archive
                .entries()
                .map_err(TarballError::ReadTarballEntries)
                .map_err(TaskError::Other)?;

            let ((_, Some(capacity)) | (capacity, None)) = entries.size_hint();
            let mut cas_paths = HashMap::<String, PathBuf>::with_capacity(capacity);
            let mut pkg_files_idx = PackageFilesIndex {
                name: None,
                version: None,
                requires_build: None,
                files: HashMap::with_capacity(capacity),
                side_effects: None,
            };
            let mut package_manifest: Option<serde_json::Value> = None;

            for entry in entries {
                let mut entry = match entry {
                    Ok(e) => e,
                    Err(error) => {
                        tracing::warn!("Skipping unreadable tarball entry: {error}");
                        continue;
                    }
                };

                // Skip directories, symlinks, hardlinks, and other non-regular-file entries.
                let entry_type = entry.header().entry_type();
                if !entry_type.is_file() {
                    if entry_type.is_symlink() || entry_type.is_hard_link() {
                        tracing::warn!(
                            "Skipping symlink/hardlink entry in tarball (potential security risk)"
                        );
                    }
                    continue;
                }

                let file_mode = entry.header().mode().unwrap_or(0o644);
                let file_is_executable = file_mode::is_all_exec(file_mode);

                // Read the contents of the entry
                let mut buffer = Vec::with_capacity(entry.size() as usize);
                if let Err(error) = entry.read_to_end(&mut buffer) {
                    tracing::warn!("Skipping tarball entry with read error: {error}");
                    continue;
                }

                let entry_path = match entry.path() {
                    Ok(p) => p,
                    Err(error) => {
                        tracing::warn!("Skipping tarball entry with invalid path: {error}");
                        continue;
                    }
                };
                let cleaned_entry_path = entry_path
                    .components()
                    .skip(1)
                    .collect::<PathBuf>();

                // Path traversal protection: reject entries that escape the package root.
                for component in cleaned_entry_path.components() {
                    if matches!(component, std::path::Component::ParentDir) {
                        tracing::warn!(
                            ?entry_path,
                            "Skipping tarball entry with path traversal (../)"
                        );
                        continue;
                    }
                }

                let cleaned_entry_path = match cleaned_entry_path.into_os_string().into_string() {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::warn!(?entry_path, "Skipping tarball entry with non-UTF-8 path");
                        continue;
                    }
                };

                if cleaned_entry_path.is_empty() {
                    continue;
                }
                if cleaned_entry_path == "package.json" {
                    package_manifest = serde_json::from_slice(&buffer).ok();
                }
                let (file_path, file_hash) = store_dir
                    .write_cas_file(&buffer, file_is_executable)
                    .map_err(TarballError::WriteCasFile)?;

                if let Some(previous) = cas_paths.insert(cleaned_entry_path.clone(), file_path) {
                    tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
                }

                let checked_at = UNIX_EPOCH.elapsed().ok().map(|x| x.as_millis());
                let file_size = entry.header().size().ok();
                let file_integrity = format!("sha512-{}", BASE64_STD.encode(file_hash));
                let file_attrs = PackageFileInfo {
                    checked_at,
                    integrity: file_integrity,
                    mode: file_mode,
                    size: file_size,
                };

                if let Some(previous) = pkg_files_idx.files.insert(cleaned_entry_path, file_attrs) {
                    tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
                }
            }

            pkg_files_idx.name = package_manifest
                .as_ref()
                .and_then(|manifest| manifest.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            pkg_files_idx.version = package_manifest
                .as_ref()
                .and_then(|manifest| manifest.get("version"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            pkg_files_idx.requires_build =
                Some(pkg_requires_build(package_manifest.as_ref(), &pkg_files_idx.files));

            store_dir
                .write_index_file(&package_integrity, &package_id, &pkg_files_idx)
                .map_err(TarballError::WriteTarballIndexFile)?;

            Ok(cas_paths)
        })
        .await
        .map_err(TarballError::TaskJoin)?
        .map_err(|error| match error {
            TaskError::Checksum(error) => {
                TarballError::Checksum(VerifyChecksumError { url: package_url.to_string(), error })
            }
            TaskError::Other(error) => error,
        })?;

        tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

        Ok(cas_paths)
    }
}

#[cfg(test)]
mod tests {
    use flate2::{Compression, write::GzEncoder};
    use mockito::Server;
    use pipe_trait::Pipe;
    use sha2::{Digest, Sha512};
    use std::io::Write;
    use tempfile::{TempDir, tempdir};

    use super::*;

    fn integrity(integrity_str: &str) -> Integrity {
        integrity_str.parse().expect("parse integrity string")
    }

    fn create_tgz_with_single_file(path: &str, content: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::<u8>::new());
        let mut header = tar::Header::new_gnu();
        header.set_path(path).expect("set tar path");
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, content).expect("append tar entry");
        let tar_bytes = builder.into_inner().expect("finish tar build");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip write");
        encoder.finish().expect("finish gzip")
    }

    /// **Problem:**
    /// The tested function requires `'static` paths, leaking would prevent
    /// temporary files from being cleaned up.
    ///
    /// **Solution:**
    /// Create [`TempDir`] as a temporary variable (which can be dropped)
    /// but provide its path as `'static`.
    ///
    /// **Side effect:**
    /// The `'static` path becomes dangling outside the scope of [`TempDir`].
    fn tempdir_with_leaked_path() -> (TempDir, &'static StoreDir) {
        let tempdir = tempdir().unwrap();
        let leaked_path =
            tempdir.path().to_path_buf().pipe(StoreDir::from).pipe(Box::new).pipe(Box::leak);
        (tempdir, leaked_path)
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn packages_under_orgs_should_work() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let cas_files = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "@fastify/error@3.3.0",
            package_integrity: &integrity("sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            auth_header: None,
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            offline: false,
            force: false,
        }
        .run_without_mem_cache()
        .await
        .unwrap();

        let mut filenames = cas_files.keys().collect::<Vec<_>>();
        filenames.sort();
        assert_eq!(
            filenames,
            vec![
                ".github/dependabot.yml",
                ".github/workflows/ci.yml",
                ".taprc",
                "LICENSE",
                "README.md",
                "benchmarks/create.js",
                "benchmarks/instantiate.js",
                "benchmarks/no-stack.js",
                "benchmarks/toString.js",
                "index.js",
                "package.json",
                "test/index.test.js",
                "types/index.d.ts",
                "types/index.test-d.ts"
            ]
        );

        drop(store_dir);
    }

    #[tokio::test]
    async fn should_throw_error_on_checksum_mismatch() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "@fastify/error@3.3.0",
            package_integrity: &integrity("sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            auth_header: None,
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            offline: false,
            force: false,
        }
        .run_without_mem_cache()
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn should_reuse_store_index_without_network() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let integrity = integrity(
            "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
        );

        let first = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "@fastify/error@3.3.0",
            package_integrity: &integrity,
            package_unpacked_size: Some(16697),
            auth_header: None,
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            offline: false,
            force: false,
        }
        .run_without_mem_cache()
        .await
        .expect("first download");

        let second = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "@fastify/error@3.3.0",
            package_integrity: &integrity,
            package_unpacked_size: Some(16697),
            auth_header: None,
            package_url: "http://127.0.0.1:1/this-url-should-never-be-called.tgz",
            offline: false,
            force: false,
        }
        .run_without_mem_cache()
        .await
        .expect("reuse from store cache");

        assert_eq!(first.len(), second.len());

        drop(store_dir);
    }

    #[tokio::test]
    async fn should_send_auth_header_when_downloading_tarball() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let mut server = Server::new_async().await;
        let body = create_tgz_with_single_file(
            "package/package.json",
            br#"{"name":"pkg","version":"1.0.0"}"#,
        );
        let hash = Sha512::digest(&body);
        let integrity = integrity(&format!("sha512-{}", BASE64_STD.encode(hash)));
        let _mock = server
            .mock("GET", "/pkg.tgz")
            .match_header("authorization", "Bearer tarball-secret")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let url = format!("{}/pkg.tgz", server.url());

        let cas_files = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "pkg@1.0.0",
            package_integrity: &integrity,
            package_unpacked_size: None,
            auth_header: Some("Bearer tarball-secret".to_string()),
            package_url: &url,
            offline: false,
            force: false,
        }
        .run_without_mem_cache()
        .await
        .expect("download tarball with auth header");

        assert!(cas_files.contains_key("package.json"));
        drop(store_dir);
    }

    #[tokio::test]
    async fn mem_cache_should_clear_failed_in_progress_entry_before_retry() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let mut server = Server::new_async().await;
        let body = create_tgz_with_single_file(
            "package/package.json",
            br#"{"name":"pkg","version":"1.0.0"}"#,
        );
        let hash = Sha512::digest(&body);
        let integrity = integrity(&format!("sha512-{}", BASE64_STD.encode(hash)));
        let url = format!("{}/pkg.tgz", server.url());
        let mem_cache = MemCache::default();

        let _first = server.mock("GET", "/pkg.tgz").with_status(500).create_async().await;
        DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "pkg@1.0.0",
            package_integrity: &integrity,
            package_unpacked_size: None,
            auth_header: None,
            package_url: &url,
            offline: false,
            force: false,
        }
        .run_with_mem_cache(&mem_cache)
        .await
        .expect_err("first attempt should fail");

        assert!(mem_cache.get(&url).is_none());

        server.reset();
        let _second =
            server.mock("GET", "/pkg.tgz").with_status(200).with_body(body).create_async().await;

        let cas_files = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_id: "pkg@1.0.0",
            package_integrity: &integrity,
            package_unpacked_size: None,
            auth_header: None,
            package_url: &url,
            offline: false,
            force: false,
        }
        .run_with_mem_cache(&mem_cache)
        .await
        .expect("second attempt should succeed");

        assert!(cas_files.contains_key("package.json"));
        drop(store_dir);
    }
}
