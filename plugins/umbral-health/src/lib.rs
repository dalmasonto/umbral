//! Liveness + readiness probes for umbral (feature #47).
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
//! - **`GET /ready`** (alias **`GET /readyz`**) — *readiness*. Returns
//!   200 + JSON when the process is ready to accept traffic; 503 + JSON
//!   when it isn't. "Ready" means: (a) the default DB pool answers
//!   `SELECT 1`, (b) every [`HealthCheck`] the developer registered via
//!   `HealthPlugin::default().check(...)` returns `Ok(())`, and (c) — when
//!   [`HealthPlugin::require_migrations`] is on — no on-disk migration is
//!   unapplied. Kubernetes uses this to decide whether to send traffic to
//!   the pod; load balancers use the same signal during rolling deploys.
//!
//! The split mirrors what every production-grade framework
//! eventually settles on (Spring, Rails ActionCable). Without it,
//! infrastructure has no way to
//! tell "the binary is alive" from "the binary can serve work" —
//! a Postgres outage during a deploy would flap every pod even
//! though restarting them won't help.
//!
//! # Usage
//!
//! ```ignore
//! use umbral_health::{HealthPlugin, HealthCheck, HealthError};
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
use umbral::plugin::Plugin;
use umbral::routes::RouteSpec;

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
    /// When true, readiness also gates on the migration state: a pod whose
    /// database has not applied the migrations this binary carries is reported
    /// not-ready. Opt in via [`HealthPlugin::require_migrations`]. Kikosi #5.
    require_migrations: bool,
}

/// Map a migration [`umbral::migrate::DriftReport`] to a readiness verdict.
///
/// `Ok(())` when the schema is at least as new as this binary — nothing on disk
/// is unapplied. `Err(reason)` — hold traffic off the pod — when the database is
/// behind the code. Pure, so the exact gating rule is unit-tested against
/// hand-built reports rather than requiring a live database and a temp
/// migrations tree at the health layer.
///
/// Only `Pending` blocks. `DriftReport::pending` deliberately excludes
/// `AppliedButMissing` (the database is *ahead* — a valid rollback where an
/// older binary should keep serving) and `OutOfOrder` (a stray file the migrate
/// engine only warns about), so this reports a rollback as ready, not stuck.
fn evaluate_migrations(report: &umbral::migrate::DriftReport) -> Result<(), String> {
    let pending = report.pending();
    match pending.len() {
        0 => Ok(()),
        1 => Err("1 migration pending".to_string()),
        n => Err(format!("{n} migrations pending")),
    }
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
    /// See [`HealthPlugin::require_migrations`]. Default `false`.
    require_migrations: bool,
}

impl std::fmt::Debug for HealthPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthPlugin")
            .field("checks_count", &self.checks.len())
            .field("check_timeout", &self.check_timeout)
            .field("require_migrations", &self.require_migrations)
            .finish()
    }
}

impl Default for HealthPlugin {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            check_timeout: DEFAULT_CHECK_TIMEOUT,
            require_migrations: false,
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

    /// Also gate readiness on the migration state (Kikosi #5 / gaps3 #38).
    ///
    /// With this on, `/ready` (and `/readyz`) additionally reports a
    /// `"migrations"` check and returns 503 while the database is behind this
    /// binary's code — i.e. while any migration on disk is unapplied. It becomes
    /// ready the moment the schema catches up.
    ///
    /// This is the fix for the classic rolling-deploy race: a new web container
    /// boots before the one-shot `migrate` job finishes, connects to a database
    /// on the *old* schema, and — with a DB-only readiness check — reports ready
    /// and starts 500ing against columns that don't exist yet. Gating on
    /// migrations holds the pod out of the load balancer until `migrate` lands,
    /// then lets it in.
    ///
    /// Opt-in, not default: the check reads the on-disk `migrations/` tree, and
    /// an app that ships its schema some other way (baked image, external
    /// migration tool) would otherwise see spurious 503s. It is the recommended
    /// production setting — point your container `HEALTHCHECK` / k8s
    /// `readinessProbe` at `/readyz` with this enabled.
    ///
    /// A rollback (an older binary against a newer schema) stays ready: only
    /// unapplied-on-disk migrations block, never a database that is ahead.
    pub fn require_migrations(mut self) -> Self {
        self.require_migrations = true;
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
            require_migrations: self.require_migrations,
        };
        Router::new()
            .route("/healthz", get(liveness))
            // `/ready` is the original path; `/readyz` is the k8s-convention
            // alias (matching `/healthz`/`/livez`/`/readyz`). Same handler.
            .route("/ready", get(readiness))
            .route("/readyz", get(readiness))
            .with_state(state)
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/healthz", vec!["GET"]),
            RouteSpec::new("/ready", vec!["GET"]),
            RouteSpec::new("/readyz", vec!["GET"]),
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

