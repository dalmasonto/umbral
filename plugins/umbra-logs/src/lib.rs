//! umbra-logs — Django-request-style request logging for umbra.
//!
//! Django's `django.request` logging, the umbra way: register the plugin and
//! every HTTP request is recorded to a [`RequestLog`] row, browsable in the
//! admin. Capture is **fire-and-forget** — the response is returned to the
//! client immediately, and the DB insert happens on a background task. A
//! capture failure is logged at `warn` and never propagated to the request.
//!
//! ## Quick start
//!
//! ```ignore
//! use umbra::prelude::*;
//! use umbra_logs::LogsPlugin;
//! use umbra_admin::AdminPlugin;
//!
//! App::builder()
//!     .plugin(LogsPlugin::default())
//!     // Browse captured requests in the admin (read-only).
//!     .plugin(AdminPlugin::default().register(umbra_logs::admin_model()))
//!     .build()?;
//! ```
//!
//! ## Exclusions and sampling
//!
//! The logger skips its own/asset traffic and can cap volume:
//!
//! - `.exclude_prefix("/health")` — never log requests whose path starts with
//!   the prefix. The static mount (`/static`), `/admin/static`, `/health`, and
//!   `/favicon.ico` are excluded by default.
//! - `.sample_rate(0.1)` — log ~10% of requests (deterministic, not random:
//!   every Nth request by an atomic counter, so tests are reproducible).
//! - `.min_status(400)` — only log responses with status >= the floor (e.g.
//!   errors-only logging).
//!
//! ## Surface
//!
//! - [`RequestLog`]. The model; one row per captured request.
//! - [`LogsPlugin`]. The plugin: registers the model + mounts the capture
//!   layer.
//! - [`admin_model`]. A read-only [`umbra_admin::AdminModel`] for `RequestLog`
//!   (feature `admin`, on by default).
//! - [`flush`]. Test hook: await every in-flight capture task so a test can
//!   assert the row exists right after the request.

pub mod observability;
pub use observability::{init as init_observability, ObservabilityConfig, ObservabilityGuard};

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::warn;
use umbra::prelude::*;

#[cfg(feature = "admin")]
pub use umbra_admin::AdminModel;

// ── Model ───────────────────────────────────────────────────────────────────

