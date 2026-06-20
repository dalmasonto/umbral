//! Liveness + readiness probes for umbra (feature #47).
//!
//! Two routes, kept deliberately separate per the Kubernetes
//! convention:
//!
//! - **`GET /healthz`** — *liveness*. Always returns 200 as long as
//!   the process is up. Kubernetes uses this to decide whether to
//!   restart the pod; an unconditional 200 says "the binary is
//!   running" without claiming the app can serve traffic. No DB
//!   touch, no plugin walk — anything more would risk flapping
//!   pods on transient downstream blips.
//!
//! - **`GET /ready`** — *readiness*. Returns 200 + JSON when the
//!   process is ready to accept traffic; 503 + JSON when it isn't.
//!   "Ready" means: (a) the default DB pool answers `SELECT 1`,
//!   (b) every [`HealthCheck`] the developer registered via
//!   `HealthPlugin::default().check(...)` returns `Ok(())`.
//!   Kubernetes uses this to decide whether to send traffic to the
//!   pod; load balancers use the same signal during rolling
//!   deploys.
//!
//! The split mirrors what every production-grade framework
//! eventually settles on (Spring, Rails ActionCable, Django via
//! third-party plugins). Without it, infrastructure has no way to
//! tell "the binary is alive" from "the binary can serve work" —
//! a Postgres outage during a deploy would flap every pod even
//! though restarting them won't help.
//!
//! # Usage
//!
//! ```ignore
//! use umbra_health::{HealthPlugin, HealthCheck, HealthError};
//!
//! struct RedisCheck { /* ... */ }
//! #[async_trait::async_trait]
//! impl HealthCheck for RedisCheck {
//!     fn name(&self) -> &'static str { "redis" }
//!     async fn check(&self) -> Result<(), HealthError> {
//!         /* PING the cluster */ Ok(())
//!     }
//! }
//!
//! App::builder()
//!     .plugin(HealthPlugin::default().check(RedisCheck { /* ... */ }))
//!     // ...
//!     .build()?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use umbra::plugin::Plugin;
use umbra::routes::RouteSpec;

/// Failure surface for a [`HealthCheck`]. Carries a short reason
/// the `/ready` endpoint surfaces in its JSON body so operators
/// can see which dependency is degraded without grepping logs.
#[derive(Debug)]
pub struct HealthError {
    pub reason: String,
}

impl HealthError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for HealthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for HealthError {}

/// One dependency the readiness probe should check on every
/// `GET /ready` call.
///
/// Implementations should keep their check fast (under a few
/// hundred ms) and side-effect-free. A blocking or slow check
/// will make load balancers think the pod is dead even when it
/// isn't.
#[async_trait::async_trait]
pub trait HealthCheck: Send + Sync + 'static {
    /// Short stable identifier surfaced in the readiness JSON
    /// (`"redis"`, `"stripe"`, `"s3"`). Mostly free-form;
    /// operators key off this when alerting.
    fn name(&self) -> &'static str;
    /// Run the check. `Ok(())` means the dependency is reachable;
    /// `Err(HealthError)` means the pod should be marked
    /// not-ready until the next probe.
    async fn check(&self) -> Result<(), HealthError>;
}

/// Default per-check timeout. Keeps the readiness probe from hanging
/// when one dependency is blocked: after 5 s the check is marked DOWN
/// rather than stalling the response indefinitely.
const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct HealthState {
    checks: Arc<Vec<Arc<dyn HealthCheck>>>,
    /// Per-check timeout. On elapsed the check is recorded as unhealthy
    /// with a `"timed out"` detail instead of hanging the probe.
    check_timeout: Duration,
}

/// Mounts `/healthz` (liveness) and `/ready` (readiness) plus
/// holds the list of developer-registered [`HealthCheck`]s.
///
/// Both routes are unconditionally registered when the plugin is
/// installed; gate them off your reverse proxy or auth middleware
/// if you don't want them publicly reachable. They never carry
/// authentication — by design, k8s and load balancers must reach
/// them without credentials.
pub struct HealthPlugin {
    checks: Vec<Arc<dyn HealthCheck>>,
    /// Per-check timeout applied to every check in the readiness runner
    /// (including the built-in DB probe). Defaults to 5 s.
    check_timeout: Duration,
}

