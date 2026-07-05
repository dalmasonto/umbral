//! umbral-analytics — product-analytics event capture for umbral.
//!
//! Analytics instrumentation, the umbral way: declare the plugin, call
//! [`capture`] / [`identify`] from any handler or service, and analytics
//! failures never break a request. The PostHog backend is fire-and-forget;
//! every send is spawned on a background task so the caller returns
//! immediately.
//!
//! ## Quick start
//!
//! ```ignore
//! // Wire in main
//! App::builder()
//!     .plugin(
//!         AnalyticsPlugin::new("phc_your_api_key")
//!             .capture_requests(), // optional: auto pageview per request
//!     )
//!     .build()
//!     .await?;
//!
//! // In a handler
//! use umbral_analytics::{capture, identify};
//!
//! async fn signup(/* ... */) -> impl IntoResponse {
//!     identify("user_42", serde_json::json!({ "$set": { "email": "a@b.com" } })).await;
//!     capture("user_42", "signup", serde_json::json!({ "plan": "pro" })).await;
//!     StatusCode::CREATED
//! }
//! ```
//!
//! ## Settings keys
//!
//! Read from `UMBRAL_POSTHOG_API_KEY` / `UMBRAL_POSTHOG_HOST` env vars or
//! `umbral.toml` extra keys `posthog_api_key` / `posthog_host`. Builder
//! overrides win over environment.
//!
//! - `posthog_api_key` / `UMBRAL_POSTHOG_API_KEY`. Your project API key.
//!   When absent the plugin is a **no-op**: captures are dropped with a
//!   one-time warning. Never panics, never blocks.
//! - `posthog_host` / `UMBRAL_POSTHOG_HOST`. Ingest host
//!   (default `https://us.i.posthog.com`).
//!
//! ## Surface
//!
//! - [`AnalyticsPlugin`]. The plugin; registers the ambient client at boot.
//! - [`capture`]. Fire-and-forget event capture (free function, ambient).
//! - [`identify`]. Fire-and-forget `$identify` person update (free function, ambient).
//! - [`AnalyticsClient`]. The typed PostHog client. Public so callers can
//!   build one directly for testing or send with an explicit client.

use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tracing::{debug, warn};
use umbral::plugin::PluginError;
use umbral::prelude::*;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default PostHog ingest host (US region).
pub const DEFAULT_POSTHOG_HOST: &str = "https://us.i.posthog.com";

/// HTTP request timeout for PostHog API calls (seconds).
const HTTP_TIMEOUT_SECS: u64 = 10;

/// TCP + TLS connect timeout for PostHog API calls (seconds).
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 5;

// ── Ambient client ────────────────────────────────────────────────────────────

/// Process-wide analytics client, installed once during `on_ready`.
/// `capture` / `identify` read it ambiently. When absent, both are no-ops.
static AMBIENT_CLIENT: OnceLock<AnalyticsClient> = OnceLock::new();

/// Return the ambient client, or `None` if the plugin isn't registered / no
/// API key was configured.
pub fn ambient_client() -> Option<&'static AnalyticsClient> {
    AMBIENT_CLIENT.get()
}

// ── HTTP client ───────────────────────────────────────────────────────────────

/// Process-wide shared reqwest client. Built once; cloning is `O(1)` (Arc).
/// Mirrors the `umbral-oauth` `http_client()` pattern exactly.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Ceiling on concurrent in-flight analytics sends (audit_2
/// plugin-observability #5). Each `capture_fire_and_forget` spawned an outbound
/// HTTPS POST with no bound, so a request burst at scale fanned out unbounded
/// tasks/connections — resource amplification / self-DoS. A permit is acquired
/// BEFORE spawning; when all are in use the event is dropped (analytics is
/// best-effort) rather than piling up.
const MAX_CONCURRENT_ANALYTICS_SENDS: usize = 64;

static SEND_SLOTS: OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn send_slots() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    SEND_SLOTS.get_or_init(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_ANALYTICS_SENDS))
    })
}

/// Returns a clone of the process-wide shared HTTP client.
///
/// Configured with:
/// - `timeout(10 s)` — total request duration.
/// - `connect_timeout(5 s)` — TCP + TLS handshake budget.
pub fn http_client() -> reqwest::Client {
    HTTP_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
                .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
                .build()
                .expect("failed to build the shared analytics HTTP client")
        })
        .clone()
}

// ── AnalyticsClient ───────────────────────────────────────────────────────────