    // DB connectivity via the framework's `umbral::db::ping()` — backend-
    // appropriate `SELECT 1`, no raw sqlx in the plugin. Wrapped in the
    // configured timeout so a stuck pool doesn't hang the probe.
    match tokio::time::timeout(timeout, umbral::db::ping()).await {
        Ok(Ok(())) => {
            checks.insert("database".to_string(), serde_json::json!({"status": "ok"}));
        }
        Ok(Err(e)) => {
            // Log the real error server-side, but never put the raw DB error
            // (which can carry the DSN / host / user) into the unauthenticated
            // /ready body — a generic reason only (audit_2 plugin-observability #3).
            tracing::warn!(error = %e, "health: database probe failed");
            all_ok = false;
            checks.insert(
                "database".to_string(),
                serde_json::json!({"status": "fail", "reason": "unavailable"}),
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

    // Migration readiness (Kikosi #5). Opt-in: a pod whose database is behind
    // the migrations this binary carries is not ready to serve. `drift_report`
    // reads the tracking table + on-disk tree and prints nothing, so it is safe
    // on every probe; the timeout guards a stuck pool the same way the DB probe
    // does.
    if state.require_migrations {
        match tokio::time::timeout(timeout, umbral::migrate::drift_report()).await {
            Ok(Ok(report)) => match evaluate_migrations(&report) {
                Ok(()) => {
                    checks.insert(
                        "migrations".to_string(),
                        serde_json::json!({"status": "ok"}),
                    );
                }
                Err(reason) => {
                    all_ok = false;
                    checks.insert(
                        "migrations".to_string(),
                        serde_json::json!({"status": "fail", "reason": reason}),
                    );
                }
            },
            Ok(Err(e)) => {
                // The real error (which can name the DSN / a migration path) is
                // logged; the unauthenticated body carries a generic reason.
                tracing::warn!(error = %e, "health: migration probe failed");
                all_ok = false;
                checks.insert(
                    "migrations".to_string(),
                    serde_json::json!({"status": "fail", "reason": "unavailable"}),
                );
            }
            Err(_elapsed) => {
                tracing::warn!("health: migration probe timed out");
                all_ok = false;
                checks.insert(
                    "migrations".to_string(),
                    serde_json::json!({"status": "fail", "reason": "timed out"}),
                );
            }
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

#[cfg(test)]
mod tests {
    use super::evaluate_migrations;
    use umbral::migrate::{DriftReport, MigrationEntry, MigrationStatus};

    fn entry(name: &str, status: MigrationStatus) -> MigrationEntry {
        MigrationEntry {
            plugin: "app".to_string(),
            name: name.to_string(),
            status,
        }
    }

    fn report(entries: Vec<MigrationEntry>) -> DriftReport {
        DriftReport { entries }
    }

    /// The schema is caught up — nothing on disk is unapplied — so the pod is
    /// ready.
    #[test]
    fn no_pending_is_ready() {
        assert!(evaluate_migrations(&report(vec![])).is_ok());
        assert!(
            evaluate_migrations(&report(vec![entry("0001", MigrationStatus::Applied)])).is_ok()
        );
    }

    /// Pending migrations hold the pod out of the load balancer, and the reason
    /// carries a count operators can read.
    #[test]
    fn pending_is_not_ready_with_a_count() {
        assert_eq!(
            evaluate_migrations(&report(vec![entry("0002", MigrationStatus::Pending)])),
            Err("1 migration pending".to_string()),
        );
        assert_eq!(
            evaluate_migrations(&report(vec![
                entry("0002", MigrationStatus::Pending),
                entry("0003", MigrationStatus::Pending),
            ])),
            Err("2 migrations pending".to_string()),
        );
    }

    /// A rollback — an older binary against a newer schema — reports the
    /// database's extra migrations as `AppliedButMissing`, NOT `Pending`. The
    /// pod must stay ready: blocking here would make every rollback fail its
    /// readiness probe and get pulled from rotation.
    #[test]
    fn a_database_ahead_of_the_code_stays_ready() {
        assert!(
            evaluate_migrations(&report(vec![entry(
                "0009",
                MigrationStatus::AppliedButMissing
            )]))
            .is_ok(),
            "AppliedButMissing means the DB is ahead — a valid rollback, not a blocker",
        );
    }

    /// An out-of-order file is a warn-only state in the migrate engine; it must
    /// not hold traffic.
    #[test]
    fn an_out_of_order_file_stays_ready() {
        assert!(
            evaluate_migrations(&report(vec![entry("0004", MigrationStatus::OutOfOrder)])).is_ok()
        );
    }

    /// A real pending migration blocks even when mixed with a DB-ahead entry:
    /// the code is genuinely newer than the schema in at least one place.
    #[test]
    fn one_pending_among_others_still_blocks() {
        assert_eq!(
            evaluate_migrations(&report(vec![
                entry("0009", MigrationStatus::AppliedButMissing),
                entry("0010", MigrationStatus::Pending),
            ])),
            Err("1 migration pending".to_string()),
        );
    }
}