impl std::fmt::Debug for HealthPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthPlugin")
            .field("checks_count", &self.checks.len())
            .field("check_timeout", &self.check_timeout)
            .finish()
    }
}

impl Default for HealthPlugin {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            check_timeout: DEFAULT_CHECK_TIMEOUT,
        }
    }
}

impl HealthPlugin {
    /// Register a [`HealthCheck`]. Chainable.
    pub fn check<C: HealthCheck>(mut self, check: C) -> Self {
        self.checks.push(Arc::new(check));
        self
    }

    /// Override the per-check timeout (default: 5 s).
    ///
    /// Any check — including the built-in DB probe — that does not
    /// complete within this duration is recorded as DOWN with a
    /// `"timed out"` detail, and the probe returns promptly rather
    /// than hanging.
    pub fn check_timeout(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
        self
    }
}

impl Plugin for HealthPlugin {
    fn name(&self) -> &'static str {
        "health"
    }

    fn routes(&self) -> Router {
        let state = HealthState {
            checks: Arc::new(self.checks.clone()),
            check_timeout: self.check_timeout,
        };
        Router::new()
            .route("/healthz", get(liveness))
            .route("/ready", get(readiness))
            .with_state(state)
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/healthz", vec!["GET"]),
            RouteSpec::new("/ready", vec!["GET"]),
        ]
    }
}

#[derive(Serialize)]
struct LivenessBody {
    status: &'static str,
}

async fn liveness() -> (StatusCode, Json<LivenessBody>) {
    // Always 200: liveness is "the process answered the
    // syscall", nothing more.
    (StatusCode::OK, Json(LivenessBody { status: "ok" }))
}

#[derive(Serialize)]
struct ReadinessBody {
    status: &'static str,
    checks: serde_json::Map<String, serde_json::Value>,
}

async fn readiness(State(state): State<HealthState>) -> impl IntoResponse {
    let mut checks = serde_json::Map::new();
    let mut all_ok = true;
    let timeout = state.check_timeout;

    // DB connectivity via the framework's `umbra::db::ping()` — backend-
    // appropriate `SELECT 1`, no raw sqlx in the plugin. Wrapped in the
    // configured timeout so a stuck pool doesn't hang the probe.
    match tokio::time::timeout(timeout, umbra::db::ping()).await {
        Ok(Ok(())) => {
            checks.insert("database".to_string(), serde_json::json!({"status": "ok"}));
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "health: database probe failed");
            all_ok = false;
            checks.insert(
                "database".to_string(),
                serde_json::json!({"status": "fail", "reason": e.to_string()}),
            );
        }
        Err(_elapsed) => {
            tracing::warn!("health: database probe timed out");
            all_ok = false;
            checks.insert(
                "database".to_string(),
                serde_json::json!({"status": "fail", "reason": "timed out"}),
            );
        }
    }

    // Developer-registered checks. Run sequentially rather than
    // concurrently — concurrency would multiply tail latencies
    // and amplify the cost of one slow check across every probe.
    // Each check is bounded by the same configured timeout.
    for check in state.checks.iter() {
        let name = check.name().to_string();
        match tokio::time::timeout(timeout, check.check()).await {
            Ok(Ok(())) => {
                checks.insert(name, serde_json::json!({"status": "ok"}));
            }
            Ok(Err(e)) => {
                tracing::warn!(check = %check.name(), reason = %e, "health: check failed");
                all_ok = false;
                checks.insert(
                    name,
                    serde_json::json!({"status": "fail", "reason": e.reason}),
                );
            }
            Err(_elapsed) => {
                tracing::warn!(check = %check.name(), "health: check timed out");
                all_ok = false;
                checks.insert(
                    name,
                    serde_json::json!({"status": "fail", "reason": "timed out"}),
                );
            }
        }
    }

    let status_code = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = ReadinessBody {
        status: if all_ok { "ok" } else { "fail" },
        checks,
    };
    (status_code, Json(body))
}