/// A configured PostHog client. Owns an API key + host; reuses the
/// process-wide [`http_client`] connection pool.
///
/// Normally installed as the ambient client via [`AnalyticsPlugin`].
/// Build one explicitly for testing or for callers that prefer explicit
/// dependency injection over the ambient pattern.
#[derive(Clone, Debug)]
pub struct AnalyticsClient {
    api_key: String,
    host: String,
    /// Request-path prefixes NOT to auto-capture as `$pageview` (audit_2
    /// plugin-observability #4). Paths under these prefixes carry secrets/PII
    /// (`/reset-password/<token>`, `/users/<email>/…`) that must not leave the
    /// trust boundary for a third-party analytics host.
    exclude_prefixes: Vec<String>,
}

impl AnalyticsClient {
    /// Build a client with explicit API key and host.
    pub fn new(api_key: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            host: host.into(),
            exclude_prefixes: Vec::new(),
        }
    }

    /// Set the pageview-exclusion prefixes (see [`Self::exclude_prefixes`]).
    pub fn with_exclude_prefixes(mut self, prefixes: Vec<String>) -> Self {
        self.exclude_prefixes = prefixes;
        self
    }

    /// Whether an auto-`$pageview` should be captured for `path`. `false` when
    /// the path starts with any configured exclusion prefix, so sensitive
    /// routes never ship their path to the analytics host.
    pub fn should_capture_path(&self, path: &str) -> bool {
        !self
            .exclude_prefixes
            .iter()
            .any(|p| path.starts_with(p.as_str()))
    }

    /// Build the PostHog `/capture/` JSON payload.
    ///
    /// Shape: `{ "api_key", "event", "distinct_id", "properties", "timestamp" }`.
    /// The timestamp is RFC 3339 (ISO 8601) UTC.
    pub fn build_payload(&self, distinct_id: &str, event: &str, properties: Value) -> Value {
        json!({
            "api_key": self.api_key,
            "event": event,
            "distinct_id": distinct_id,
            "properties": properties,
            "timestamp": Utc::now().to_rfc3339(),
        })
    }

    /// Send one event to PostHog `/capture/`. Fire-and-forget: spawns the
    /// HTTP send in a background task; the caller returns immediately.
    /// Analytics send errors are logged at `warn` / `debug` level and
    /// never propagated.
    pub fn capture_fire_and_forget(
        &self,
        distinct_id: impl Into<String>,
        event: impl Into<String>,
        properties: Value,
    ) {
        // audit_2 #5: acquire a send slot BEFORE spawning so a burst can't fan
        // out unbounded outbound tasks. At capacity we drop the event (analytics
        // is best-effort) instead of queueing without bound. The permit is moved
        // into the task and released when the send finishes.
        let permit = match send_slots().clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                debug!("analytics: concurrent-send limit reached; dropping event");
                return;
            }
        };
        let payload = self.build_payload(&distinct_id.into(), &event.into(), properties);
        let url = format!("{}/capture/", self.host.trim_end_matches('/'));
        let client = http_client();

        tokio::spawn(async move {
            let _permit = permit; // released when the send completes
            match client.post(&url).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!(url = %url, "analytics: event captured");
                }
                Ok(resp) => {
                    warn!(
                        url = %url,
                        status = %resp.status(),
                        "analytics: PostHog returned non-success status (swallowed)"
                    );
                }
                Err(e) => {
                    warn!(
                        url = %url,
                        error = %e,
                        "analytics: PostHog send failed (swallowed)"
                    );
                }
            }
        });
    }
}

// ── Free functions (ambient API) ──────────────────────────────────────────────

/// Fire-and-forget event capture. Sends `event` with `properties` attributed
/// to `distinct_id` to PostHog. The HTTP send happens in a background task;
/// this function returns immediately and analytics failures never affect the
/// caller.
///
/// When no API key is configured (no ambient client), this is a clean no-op.
///
/// # Example
///
/// ```ignore
/// capture("user_42", "purchase", serde_json::json!({ "amount_cents": 999 })).await;
/// ```
pub async fn capture(distinct_id: impl Into<String>, event: impl Into<String>, properties: Value) {
    if let Some(client) = ambient_client() {
        client.capture_fire_and_forget(distinct_id, event, properties);
    } else {
        debug!("analytics: capture called with no client installed (no-op)");
    }
}

/// Fire-and-forget person identification. Sends a PostHog `$identify` event
/// with person properties under `$set`. Use this to associate a `distinct_id`
/// with user properties (name, email, plan, etc.).
///
/// When no API key is configured, this is a clean no-op.
///
/// # Example
///
/// ```ignore
/// identify("user_42", serde_json::json!({ "$set": { "email": "a@b.com", "plan": "pro" } })).await;
/// ```
pub async fn identify(distinct_id: impl Into<String>, properties: Value) {
    if let Some(client) = ambient_client() {
        client.capture_fire_and_forget(distinct_id, "$identify", properties);
    } else {
        debug!("analytics: identify called with no client installed (no-op)");
    }
}

