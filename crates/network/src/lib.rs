use pipe_trait::Pipe;
use reqwest::Client;
use std::{future::IntoFuture, time::Duration};
use tokio::sync::Semaphore;

/// Wrapper around [`Client`] with concurrent request limit enforced by the [`Semaphore`] mechanism.
#[derive(Debug)]
pub struct ThrottledClient {
    semaphore: Semaphore,
    client: Client,
    permits: usize,
    request_timeout_ms: Option<u64>,
    strict_ssl: bool,
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

    /// Construct a new throttled client based on the number of CPUs.
    /// If the number of CPUs is greater than 16, the number of permits will be equal to the number of CPUs.
    /// Otherwise, the number of permits will be 16.
    pub fn new_from_cpu_count() -> Self {
        const MIN_PERMITS: usize = 16;
        Self::new_with_limit(num_cpus::get().max(MIN_PERMITS))
    }

    /// Construct a new throttled client with a fixed permit count.
    pub fn new_with_limit(permits: usize) -> Self {
        Self::new_with_options(permits, None, true)
    }

    /// Construct a new throttled client with a fixed permit count and optional request timeout.
    pub fn new_with_options(
        permits: usize,
        request_timeout_ms: Option<u64>,
        strict_ssl: bool,
    ) -> Self {
        let permits = permits.max(1);
        let semaphore = permits.pipe(Semaphore::new);
        let mut builder = Client::builder().danger_accept_invalid_certs(!strict_ssl);
        if let Some(ms) = request_timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        }
        let client = builder.build().expect("build reqwest client");
        ThrottledClient { semaphore, client, permits, request_timeout_ms, strict_ssl }
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
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_from_cpu_count()
    }
}
