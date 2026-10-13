//! The framework lab — middlewares you can switch on at runtime to watch
//! what a middleware actually costs.
//!
//! Two experiments, both real middlewares in the chain (they appear in the
//! expandable middleware view with their own measured time):
//!
//! - **Injectable latency** (`fake_middleware_us`): sleeps N µs before the
//!   request continues. Crank it up and the whole latency curve shifts while
//!   this middleware's row grows — the clearest way to see "a middleware
//!   costs exactly this much per request, everywhere".
//! - **Rate limit** (`rate_limit_per_sec`): a per-user token bucket that
//!   short-circuits with 429 once exhausted. Shows off the `before_request`
//!   short-circuit path: handlers never run, and the 429s land in the run's
//!   error breakdown with the limiter's own row in the ledger.
//!
//! Controls live at `GET/POST /api/bench/lab`; the frontend's Live view has
//! sliders for both. Defaults: both OFF (0) so plain runs are unaffected.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use umbral::middleware::Middleware;
use umbral::web::{IntoResponse, Request, Response};

use crate::telemetry::Trace;

static FAKE_MIDDLEWARE_US: AtomicU64 = AtomicU64::new(0);
static RATE_LIMIT_PER_SEC: AtomicU64 = AtomicU64::new(0);

/// Current lab settings: injectable middleware latency (µs) and the per-user
/// rate limit (requests/second; 0 = off).
pub fn lab_settings() -> (u64, u64) {
    (
        FAKE_MIDDLEWARE_US.load(Ordering::Relaxed),
        RATE_LIMIT_PER_SEC.load(Ordering::Relaxed),
    )
}

pub fn set_lab_settings(fake_middleware_us: Option<u64>, rate_limit_per_sec: Option<u64>) {
    if let Some(us) = fake_middleware_us {
        FAKE_MIDDLEWARE_US.store(us.min(100_000), Ordering::Relaxed);
    }
    if let Some(limit) = rate_limit_per_sec {
        RATE_LIMIT_PER_SEC.store(limit.min(100_000), Ordering::Relaxed);
    }
}

// ===========================================================================
// Injectable latency
// ===========================================================================

/// Sleeps `fake_middleware_us` into every request. A no-op at 0 — but when
/// it is not, its own row is where the injected time shows up, per request.
pub struct BenchLabMiddleware;

#[umbral::async_trait]
impl Middleware for BenchLabMiddleware {
    fn name(&self) -> &'static str {
        "bench lab (injectable)"
    }

    fn order(&self) -> i32 {
        -70
    }

    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        let t = Instant::now();
        let us = FAKE_MIDDLEWARE_US.load(Ordering::Relaxed);
        if us > 0 {
            tokio::time::sleep(Duration::from_micros(us)).await;
        }
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

// ===========================================================================
// Rate limit
// ===========================================================================

struct Bucket {
    tokens: f64,
    last: Instant,
}

fn buckets() -> &'static Mutex<HashMap<i64, Bucket>> {
    static B: OnceLock<Mutex<HashMap<i64, Bucket>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Per-identity token bucket (1-second refill). Off by default; when on,
/// excess requests short-circuit with 429 before any inner layer or handler
/// runs — the handler never executes, and the client gets a real 429.
pub struct RateLimitMiddleware;

#[umbral::async_trait]
impl Middleware for RateLimitMiddleware {
    fn name(&self) -> &'static str {
        "api rate limit"
    }

    fn order(&self) -> i32 {
        -30
    }

    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        let t = Instant::now();
        // Grab the trace handle up-front: the short-circuit path consumes the
        // request, and a tripped limiter is exactly the row the timing view
        // should make visible.
        let trace = req.extensions().get::<Trace>().cloned();

        let limit = RATE_LIMIT_PER_SEC.load(Ordering::Relaxed);
        let mut verdict: Result<Request, Response> = Ok(req);
        if limit > 0 {
            let key = {
                let r = verdict.as_ref().ok().expect("verdict is Ok here");
                r.headers()
                    .get("x-bench-user")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0) // anonymous share one bucket
            };
            let allowed = {
                let now = Instant::now();
                let mut map = buckets().lock().unwrap_or_else(|e| e.into_inner());
                let b = map.entry(key).or_insert_with(|| Bucket {
                    tokens: limit as f64,
                    last: now,
                });
                let elapsed = now.duration_since(b.last).as_secs_f64();
                b.tokens = (b.tokens + elapsed * limit as f64).min(limit as f64);
                b.last = now;
                if b.tokens >= 1.0 {
                    b.tokens -= 1.0;
                    true
                } else {
                    false
                }
            };
            if !allowed {
                verdict = Err(
                    (StatusCode::TOO_MANY_REQUESTS, "rate limited by the bench lab")
                        .into_response(),
                );
            }
        }

        if let Some(trace) = trace {
            trace.mw_before(self.name(), t.elapsed().as_micros() as u64);
        }
        verdict
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