/// One captured HTTP request. Lives in the `logs_requestlog` table (the
/// `#[umbra(table = ...)]` override namespaces it under the plugin, the same
/// way the built-in auth / sessions tables are named).
///
/// Created by the [`capture_layer`] on every (non-excluded, sampled) request.
/// Operators browse these in the admin; nothing writes them by hand.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "logs_requestlog")]
pub struct RequestLog {
    pub id: i64,
    /// HTTP method (`GET`, `POST`, …).
    pub method: String,
    /// Request path (no query string).
    pub path: String,
    /// Response status code.
    pub status: i32,
    /// Wall-clock handler duration in milliseconds.
    pub duration_ms: i64,
    /// Authenticated user id, best-effort. `None` for anonymous requests or
    /// when the identity can't be resolved.
    pub user_id: Option<i64>,
    /// Client IP, from `X-Forwarded-For` / `X-Real-IP` when present.
    pub ip: Option<String>,
    /// `User-Agent` header value, if present.
    pub user_agent: Option<String>,
    /// When the request was captured.
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

// ── Defaults ────────────────────────────────────────────────────────────────

/// Path prefixes excluded out of the box: static asset mounts, the health
/// endpoint, and the favicon. These never produce a `RequestLog` row so the
/// logger doesn't drown in its own/asset traffic.
const DEFAULT_EXCLUDE_PREFIXES: &[&str] = &["/static", "/admin/static", "/health", "/favicon.ico"];

// ── Test/flush hook ─────────────────────────────────────────────────────────

/// In-flight capture task handles. Each fire-and-forget insert pushes its
/// `JoinHandle` here so [`flush`] can await them in tests. Production never
/// calls `flush`, so handles accumulate only between flushes (bounded by
/// request volume between test assertions — fine for tests, untouched in
/// prod where `flush` is never called).
static PENDING: OnceLock<std::sync::Mutex<Vec<JoinHandle<()>>>> = OnceLock::new();

fn pending() -> &'static std::sync::Mutex<Vec<JoinHandle<()>>> {
    PENDING.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Test hook: await every capture task spawned so far, so a test can assert
/// the `RequestLog` row exists immediately after the request. Drains the
/// in-flight handle list.
///
/// Never call this on the request path in production — capture is meant to be
/// fire-and-forget. It exists purely so async capture is testable without
/// making the production insert block the response.
pub async fn flush() {
    let handles: Vec<JoinHandle<()>> = {
        let mut guard = pending().lock().expect("logs flush mutex");
        std::mem::take(&mut *guard)
    };
    for h in handles {
        let _ = h.await;
    }
}

// ── Deterministic sampler ───────────────────────────────────────────────────

/// Per-process request counter the sampler reads. Deterministic — every Nth
/// request is sampled rather than a random draw — so tests are reproducible.
static SAMPLE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Decide whether the request at `seq` (0-based) is sampled at `rate`.
///
/// `rate >= 1.0` keeps everything; `rate <= 0.0` drops everything. Otherwise
/// it keeps roughly one in `1/rate` requests on a fixed cadence: with
/// `rate = 0.25` the kept sequence is 0, 4, 8, … This is deterministic in
/// `seq`, which is what makes the sampling testable.
fn sampled(seq: u64, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    // Period = round(1/rate). Keep when seq is a multiple of the period.
    let period = (1.0 / rate).round().max(1.0) as u64;
    seq % period == 0
}

// ── LogsPlugin ──────────────────────────────────────────────────────────────

/// The request-logging plugin. Registers [`RequestLog`] (so `makemigrations`
/// emits the `logs_requestlog` table) and mounts the [`capture_layer`].
///
/// ```ignore
/// App::builder()
///     .plugin(
///         LogsPlugin::default()
///             .exclude_prefix("/metrics")
///             .sample_rate(0.5)   // log half the requests, deterministically
///             .min_status(400),   // …and only errors
///     )
///     .build()?;
/// ```
#[derive(Debug, Clone)]
pub struct LogsPlugin {
    /// Extra path prefixes to skip, on top of [`DEFAULT_EXCLUDE_PREFIXES`].
    exclude_prefixes: Vec<String>,
    /// Fraction of requests to log, in `[0.0, 1.0]`. Default `1.0` (all).
    sample_rate: f64,
    /// Minimum response status to log. Default `0` (all).
    min_status: i32,
}

impl Default for LogsPlugin {
    fn default() -> Self {
        Self {
            exclude_prefixes: Vec::new(),
            sample_rate: 1.0,
            min_status: 0,
        }
    }
}

impl LogsPlugin {
    /// Skip requests whose path starts with `prefix` (in addition to the
    /// built-in static/health/favicon exclusions). Call repeatedly to add
    /// several.
    pub fn exclude_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.exclude_prefixes.push(prefix.into());
        self
    }

    /// Sample a fraction of requests in `[0.0, 1.0]`. `1.0` (default) logs
    /// everything; `0.1` logs ~1 in 10 on a deterministic cadence. Values
    /// outside the range clamp to the nearest bound.
    pub fn sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = rate.clamp(0.0, 1.0);
        self
    }

    /// Only log responses with status >= `status`. Default `0` (log all).
    /// Set to `400` for errors-only logging, `500` for server-errors-only.
    pub fn min_status(mut self, status: i32) -> Self {
        self.min_status = status;
        self
    }

    /// The fully-resolved [`LogsConfig`] this plugin installs at boot —
    /// builder exclusions merged with the built-in defaults, plus the sample
    /// rate and status floor. Public so a test can assert the filtering rules
    /// via [`LogsConfig::should_capture`] without relying on the ambient
    /// process-global config.
    pub fn resolved_config(&self) -> LogsConfig {
        self.config()
    }

    /// Build the runtime config the [`capture_layer`] reads ambiently.
    fn config(&self) -> LogsConfig {
        let mut prefixes: Vec<String> =
            DEFAULT_EXCLUDE_PREFIXES.iter().map(|s| s.to_string()).collect();
        prefixes.extend(self.exclude_prefixes.iter().cloned());
        LogsConfig {
            exclude_prefixes: prefixes,
            sample_rate: self.sample_rate,
            min_status: self.min_status,
        }
    }
}

