//! The `manage.py` equivalent binary for umbra.
//!
//! At M0 this was scaffold-only. At M1/M2 the binary doubles as the live
//! demonstration that the ORM works end-to-end through the facade: it
//! creates a `post` table, seeds two rows, and exposes a `GET /posts`
//! route that runs `Post::objects().fetch().await` — with **no
//! `.on(&pool)`** — to prove that the ambient pool installed by
//! `App::build()` is what the QuerySet picks up. That's the Django
//! ergonomic the framework promises.
//!
//! Later milestones add the real `manage.py` shape: `migrate`,
//! `makemigrations`, `worker`, `inspectdb`, configurable bind address,
//! signal-based graceful shutdown. See `docs/specs/06-migration-engine.md`
//! and `docs/specs/07-inspectdb.md` for the subcommand contracts. At
//! that point the hand-rolled CREATE TABLE here goes away (M5's
//! `migrate` handles schema bootstrap) and the demo `Post` model moves
//! to its real shape from `#[derive(Model)]` (M3).

use std::net::SocketAddr;

use umbra::orm::{Post, post};
use umbra::prelude::*;
use umbra::web::{Json, Router, StatusCode};

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

    // For the demo we override sqlite::memory: with a file-backed URL so
    // every pool connection sees the same database. sqlx's pool can open
    // multiple connections; with bare ":memory:" each connection is its
    // own isolated database, and the seed rows installed on one connection
    // would be invisible to handlers running on a different one. A file
    // is the simplest fix. Users override with UMBRA_DATABASE_URL for
    // anything serious.
    let database_url = if settings.database_url == "sqlite::memory:" {
        "sqlite://umbra-cli-demo.db?mode=rwc".to_string()
    } else {
        settings.database_url.clone()
    };
    let pool = umbra::db::connect(&database_url).await?;

    // Demo seed. CREATE TABLE IF NOT EXISTS + INSERT OR IGNORE so re-runs
    // are idempotent. M5's migrate retires this.
    init_post_table(&pool).await?;
    seed_post_rows(&pool).await?;

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
                .route("/healthz", get(|| async { "ok" }))
                .route("/posts", get(list_posts)),
        )
        .build()?;

    app.serve(addr).await?;
    Ok(())
}

/// The Django-shape handler. Notice what isn't here: no pool parameter,
/// no `.on(&pool)` on the QuerySet, no `State<DbPool>` extractor. The
/// `Post::objects()` Manager picks up the ambient pool the
/// `App::build()` installed in `umbra::db`'s `OnceLock`, and the
/// terminal `.fetch().await` runs against it.
///
/// This is the cross-cutting rule from `arch.md §2.2`: process-scoped
/// context (the DB pool) is ambient; request-scoped context (Request,
/// Session, body, params) is an explicit handler argument.
async fn list_posts() -> Result<Json<Vec<Post>>, (StatusCode, String)> {
    let posts = Post::objects()
        .order_by(post::ID.asc())
        .fetch()
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(posts))
}

async fn init_post_table(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            published_at TEXT
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_post_rows(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO post (id, title, body, published_at) VALUES \
         (1, 'Hello from umbra', 'first demo post', '2026-05-30T12:00:00Z'), \
         (2, 'Ambient pool demo', 'no .on(&pool) needed in handlers', '2026-05-30T13:00:00Z')",
    )
    .execute(pool)
    .await?;
    Ok(())
}