// ── Request middleware ────────────────────────────────────────────────────────

/// Axum `from_fn` middleware that fires a `$pageview` event for every
/// incoming HTTP request. Installed via [`AnalyticsPlugin::capture_requests`].
///
/// - `distinct_id`: `"anonymous"` (future: resolved from session/identity).
/// - Event: `"$pageview"`.
/// - Properties: `{ "path", "method", "status" }`.
///
/// The status is captured after the inner handler responds.
async fn pageview_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let method = req.method().to_string();

    let response = next.run(req).await;
    let status = response.status().as_u16();

    // Fire-and-forget: send after the response is composed so the status
    // code is available, but spawn the HTTP call so we never block the
    // response stream returning to the client.
    if let Some(client) = ambient_client() {
        // Don't ship the path of a sensitive route (reset tokens, per-user
        // paths) to the third-party analytics host (audit_2 #4).
        if client.should_capture_path(&path) {
            let props = json!({
                "path": path,
                "method": method,
                "status": status,
                "$current_url": path,
            });
            client.capture_fire_and_forget("anonymous", "$pageview", props);
        }
    }

    response
}

// ── AnalyticsPlugin ───────────────────────────────────────────────────────────

/// The analytics plugin. Carries no models, no persistent routes — just an
/// [`AnalyticsClient`] it installs as the ambient handle at boot so
/// [`capture`] / [`identify`] work anywhere in the process.
///
/// ## Registration
///
/// ```ignore
/// App::builder()
///     .plugin(AnalyticsPlugin::new("phc_your_api_key"))
///     .build()
///     .await?;
/// ```
///
/// ## Opt-in per-request pageview capture
///
/// ```ignore
/// AnalyticsPlugin::new("phc_your_api_key")
///     .capture_requests()   // fires a $pageview event on every request
/// ```
///
/// ## No-op when unconfigured
///
/// When the API key is absent (neither builder arg nor env var), the plugin
/// registers but the ambient client is not installed. Both [`capture`] and
/// [`identify`] are silent no-ops. A one-time `warn!` fires at boot so the
/// operator can diagnose misconfiguration without a runtime panic.
pub struct AnalyticsPlugin {
    /// API key supplied via builder. Beats the env var.
    api_key: Option<String>,
    /// PostHog host. Defaults to [`DEFAULT_POSTHOG_HOST`].
    host: String,
    /// When true, mount the [`pageview_middleware`] in `wrap_router`.
    auto_capture_requests: bool,
    /// Request-path prefixes excluded from auto pageview capture (#4).
    exclude_prefixes: Vec<String>,
}

impl AnalyticsPlugin {
    /// Build the plugin with an explicit PostHog project API key.
    ///
    /// The key wins over `UMBRAL_POSTHOG_API_KEY` / `posthog_api_key` in
    /// settings. Use this for apps that keep secrets in code (not recommended
    /// for production; prefer the env var and call [`AnalyticsPlugin::from_env`]).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: Some(api_key.into()),
            host: DEFAULT_POSTHOG_HOST.to_string(),
            auto_capture_requests: false,
            exclude_prefixes: Vec::new(),
        }
    }

    /// Build the plugin reading configuration exclusively from environment
    /// variables / `umbral.toml` settings. Equivalent to
    /// `AnalyticsPlugin::default()` with no builder overrides.
    pub fn from_env() -> Self {
        Self::default()
    }

    /// Override the PostHog ingest host. Default: `https://us.i.posthog.com`.
    /// Override for EU region (`https://eu.i.posthog.com`) or a self-hosted
    /// instance.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Opt in to automatic per-request `$pageview` capture. Mounts a
    /// `from_fn` middleware that fires one event per request (with path,
    /// method, and status code in properties) without any handler
    /// changes. Default OFF.
    pub fn capture_requests(mut self) -> Self {
        self.auto_capture_requests = true;
        self
    }

    /// Exclude a request-path prefix from auto `$pageview` capture so its path
    /// never ships to the analytics host (audit_2 plugin-observability #4).
    /// Add every route whose path can carry a secret or PII — password-reset
    /// and email-verification links, per-user resource paths, signed URLs, etc.
    /// Call more than once to exclude several prefixes.
    pub fn exclude_path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.exclude_prefixes.push(prefix.into());
        self
    }

    /// Resolve the API key: builder field beats env var beats settings.
    fn resolve_api_key(&self) -> Option<String> {
        // Builder arg wins.
        if let Some(ref key) = self.api_key {
            if !key.trim().is_empty() {
                return Some(key.clone());
            }
        }

        // Environment variable next.
        if let Ok(val) = std::env::var("UMBRAL_POSTHOG_API_KEY") {
            if !val.trim().is_empty() {
                return Some(val);
            }
        }

        // umbral.toml extra key last.
        if let Ok(settings) = umbral::Settings::from_env() {
            if let Some(v) = settings.extra.get("posthog_api_key") {
                if let Some(key) = v.as_str() {
                    if !key.trim().is_empty() {
                        return Some(key.to_string());
                    }
                }
            }
        }

        None
    }

    /// Resolve the PostHog host: builder field beats env var beats settings,
    /// with [`DEFAULT_POSTHOG_HOST`] as the final fallback.
    fn resolve_host(&self) -> String {
        // Builder field (already defaulted in ::new / ::default).
        if self.host != DEFAULT_POSTHOG_HOST {
            return self.host.clone();
        }

        // Environment variable.
        if let Ok(val) = std::env::var("UMBRAL_POSTHOG_HOST") {
            if !val.trim().is_empty() {
                return val;
            }
        }

        // umbral.toml extra key.
        if let Ok(settings) = umbral::Settings::from_env() {
            if let Some(v) = settings.extra.get("posthog_host") {
                if let Some(h) = v.as_str() {
                    if !h.trim().is_empty() {
                        return h.to_string();
                    }
                }
            }
        }

        DEFAULT_POSTHOG_HOST.to_string()
    }
}