/// Resolved config the capture layer reads. Installed once in `on_ready` so
/// the `from_fn` layer (which can't carry `&self`) reaches it ambiently.
///
/// `pub` (and built via [`LogsPlugin::resolved_config`]) so tests can exercise
/// the filtering decision ([`LogsConfig::should_capture`]) directly without
/// depending on the process-global ambient config the layer reads at runtime.
#[derive(Debug, Clone)]
pub struct LogsConfig {
    exclude_prefixes: Vec<String>,
    sample_rate: f64,
    min_status: i32,
}

impl LogsConfig {
    /// Decide whether a request should be captured, given its `path`,
    /// response `status`, and the request's 0-based sequence number `seq`
    /// (used by the deterministic sampler). This is the exact predicate the
    /// [`capture_layer`] applies; exposed so the exclusion / `min_status` /
    /// sampling rules are unit-testable with explicit configs (the runtime
    /// config is process-global and sealed once at boot).
    pub fn should_capture(&self, path: &str, status: i32, seq: u64) -> bool {
        if self.exclude_prefixes.iter().any(|p| path.starts_with(p.as_str())) {
            return false;
        }
        if status < self.min_status {
            return false;
        }
        sampled(seq, self.sample_rate)
    }
}

static CONFIG: OnceLock<LogsConfig> = OnceLock::new();

fn config() -> &'static LogsConfig {
    // Fall back to the all-permissive default if the layer somehow runs
    // before `on_ready` installed the real config (e.g. a test that mounts
    // the layer directly). The default mirrors `LogsPlugin::default()`.
    static FALLBACK: OnceLock<LogsConfig> = OnceLock::new();
    CONFIG.get().unwrap_or_else(|| {
        FALLBACK.get_or_init(|| LogsConfig {
            exclude_prefixes: DEFAULT_EXCLUDE_PREFIXES.iter().map(|s| s.to_string()).collect(),
            sample_rate: 1.0,
            min_status: 0,
        })
    })
}

impl Plugin for LogsPlugin {
    fn name(&self) -> &'static str {
        "logs"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<RequestLog>()]
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), umbra::plugin::PluginError> {
        // Seal the config so `capture_layer` reads it without a `&self`
        // reference. First registration wins (a second plugin / a re-boot in
        // the same test process is a no-op, mirroring the sessions plugin).
        let _ = CONFIG.set(self.config());
        Ok(())
    }

    fn wrap_router(&self, router: Router) -> Router {
        router.layer(axum::middleware::from_fn(capture_layer))
    }
}

// ── Capture layer ───────────────────────────────────────────────────────────

