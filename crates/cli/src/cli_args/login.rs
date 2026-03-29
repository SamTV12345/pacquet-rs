use clap::Args;
use miette::{Context, IntoDiagnostic};
use pacquet_npmrc::Npmrc;
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    thread,
    time::Duration,
};

/// Log in to a registry.
#[derive(Debug, Args, Default)]
pub struct LoginArgs {
    /// Base URL of the registry.
    #[arg(long)]
    registry: Option<String>,

    /// Scope to associate the auth token with.
    #[arg(long)]
    scope: Option<String>,

    /// Auth type: "web" (default, browser-based) or "legacy" (username/password prompt).
    #[arg(long = "auth-type", default_value = "web")]
    auth_type: String,
}

impl LoginArgs {
    pub fn run(self, npmrc: &Npmrc) -> miette::Result<()> {
        let registry = self.registry.as_deref().unwrap_or(&npmrc.registry);
        let registry = registry.trim_end_matches('/');

        println!("Log in on {registry}/");

        match self.auth_type.as_str() {
            "web" => self.web_login(registry),
            "legacy" => self.legacy_login(registry),
            other => miette::bail!("Unknown auth type: {other}. Use \"web\" or \"legacy\"."),
        }
    }

    /// Browser-based login (like npm's default flow).
    /// Creates a login session, user opens URL in browser, we poll until done.
    fn web_login(&self, registry: &str) -> miette::Result<()> {
        let session_url = format!("{registry}/-/v1/login");
        let session_id = uuid_v4();
        let user_agent = format!("pacquet/0.2.1 node/{}", node_version_hint());
        let output = Command::new("curl")
            .args([
                "-s",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-H",
                &format!("User-Agent: {user_agent}"),
                "-H",
                "npm-auth-type: web",
                "-H",
                "npm-command: login",
                "-H",
                &format!("npm-session: {session_id}"),
                "-d",
                "{}",
                &session_url,
            ])
            .output()
            .into_diagnostic()
            .wrap_err("create login session")?;

        let body = String::from_utf8_lossy(&output.stdout);
        let response: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

        let login_url = response.get("loginUrl").and_then(Value::as_str);
        let done_url = response.get("doneUrl").and_then(Value::as_str);

        match (login_url, done_url) {
            (Some(login_url), Some(done_url)) => {
                println!("Login at:");
                println!("{login_url}");
                println!("Press ENTER to open in the browser...");

                let mut input = String::new();
                let _ = io::stdin().read_line(&mut input);

                open_browser(login_url);

                println!("Waiting for authentication...");
                let token = poll_for_token(done_url)?;
                save_token(registry, &token)?;
                println!("Logged in on {registry}/");
                Ok(())
            }
            _ => {
                println!("Registry does not support web login, falling back to legacy auth.");
                self.legacy_login(registry)
            }
        }
    }

    /// Legacy username/password login via CouchDB user API.
    fn legacy_login(&self, registry: &str) -> miette::Result<()> {
        let username = prompt("Username: ")?;
        let password = prompt("Password: ")?;

        let url = format!("{registry}/-/user/org.couchdb.user:{username}");
        let payload = serde_json::json!({
            "_id": format!("org.couchdb.user:{username}"),
            "name": username,
            "password": password,
            "type": "user",
        });

        let output = Command::new("curl")
            .args([
                "-s",
                "-X",
                "PUT",
                "-H",
                "Content-Type: application/json",
                "-d",
                &serde_json::to_string(&payload).unwrap_or_default(),
                &url,
            ])
            .output()
            .into_diagnostic()
            .wrap_err("authenticate with registry")?;

        let body = String::from_utf8_lossy(&output.stdout);
        let response: Value =
            serde_json::from_str(&body).into_diagnostic().wrap_err("parse auth response")?;

        let token = response.get("token").and_then(Value::as_str).ok_or_else(|| {
            let error = response.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            miette::miette!("Login failed: {error}")
        })?;

        save_token(registry, token)?;
        println!("Logged in as {username} on {registry}/");
        Ok(())
    }
}

fn poll_for_token(done_url: &str) -> miette::Result<String> {
    let max_attempts = 120;
    for _ in 0..max_attempts {
        // Use -w to capture HTTP status code, -D - to capture headers
        let output = Command::new("curl")
            .args(["-s", "-w", "\n%{http_code}", "-H", "Accept: application/json", done_url])
            .output()
            .into_diagnostic()
            .wrap_err("poll login status")?;

        let raw = String::from_utf8_lossy(&output.stdout);
        let (body, status_str) = raw.rsplit_once('\n').unwrap_or((&raw, "0"));
        let status: u16 = status_str.trim().parse().unwrap_or(0);

        match status {
            200 => {
                // Login complete — extract token
                if let Ok(response) = serde_json::from_str::<Value>(body)
                    && let Some(token) = response.get("token").and_then(Value::as_str)
                {
                    return Ok(token.to_string());
                }
                miette::bail!("Login response did not contain a token");
            }
            202 => {
                // Not yet — respect retry-after or default 1s
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            _ => {
                // Check for error messages in body
                if let Ok(response) = serde_json::from_str::<Value>(body)
                    && let Some(error) = response.get("error").and_then(Value::as_str)
                {
                    if error == "retry" {
                        thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    miette::bail!("Login failed: {error}");
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    miette::bail!("Login timed out. Please try again.")
}

fn save_token(registry: &str, token: &str) -> miette::Result<()> {
    let npmrc_path =
        home::home_dir().map(|h| h.join(".npmrc")).unwrap_or_else(|| PathBuf::from(".npmrc"));
    let registry_key = registry.trim_start_matches("https:").trim_start_matches("http:");
    let line = format!("{registry_key}:_authToken={token}\n");

    let mut content = fs::read_to_string(&npmrc_path).unwrap_or_default();
    let prefix = format!("{registry_key}:_authToken=");
    let lines: Vec<&str> = content.lines().filter(|l| !l.starts_with(&prefix)).collect();
    content = lines.join("\n");
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&line);

    fs::write(&npmrc_path, content)
        .into_diagnostic()
        .wrap_err_with(|| format!("write {}", npmrc_path.display()))
}

fn open_browser(url: &str) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/c", "start", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };
}

fn prompt(message: &str) -> miette::Result<String> {
    eprint!("{message}");
    io::stderr().flush().into_diagnostic()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input).into_diagnostic().wrap_err("read input")?;
    Ok(input.trim().to_string())
}

/// Generate a simple v4-style UUID without pulling in a full uuid crate.
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    // XOR with process ID for extra entropy
    let pid = std::process::id() as u128;
    let val = seed ^ (pid << 64);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (val >> 96) as u32,
        (val >> 80) as u16,
        (val >> 64) as u16 & 0x0FFF,
        ((val >> 48) as u16 & 0x3FFF) | 0x8000,
        val as u64 & 0xFFFF_FFFF_FFFF,
    )
}

/// Best-effort Node.js version hint for User-Agent (doesn't need to be real).
fn node_version_hint() -> &'static str {
    "v20.0.0"
}