impl Default for AnalyticsPlugin {
    fn default() -> Self {
        Self {
            api_key: None,
            host: DEFAULT_POSTHOG_HOST.to_string(),
            auto_capture_requests: false,
            exclude_prefixes: Vec::new(),
        }
    }
}

impl Plugin for AnalyticsPlugin {
    fn name(&self) -> &'static str {
        "analytics"
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        match self.resolve_api_key() {
            Some(key) => {
                let host = self.resolve_host();
                let client = AnalyticsClient::new(key, host.clone())
                    .with_exclude_prefixes(self.exclude_prefixes.clone());
                if AMBIENT_CLIENT.set(client).is_err() {
                    warn!(
                        "AnalyticsPlugin: an ambient analytics client was already installed; \
                         ignoring this registration."
                    );
                } else {
                    tracing::info!(host = %host, "analytics: PostHog client installed");
                }
            }
            None => {
                warn!(
                    "AnalyticsPlugin registered with no PostHog API key. Set \
                     UMBRAL_POSTHOG_API_KEY or pass an explicit key via \
                     AnalyticsPlugin::new(key). Capture calls will be silent no-ops."
                );
            }
        }
        Ok(())
    }

    fn wrap_router(&self, router: Router) -> Router {
        if self.auto_capture_requests {
            router.layer(axum::middleware::from_fn(pageview_middleware))
        } else {
            router
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AnalyticsClient;

    // audit_2 plugin-observability #5: outbound sends are bounded so a burst
    // can't fan out unbounded tasks. The semaphore starts sized to the ceiling,
    // and once exhausted, further acquisitions fail (→ the event is dropped).
    #[test]
    fn outbound_sends_are_concurrency_bounded() {
        let sem = super::send_slots();
        assert_eq!(
            sem.available_permits(),
            super::MAX_CONCURRENT_ANALYTICS_SENDS
        );
        // Exhaust a private clone's worth to prove the drop path: hold every
        // permit, then the next acquire fails (what `capture` treats as "drop").
        let mut held = Vec::new();
        for _ in 0..super::MAX_CONCURRENT_ANALYTICS_SENDS {
            held.push(sem.clone().try_acquire_owned().expect("permit"));
        }
        assert!(
            sem.clone().try_acquire_owned().is_err(),
            "at capacity, a further send must be refused (dropped)"
        );
        // Permits released here (held dropped) so sibling tests aren't starved.
    }

    #[test]
    fn excluded_prefixes_are_not_captured() {
        let client = AnalyticsClient::new("k", "https://h")
            .with_exclude_prefixes(vec!["/reset-password".to_string(), "/verify".to_string()]);
        // Sensitive paths (incl. a token segment) are excluded.
        assert!(!client.should_capture_path("/reset-password/abc123token"));
        assert!(!client.should_capture_path("/verify/xyz"));
        // Ordinary paths are still captured.
        assert!(client.should_capture_path("/"));
        assert!(client.should_capture_path("/pricing"));
        // With no exclusions everything is captured.
        let open = AnalyticsClient::new("k", "https://h");
        assert!(open.should_capture_path("/reset-password/abc"));
    }
}