/// Resolve the client IP best-effort from proxy headers. ConnectInfo isn't
/// wired in umbra's serve path (it uses `into_make_service()` without connect
/// info), so the peer address isn't available; the reverse-proxy headers are
/// the reliable source. Takes the first hop of `X-Forwarded-For`, else
/// `X-Real-IP`.
fn resolve_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            let ip = first.trim();
            if !ip.is_empty() {
                return Some(ip.to_string());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the authenticated user id best-effort. The session/identity
/// surface lives in optional sibling plugins this crate doesn't depend on, so
/// v1 reads an `X-Umbra-User-Id` extension/header only when present and parses
/// it as an i64; otherwise `None`. Wiring the real identity resolver is a
/// deliberate later step (it would pull umbra-auth/umbra-sessions into the
/// dep graph), kept out so a logs-only app stays slim.
fn resolve_user_id(headers: &axum::http::HeaderMap) -> Option<i64> {
    headers
        .get("x-umbra-user-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
}

/// axum `from_fn` middleware that records each request to [`RequestLog`].
///
/// Flow: stamp `Instant` at entry, run the handler, then (unless the path is
/// excluded, the request isn't sampled, or the status is below `min_status`)
/// fire-and-forget a background task that inserts the row via the ORM. The
/// response is returned to the client immediately; the insert never blocks it,
/// and a DB error is logged at `warn` and swallowed.
pub async fn capture_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let cfg = config();

    let path = req.uri().path().to_string();

    // Exclusion check up front: skip the handler-side bookkeeping entirely
    // for excluded paths.
    let excluded = cfg.exclude_prefixes.iter().any(|p| path.starts_with(p.as_str()));

    let method = req.method().to_string();
    let ip = resolve_ip(req.headers());
    let user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let user_id = resolve_user_id(req.headers());

    let started = Instant::now();
    let response = next.run(req).await;
    let duration_ms = started.elapsed().as_millis() as i64;
    let status = response.status().as_u16() as i32;

    // Exclusion + status floor first (cheap, counter-free), so excluded /
    // below-floor requests don't perturb the sampler cadence.
    if excluded || status < cfg.min_status {
        return response;
    }
    // Deterministic sampling: advance the counter for every candidate request
    // (those that passed exclusion + status), so the cadence is stable. The
    // full predicate is `LogsConfig::should_capture` — kept consistent here.
    let seq = SAMPLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    if !cfg.should_capture(&path, status, seq) {
        return response;
    }

    let row = RequestLog {
        id: 0, // assigned by the DB on insert
        method,
        path,
        status,
        duration_ms,
        user_id,
        ip,
        user_agent,
        created_at: Utc::now(), // overwritten by auto_now_add on insert
    };

    // Fire-and-forget: spawn the insert so the response returns immediately.
    // A DB error is logged at `warn` and never propagated to the request.
    let handle = tokio::spawn(async move {
        if let Err(e) = RequestLog::objects().create(row).await {
            warn!(error = ?e, "logs: failed to record request (swallowed)");
        }
    });
    // Track the handle so the test `flush()` hook can await it. Cheap in
    // production (one push per logged request); `flush` is never called there.
    if let Ok(mut guard) = pending().lock() {
        guard.push(handle);
    }

    response
}

// ── Admin visibility ────────────────────────────────────────────────────────

/// A **read-only** [`AdminModel`] for [`RequestLog`]. Register it on the admin
/// so operators can browse captured requests; every column is marked readonly
/// (operators browse logs, they don't author them).
///
/// ```ignore
/// AdminPlugin::default().register(umbra_logs::admin_model())
/// ```
///
/// Feature-gated behind `admin` (on by default). Build with
/// `default-features = false` for a logs-only app that doesn't pull the admin
/// into its dependency graph.
#[cfg(feature = "admin")]
pub fn admin_model() -> AdminModel {
    AdminModel::new("logs_requestlog")
        .label("Request logs")
        .list_display(&["created_at", "method", "path", "status", "duration_ms", "user_id"])
        .list_filter(&["method", "status"])
        .search_fields(&["path"])
        .ordering(&["-created_at"])
        // Every field is read-only: logs are captured by the layer, never
        // authored in the admin.
        .readonly_fields(&[
            "id",
            "method",
            "path",
            "status",
            "duration_ms",
            "user_id",
            "ip",
            "user_agent",
            "created_at",
        ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_full_rate_keeps_all() {
        for seq in 0..20 {
            assert!(sampled(seq, 1.0), "rate 1.0 keeps every request");
        }
    }

    #[test]
    fn sampler_zero_rate_drops_all() {
        for seq in 0..20 {
            assert!(!sampled(seq, 0.0), "rate 0.0 drops every request");
        }
    }

    #[test]
    fn sampler_quarter_rate_keeps_every_fourth() {
        let kept: Vec<u64> = (0..12).filter(|&s| sampled(s, 0.25)).collect();
        assert_eq!(kept, vec![0, 4, 8], "rate 0.25 keeps 1-in-4 on a fixed cadence");
    }
}
