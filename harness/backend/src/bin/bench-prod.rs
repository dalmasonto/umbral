//! bench-prod — the MEASURED application.
//!
//! Mounts exactly what a production deployment of this app would: real
//! session auth, real CSRF protection, real security headers, the real
//! `DbStore` session backend, and the same query logic the instrumented
//! binary decomposes.
//!
//! Deliberately ABSENT: `TraceRoot`, `DevAuth`, `RequestLog`, `ResponseStamp`,
//! every `ProbePlugin`, `BenchLabMiddleware`, `RateLimitMiddleware`, and the
//! `UMBRAL_DB_LOG_STATEMENTS` override. Because none of them are constructed
//! here, a number from this binary cannot have measurement scaffolding in its
//! path.
//!
//! Boot:
//! ```bash
//! cargo run --release --bin umbral-bench-prod -- migrate
//! cargo run --release --bin umbral-bench-prod -- serve
//! ```

use umbral::cli::{CliError, PluginCommand, clap};
use umbral::cors::CorsConfig;
use umbral::prelude::*;
use umbral_auth::AuthPlugin;
use umbral_health::HealthPlugin;
use umbral_security::SecurityPlugin;
use umbral_sessions::SessionsPlugin;

use umbral_bench::models::BenchPlugin;
use umbral_bench::{prod_handlers, seed};

/// `umbral-bench-prod seed-sessions` — give the first `bench_user_1..=N`
/// DATASET users the pool password, the ones `bench/seed-sessions.sh` logs in
/// as.
///
/// A project-owned command (`AppBuilder::command`), not a plugin: it belongs
/// to this binary, not to a reusable unit. It runs against live data, so the
/// default `needs_ready() == true` is right — plugins are up and the pool is
/// open before the first `set_password`.
///
/// Size comes from `BENCH_SESSION_POOL_SIZE` (default 10000 — the peak VU
/// count, one pooled session per VU) and the password from `BENCH_PASSWORD`
/// (default `bench-password-123`), the same two variables the pool builder
/// script reads, so the two halves cannot drift on the default. The users are
/// the dataset's own rows: nothing is created, nothing is deleted, and a
/// missing `bench_user_i` (a pool sized past the dataset) is a hard error.
struct SeedSessions;

#[umbral::async_trait]
impl PluginCommand for SeedSessions {
    fn command(&self) -> clap::Command {
        clap::Command::new("seed-sessions")
            .about("Give the first N dataset users the pool password")
            .long_about(
                "Give the first `bench_user_1..bench_user_N` dataset users the password in \
                 BENCH_PASSWORD, so `bench/seed-sessions.sh` can log them in and build \
                 `bench/session_pool.json` — one pooled session per VU.\n\n\
                 N comes from BENCH_SESSION_POOL_SIZE (default 10000). Idempotent and \
                 non-destructive: the users are the dataset's own rows, so nothing is \
                 created and nothing is deleted — `auth_user` keeps its row count and id \
                 range, and only `password_hash` changes. A user whose stored hash already \
                 verifies is left untouched; a placeholder (or a rotated BENCH_PASSWORD) \
                 is set in place. A missing dataset user is an error: the pool is sized \
                 past the dataset.",
            )
    }

    async fn run(&self, _matches: &clap::ArgMatches) -> Result<(), CliError> {
        seed::seed_session_pool(&seed::SessionPoolConfig::from_env()).await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut settings = Settings::from_env()?;
    // The prod bench owns 8114 so it can never disturb a developer's dev
    // server on the instrumented binary's 8113 (`umbral.toml` in this
    // directory sets 8113, so the default below has to override the file, not
    // just fill a hole). Two ways to say something else:
    //   - `UMBRAL_BIND` — this binary's explicit override; the run silo's
    //     container sets it to `0.0.0.0:8114`.
    //   - `UMBRAL_BIND_ADDR` — the framework's own knob, which
    //     `Settings::from_env` has already applied, so it is left alone.
    if let Ok(addr) = std::env::var("UMBRAL_BIND") {
        settings.bind_addr = addr;
    } else if std::env::var("UMBRAL_BIND_ADDR").is_err() {
        settings.bind_addr = "127.0.0.1:8114".to_string();
    }

    let pool = umbral::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // The app's plugin — the same models owner the instrumented binary
        // mounts as `probes::HarnessPlugin`, minus that one's dependency on
        // the outermost probe (instrumentation-layer ordering). Prod cannot
        // mount `HarnessPlugin` itself: registering the probe to satisfy the
        // dependency is exactly the scaffolding prod must not construct, and
        // a declared dependency on an unregistered plugin is a hard build
        // error. Without a `bench`-named owner, `migrate` reads the rows
        // already in `umbral_migrations` as applied-but-missing and refuses
        // to boot.
        .plugin(BenchPlugin)
        // Real plugins only. `SessionsPlugin::default()` is `DbStore` — a real
        // session read per authenticated request, which is the honest cost.
        .plugin(SessionsPlugin::default())
        .plugin(SecurityPlugin::new().csrf_exempt(["/api"]))
        .plugin(AuthPlugin::new().with_default_routes())
        .plugin(HealthPlugin::default())
        .cors_for(
            "/api",
            CorsConfig::strict()
                .allow_origins(vec!["http://localhost:5173"])
                .allow_credentials(false),
        )
        // The pool's credentials come from the same binary that serves the
        // measured traffic, so `bench/seed-sessions.sh` never has to know how
        // a password is hashed — it only logs in.
        .command(SeedSessions)
        .templates_dir("templates")
        .routes(
            Routes::new()
                .get("/", prod_handlers::feed_page)
                .get("/u/{id}", prod_handlers::profile_page)
                .get("/posts/{id}", prod_handlers::post_page)
                .get("/api/ping", prod_handlers::ping)
                .get("/api/feed", prod_handlers::feed)
                .get("/api/feed-naive", prod_handlers::feed_naive)
                .get("/api/users/{id}/profile", prod_handlers::profile)
                .post("/api/posts", prod_handlers::create_post)
                .post("/api/posts/{id}/like", prod_handlers::like_post)
                .post("/api/users/{id}/follow", prod_handlers::follow_user),
        )
        .auto_migrate_on_serve()
        .seed_on_serve(seed::all)
        .build_deferred()?;

    umbral_cli::dispatch(app).await
}
