//! Marker plugins + bench-only identity middleware.
//!
//! # The probe seam
//!
//! `App::build` applies plugin `wrap_router` layers in topological
//! dependency order, and **later plugins wrap earlier ones** — so a plugin's
//! `dependencies()` list controls how deep in the onion its layer lands.
//! A `ProbePlugin` is a no-op layer that only records a timestamp into the
//! request's `Trace`. Registering them *between* the real plugins brackets
//! each one:
//!
//! ```text
//! topo order →   probe:outer  (outermost)
//!                  probe:after-security
//!                    security's own layers
//!                  probe:after-sessions
//!                    sessions' own layers
//!                  probe:sessions-inner
//!                    routes
//! ```
//!
//! Time between two adjacent markers is the bracketed plugin's own cost —
//! without modifying the plugin, reaching into the framework, or guessing.
//! The outermost marker also installs the `bench.request` tracing span so
//! sqlx events are attributed to the right request.

use std::time::Instant;

use axum::extract::Request as AxRequest;
use axum::middleware::Next;
use axum::response::Response as AxResponse;
use tracing::Instrument;
use umbral::migrate::ModelMeta;
use umbral::plugin::Plugin;
use umbral::web::{Request, Response, Router};

use crate::telemetry::{Trace, register_span, unregister_span};

pub const PROBE_INNER: &str = "probe:sessions-inner";
pub const PROBE_AFTER_SESSIONS: &str = "probe:after-sessions";
pub const PROBE_AFTER_SECURITY: &str = "probe:after-security";
pub const PROBE_OUTER: &str = "probe:outer";

/// A marker layer with no behaviour beyond stamping the trace.
pub struct ProbePlugin {
    pub label: &'static str,
    pub deps: &'static [&'static str],
}

impl Plugin for ProbePlugin {
    fn name(&self) -> &'static str {
        self.label
    }

    fn dependencies(&self) -> &'static [&'static str] {
        self.deps
    }

    fn wrap_router(&self, router: Router) -> Router {
        let label = self.label;
        let is_outer = label == PROBE_OUTER;
        let is_inner = label == PROBE_INNER;
        router.layer(axum::middleware::from_fn(
            move |req: AxRequest, next: Next| async move {
                probe(req, next, label, is_outer, is_inner).await
            },
        ))
    }
}

async fn probe(
    req: AxRequest,
    next: Next,
    label: &'static str,
    is_outer: bool,
    is_inner: bool,
) -> AxResponse {
    let trace = req.extensions().get::<Trace>().cloned();
    if let Some(t) = &trace {
        t.stamp(&format!("{label}:in"));
    }

    let res = if is_outer {
        if let Some(t) = &trace {
            // The span makes every sqlx event fired while handling this
            // request reachable from the trace (the SQL layer walks
            // event-span → ... → bench.request).
            let span = tracing::info_span!("bench.request", trace_id = t.id());
            let sid = span.id().map(|id| id.into_u64());
            if let Some(sid) = sid {
                register_span(sid, t.id());
            }
            let res = next.run(req).instrument(span).await;
            if let Some(sid) = sid {
                unregister_span(sid);
            }
            res
        } else {
            next.run(req).await
        }
    } else {
        next.run(req).await
    };

    if let Some(t) = &trace {
        t.stamp(&format!("{label}:out"));
    }

    let mut res = res;
    if is_inner {
        // Typed middlewares only see the Response in `after_response`; the
        // innermost marker hands the trace across by copying the handle
        // into the response extensions.
        if let Some(t) = &trace {
            res.extensions_mut().insert(t.clone());
        }
    }
    res
}

/// The app's own plugin: contributes the bench models (link-time discovery)
/// and nothing else. Declared to depend on the outermost probe so its
/// (absent) layer would sit outside every measured plugin.
pub struct HarnessPlugin;

impl Plugin for HarnessPlugin {
    fn name(&self) -> &'static str {
        "bench"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[PROBE_OUTER]
    }

    fn models(&self) -> Vec<ModelMeta> {
        // Every `#[derive(Model)]` in THIS crate (`src/models.rs`).
        umbral::discovered_models!()
    }
}

// ===========================================================================
// Bench-only identity
// ===========================================================================

/// The authenticated principal for a bench request, derived from the
/// `x-bench-user: <id>` header. Exists so load scenarios can act as
/// different users without running the full login ceremony (which would
/// put argon2's ~100 ms CPU inside every "authed" measurement — a cost the
/// harness wants visible ONLY when a scenario deliberately exercises login).
#[derive(Debug, Clone)]
pub struct BenchUser {
    pub id: i64,
}

/// Reads `x-bench-user` into `Extension<Option<BenchUser>>`. No DB access —
/// identity is just the id the client claims (bench traffic only; the real
/// auth plugins are also installed and cover their own routes).
pub struct DevAuth;

#[umbral::async_trait]
impl umbral::middleware::Middleware for DevAuth {
    fn name(&self) -> &'static str {
        "dev auth (header)"
    }

    fn order(&self) -> i32 {
        -95
    }

    async fn before_request(&self, mut req: Request) -> Result<Request, Response> {
        let t = Instant::now();
        let id = req
            .headers()
            .get("x-bench-user")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());
        req.extensions_mut().insert(id.map(|id| BenchUser { id }));
        let trace = req.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_before(self.name(), t.elapsed().as_micros() as u64);
        }
        Ok(req)
    }

    async fn after_response(&self, res: Response) -> Response {
        let t = Instant::now();
        let trace = res.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_after(self.name(), t.elapsed().as_micros() as u64);
        }
        res
    }
}
