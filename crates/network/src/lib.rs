use pipe_trait::Pipe;
use reqwest::{Certificate, Client, Identity, NoProxy, Proxy};
use std::{
    collections::HashMap,
    future::IntoFuture,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;
use url::Url;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryTlsConfig {
    pub ca: Option<String>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThrottledClientOptions {
    pub request_timeout_ms: Option<u64>,
    pub strict_ssl: bool,
    pub ca_certs: Vec<String>,
    pub registry_tls_configs: HashMap<String, RegistryTlsConfig>,
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
}

/// Wrapper around [`Client`] with concurrent request limit enforced by the [`Semaphore`] mechanism.
#[derive(Debug)]
pub struct ThrottledClient {
    semaphore: Semaphore,
    client: Arc<Client>,
    permits: usize,
    request_timeout_ms: Option<u64>,
    strict_ssl: bool,
    ca_certs: Vec<String>,
    registry_tls_configs: HashMap<String, RegistryTlsConfig>,
    https_proxy: Option<String>,
    http_proxy: Option<String>,
    no_proxy: Option<String>,
    registry_clients: Mutex<HashMap<String, Arc<Client>>>,
}

impl ThrottledClient {
    /// Acquire a permit and run `proc` with the underlying [`Client`].
    pub async fn run_with_permit<Proc, ProcFuture>(&self, proc: Proc) -> ProcFuture::Output
    where
        Proc: FnOnce(&Client) -> ProcFuture,
        ProcFuture: IntoFuture,
    {
        let permit =
            self.semaphore.acquire().await.expect("semaphore shouldn't have been closed this soon");
        let result = proc(&self.client).await;
        drop(permit);
        result
    }

    /// Acquire a permit and run `proc` with the underlying [`Client`], selecting a TLS-configured
    /// client for the requested URL when needed.
    pub async fn run_with_permit_for_url<Proc, ProcFuture>(
        &self,
        url: &str,
        proc: Proc,
    ) -> ProcFuture::Output
    where
        Proc: FnOnce(&Client) -> ProcFuture,
        ProcFuture: IntoFuture,
    {
        let permit =
            self.semaphore.acquire().await.expect("semaphore shouldn't have been closed this soon");
        let client = self.client_for_url(url);
        let result = proc(client.as_ref()).await;
        drop(permit);
        result
    }

    /// Construct a new throttled client based on the number of CPUs.
    /// If the number of CPUs is greater than 16, the number of permits will be equal to the number of CPUs.
    /// Otherwise, the number of permits will be 16.
    pub fn new_from_cpu_count() -> Self {
        const MIN_PERMITS: usize = 16;
        Self::new_with_limit(num_cpus::get().max(MIN_PERMITS))
    }

    /// Construct a new throttled client with a fixed permit count.
    pub fn new_with_limit(permits: usize) -> Self {
        Self::new_with_options(permits, ThrottledClientOptions::default())
    }

    /// Construct a new throttled client with custom options.
    pub fn new_with_options(permits: usize, options: ThrottledClientOptions) -> Self {
        let ThrottledClientOptions {
            request_timeout_ms,
            strict_ssl,
            ca_certs,
            registry_tls_configs,
            https_proxy,
            http_proxy,
            no_proxy,
        } = options;
        let permits = permits.max(1);
        let semaphore = permits.pipe(Semaphore::new);
        let client = Arc::new(build_client(
            strict_ssl,
            request_timeout_ms,
            &ca_certs,
            None,
            https_proxy.as_deref(),
            http_proxy.as_deref(),
            no_proxy.as_deref(),
        ));
        ThrottledClient {
            semaphore,
            client,
            permits,
            request_timeout_ms,
            strict_ssl,
            ca_certs,
            registry_tls_configs,
            https_proxy,
            http_proxy,
            no_proxy,
            registry_clients: Mutex::new(HashMap::new()),
        }
    }

    /// Configured request concurrency limit.
    pub fn concurrency_limit(&self) -> usize {
        self.permits
    }

    /// Configured request timeout in milliseconds.
    pub fn request_timeout_ms(&self) -> Option<u64> {
        self.request_timeout_ms
    }

    /// Whether TLS certificate validation is enforced.
    pub fn strict_ssl(&self) -> bool {
        self.strict_ssl
    }

    /// Number of configured global CA certificates.
    pub fn ca_cert_count(&self) -> usize {
        self.ca_certs.len()
    }

    /// Number of configured per-registry TLS entries.
    pub fn registry_tls_config_count(&self) -> usize {
        self.registry_tls_configs.len()
    }

    pub fn https_proxy(&self) -> Option<&str> {
        self.https_proxy.as_deref()
    }

    pub fn http_proxy(&self) -> Option<&str> {
        self.http_proxy.as_deref()
    }

    pub fn no_proxy(&self) -> Option<&str> {
        self.no_proxy.as_deref()
    }

    fn client_for_url(&self, url: &str) -> Arc<Client> {
        let Some(key) = matching_ssl_config_key(&self.registry_tls_configs, url) else {
            return self.client.clone();
        };
        if let Some(existing) =
            self.registry_clients.lock().expect("registry client cache mutex").get(&key).cloned()
        {
            return existing;
        }
        let registry_config = self.registry_tls_configs.get(&key);
        let built = Arc::new(build_client(
            self.strict_ssl,
            self.request_timeout_ms,
            &self.ca_certs,
            registry_config,
            self.https_proxy.as_deref(),
            self.http_proxy.as_deref(),
            self.no_proxy.as_deref(),
        ));
        self.registry_clients
            .lock()
            .expect("registry client cache mutex")
            .insert(key, built.clone());
        built
    }
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_from_cpu_count()
    }
}

fn build_client(
    strict_ssl: bool,
    request_timeout_ms: Option<u64>,
    ca_certs: &[String],
    registry_config: Option<&RegistryTlsConfig>,
    https_proxy: Option<&str>,
    http_proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> Client {
    let mut builder = Client::builder().danger_accept_invalid_certs(!strict_ssl).no_proxy();
    if let Some(ms) = request_timeout_ms {
        builder = builder.timeout(Duration::from_millis(ms));
    }
    if let Some(no_proxy) = no_proxy {
        let parsed = NoProxy::from_string(no_proxy);
        if let Some(proxy) = https_proxy.and_then(|value| Proxy::https(value).ok()) {
            builder = builder.proxy(proxy.no_proxy(parsed.clone()));
        }
        if let Some(proxy) = http_proxy.and_then(|value| Proxy::http(value).ok()) {
            builder = builder.proxy(proxy.no_proxy(parsed));
        }
    } else {
        if let Some(proxy) = https_proxy.and_then(|value| Proxy::https(value).ok()) {
            builder = builder.proxy(proxy);
        }
        if let Some(proxy) = http_proxy.and_then(|value| Proxy::http(value).ok()) {
            builder = builder.proxy(proxy);
        }
    }
    for cert in ca_certs {
        if let Ok(cert) = Certificate::from_pem(cert.as_bytes()) {
            builder = builder.add_root_certificate(cert);
        }
    }
    if let Some(ca) = registry_config.and_then(|config| config.ca.as_ref())
        && let Ok(cert) = Certificate::from_pem(ca.as_bytes())
    {
        builder = builder.add_root_certificate(cert);
    }
    if let Some(identity) =
        registry_config.and_then(|config| match (config.cert.as_ref(), config.key.as_ref()) {
            (Some(cert), Some(key)) => {
                Identity::from_pkcs8_pem(cert.as_bytes(), key.as_bytes()).ok()
            }
            _ => None,
        })
    {
        builder = builder.identity(identity);
    }
    builder.build().expect("build reqwest client")
}

fn matching_ssl_config_key(
    configs: &HashMap<String, RegistryTlsConfig>,
    url: &str,
) -> Option<String> {
    let try_match = |candidate_url: &str| -> Option<String> {
        let nerfed = nerf_url(candidate_url)?;
        let parts = nerfed.split('/').collect::<Vec<_>>();
        for idx in (3..=parts.len()).rev() {
            let key = format!("{}/", parts[..idx].join("/"));
            if configs.contains_key(&key) {
                return Some(key);
            }
        }
        None
    };
    try_match(url)
        .or_else(|| remove_default_port(url).and_then(|without_port| try_match(&without_port)))
}

fn nerf_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let mut out = String::from("//");
    out.push_str(host);
    if let Some(port) = parsed.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path.is_empty() || path == "/" {
        out.push('/');
    } else {
        out.push_str(path);
        if !path.ends_with('/') {
            out.push('/');
        }
    }
    Some(out)
}

fn remove_default_port(url: &str) -> Option<String> {
    let mut parsed = Url::parse(url).ok()?;
    let has_default_port =
        matches!((parsed.scheme(), parsed.port()), ("https", Some(443)) | ("http", Some(80)));
    if !has_default_port {
        return None;
    }
    if parsed.set_port(None).is_err() {
        return None;
    }
    Some(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_ssl_config_key_prefers_longest_prefix() {
        let configs = HashMap::from([
            ("//registry.example.com/".to_string(), RegistryTlsConfig::default()),
            ("//registry.example.com/custom/".to_string(), RegistryTlsConfig::default()),
        ]);
        assert_eq!(
            matching_ssl_config_key(&configs, "https://registry.example.com/custom/pkg"),
            Some("//registry.example.com/custom/".to_string())
        );
    }

    #[test]
    fn matching_ssl_config_key_matches_default_https_port_entries() {
        let configs =
            HashMap::from([("//registry.example.com/".to_string(), RegistryTlsConfig::default())]);

        assert_eq!(
            matching_ssl_config_key(&configs, "https://registry.example.com:443/pkg"),
            Some("//registry.example.com/".to_string())
        );
    }
}
