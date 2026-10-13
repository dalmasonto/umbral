//! umbral-bench — the benchmark & profiling harness app.
//!
//! Run from `harness/backend/`:
//! ```bash
//! cargo run -- migrate      # create the schema
//! cargo run -- serve        # boot (seeds on first serve; port 8113)
//! ```
//!
//! What it is: a realistic social-feed umbral app (users, follows, posts,
//! likes, comments) wired with REAL plugins (auth, sessions, security,
//! health) plus a telemetry layer that decomposes every request into
//! middleware / routing / query-build / database / serialize / render —
//! see `src/telemetry.rs` for the seams and `src/probes.rs` for the marker
//! plugins that bracket other plugins' layers.
//!
//! Surfaces:
//!   - HTML:  GET /  (feed),  GET /u/{id} (profile),  GET /posts/{id}
//!   - JSON:  GET /api/feed, /api/feed-naive (N+1 demo),
//!            GET /api/users/{id}/profile (heavy aggregate),
//!            POST /api/posts, /api/posts/{id}/like, /api/users/{id}/follow
//!   - Bench: GET /api/bench/telemetry, /api/bench/middleware,
//!            POST /api/bench/runs, GET /api/bench/runs(/{"{"}id{"}"}),
//!            GET /api/bench/stream (SSE), POST /api/bench/reset
//!
//! Scenarios authenticate as a user via the `x-bench-user: <id>` header
//! (a bench-only identity middleware — see `probes.rs::DevAuth`).

mod api;
mod lab;
mod loadgen;
mod models;
mod probes;
mod seed;
mod telemetry;
mod views;

use umbral::cors::CorsConfig;
use umbral::prelude::*;
use umbral_auth::AuthPlugin;
use umbral_health::HealthPlugin;
use umbral_security::SecurityPlugin;
use umbral_sessions::{CookieStore, SessionsPlugin};

use crate::probes::{
    DevAuth, HarnessPlugin, PROBE_AFTER_SECURITY, PROBE_AFTER_SESSIONS, PROBE_INNER, PROBE_OUTER,
    ProbePlugin,
};
use crate::telemetry::{RequestLog, ResponseStamp, TraceRoot};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // The harness exists to profile SQL per request; statement logging is its
    // core seam (`db::connect` reads this knob, default `off`). Opt in unless
    // the operator explicitly overrode it. SAFETY: single-threaded here — the
    // tokio runtime has not spawned any worker yet.
    if std::env::var("UMBRAL_DB_LOG_STATEMENTS").is_err() {
        unsafe { std::env::set_var("UMBRAL_DB_LOG_STATEMENTS", "debug") };
    }

    telemetry::init_subscriber();

    let settings = Settings::from_env()?;
    let backend = if settings.database_url.starts_with("postgres") {
        "postgres"
    } else {
        "sqlite"
    };
    loadgen::set_backend(backend);
    loadgen::set_self_url(format!("http://{}", settings.bind_addr));
    eprintln!(
        "[umbral-bench] backend={backend} bind={} seed: users={} posts/user={}",
        settings.bind_addr,
        std::env::var("UMBRAL_BENCH_USERS").unwrap_or_else(|_| "2000".into()),
        std::env::var("UMBRAL_BENCH_POSTS_PER_USER").unwrap_or_else(|_| "10".into()),
    );

    let pool = umbral::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // --- bench middlewares (each self-times, see telemetry.rs) ----------
        .middleware(TraceRoot)
        .middleware(DevAuth)
        .middleware(RequestLog)
        .middleware(ResponseStamp)
        // The framework lab: switchable middlewares (+ controls at
        // /api/bench/lab) so the cost of a middleware is observable, not
        // theoretical. Both default off.
        .middleware(lab::BenchLabMiddleware)
        .middleware(lab::RateLimitMiddleware)
        // --- plugins, with marker probes bracketing the real layers ---------
        // Topo order == measurement order: inner marker, sessions, marker,
        // security, marker, outer marker. See probes.rs.
        .plugin(ProbePlugin {
            label: PROBE_INNER,
            deps: &[],
        })
        // Sessions on the STATELESS CookieStore: the record travels in the
        // signed cookie, so there is no per-request store round-trip. The
        // default DbStore adds an awaited session-store probe on every fresh
        // request — the first YIELDING layer in the chain, so under
        // saturation its wall time absorbs the queue and the "sessions" row
        // in Layers reads ~100x everything else. Swap in
        // `.store(umbral_sessions::DbStore)` to reproduce that.
        .plugin(SessionsPlugin::default().store(CookieStore::new()))
        .plugin(ProbePlugin {
            label: PROBE_AFTER_SESSIONS,
            deps: &["sessions"],
        })
        .plugin(SecurityPlugin::new().csrf_exempt(["/api"]))
        .plugin(ProbePlugin {
            label: PROBE_AFTER_SECURITY,
            deps: &["security"],
        })
        .plugin(ProbePlugin {
            label: PROBE_OUTER,
            deps: &[PROBE_AFTER_SECURITY],
        })
        // --- real plugins under measurement --------------------------------
        .plugin(AuthPlugin::new().with_default_routes())
        .plugin(HealthPlugin::default())
        .plugin(HarnessPlugin)
        // The React frontend dev server (Vite) talks to /api cross-origin.
        .cors_for(
            "/api",
            CorsConfig::strict()
                .allow_origins(vec!["http://localhost:5173"])
                .allow_credentials(false),
        )
        .templates_dir("templates")
        .routes(
            Routes::new()
                // HTML pages (template-render workload).
                .get("/", views::feed_page)
                .get("/u/{id}", views::profile_page)
                .get("/posts/{id}", views::post_page)
                // JSON API.
                .get("/api/ping", api::ping)
                .get("/api/feed", api::feed)
                .get("/api/feed-naive", api::feed_naive)
                .get("/api/users/{id}/profile", api::profile)
                .post("/api/posts", api::create_post)
                .post("/api/posts/{id}/like", api::like_post)
                .post("/api/users/{id}/follow", api::follow_user)
                // Bench control + observability.
                .get("/api/bench/telemetry", api::telemetry_snapshot)
                .get("/api/bench/middleware", api::middleware_inventory)
                .post("/api/bench/reset", api::reset_telemetry)
                .get("/api/bench/lab", api::lab_get)
                .post("/api/bench/lab", api::lab_set)
                .post("/api/bench/runs", api::start_run)
                .get("/api/bench/runs", api::list_runs)
                .get("/api/bench/runs/{id}", api::get_run)
                .get("/api/bench/runs/{id}/telemetry", api::run_telemetry)
                .get("/api/bench/stream", api::stream),
        )
        .auto_migrate_on_serve()
        .seed_on_serve(seed::all)
        .build_deferred()?;

    umbral_cli::dispatch(app).await
}
