//! The `manage.py` equivalent binary for umbra.
//!
//! At M0 this is intentionally minimal: it loads `Settings`, opens the default
//! database pool, builds an `App` through the `umbra` facade, and serves two
//! hand-written routes on a hardcoded address. No subcommands, no clap, no
//! graceful shutdown — the point is to prove the facade exposes everything a
//! consumer needs end-to-end without anyone reaching into `umbra-core` or
//! `umbra-macros` directly.
//!
//! Later milestones add the real `manage.py` shape: `migrate`,
//! `makemigrations`, `worker`, `inspectdb`, configurable bind address,
//! signal-based graceful shutdown. See `docs/specs/06-migration-engine.md`
//! and `docs/specs/07-inspectdb.md` for the subcommand contracts.

use std::net::SocketAddr;

use umbra::prelude::*;
use umbra::web::Router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `App::serve` logs the bound address through `tracing::info!`. Without a
    // subscriber that line is dropped and the operator gets no feedback that
    // the server is actually up. `EnvFilter` honours `RUST_LOG` so the
    // default verbosity can be tuned without rebuilding.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = match Settings::from_env() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("umbra-cli: failed to load settings: {err}");
            std::process::exit(1);
        }
    };

    let pool = umbra::db::connect(&settings.database_url).await?;

    // `App::serve` takes `impl Into<SocketAddr>`, which is implemented for
    // tuples and `SocketAddr` itself but not for `&str`. The bind address is
    // hardcoded at M0; making it configurable is a later concern.
    let addr: SocketAddr = "127.0.0.1:8000".parse()?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .router(
            Router::new()
                .route("/", get(|| async { "umbra-cli server (M0 scaffold)" }))
                .route("/healthz", get(|| async { "ok" })),
        )
        .build()?;

    app.serve(addr).await?;
    Ok(())
}
