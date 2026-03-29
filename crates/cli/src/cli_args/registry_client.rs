//! Thin blocking HTTP helper for registry commands.
//!
//! Uses `reqwest::blocking` so the caller does not need an async runtime.
//! Auth headers are resolved from [`Npmrc::auth_header_for_url`].

use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;

const PACQUET_USER_AGENT: &str = "pacquet/0.2.1";

/// A thin wrapper that attaches auth + user-agent headers derived from the
/// active [`Npmrc`] to every outgoing request.
pub struct RegistryClient<'a> {
    pub npmrc: &'a Npmrc,
    client: Client,
}

impl<'a> RegistryClient<'a> {
    pub fn new(npmrc: &'a Npmrc) -> Self {
        let client = Client::builder()
            .user_agent(PACQUET_USER_AGENT)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { npmrc, client }
    }

    /// Build common headers for a request to `url`.
    fn headers(&self, url: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(PACQUET_USER_AGENT));
        if let Some(auth) = self.npmrc.auth_header_for_url(url)
            && let Ok(val) = HeaderValue::from_str(&auth) {
                headers.insert(AUTHORIZATION, val);
            }
        headers
    }

    /// Require auth or bail.
    pub fn require_auth(&self, url: &str) -> miette::Result<String> {
        self.npmrc
            .auth_header_for_url(url)
            .ok_or_else(|| miette::miette!("Not logged in. Run `pacquet login` first."))
    }

    // ── convenience verbs ──────────────────────────────────────────

    pub fn get(&self, url: &str) -> miette::Result<Response> {
        self.client
            .get(url)
            .headers(self.headers(url))
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("GET {url}"))
    }

    pub fn get_json(&self, url: &str) -> miette::Result<Value> {
        let resp = self.get(url)?;
        let status = resp.status();
        let body = resp.text().into_diagnostic().wrap_err("read response body")?;
        let value: Value =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse JSON response")?;
        if let Some(err) = value.get("error").and_then(Value::as_str) {
            miette::bail!("{err} (HTTP {status})");
        }
        Ok(value)
    }

    /// POST JSON without requiring authentication.
    /// Useful for anonymous endpoints like the bulk advisory API.
    pub fn post_json_anonymous(&self, url: &str, body: &Value) -> miette::Result<Response> {
        self.client
            .post(url)
            .headers(self.headers(url))
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("POST {url}"))
    }

    pub fn post_json(&self, url: &str, body: &Value) -> miette::Result<Value> {
        let auth = self.require_auth(url)?;
        let resp = self
            .client
            .post(url)
            .headers(self.headers(url))
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("POST {url}"))?;
        resp.json::<Value>().into_diagnostic().wrap_err("parse response")
    }

    pub fn put_json(&self, url: &str, body: &Value) -> miette::Result<Response> {
        let auth = self.require_auth(url)?;
        self.client
            .put(url)
            .headers(self.headers(url))
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("PUT {url}"))
    }

    pub fn put_string(&self, url: &str, body: &str) -> miette::Result<Response> {
        let auth = self.require_auth(url)?;
        self.client
            .put(url)
            .headers(self.headers(url))
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("PUT {url}"))
    }

    pub fn delete(&self, url: &str) -> miette::Result<Response> {
        let auth = self.require_auth(url)?;
        self.client
            .delete(url)
            .headers(self.headers(url))
            .header(AUTHORIZATION, &auth)
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("DELETE {url}"))
    }

    pub fn patch_json(&self, url: &str, body: &Value) -> miette::Result<Response> {
        let auth = self.require_auth(url)?;
        self.client
            .patch(url)
            .headers(self.headers(url))
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .into_diagnostic()
            .wrap_err_with(|| format!("PATCH {url}"))
    }

    /// Registry base URL for a given package name (respects scoped registries).
    pub fn registry_url(&self, package_name: &str) -> String {
        self.npmrc.registry_for_package_name(package_name).trim_end_matches('/').to_string()
    }

    /// Default registry base URL.
    pub fn default_registry(&self) -> String {
        self.npmrc.registry.trim_end_matches('/').to_string()
    }
}
