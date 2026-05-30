//! The `manage.py` equivalent binary for umbra.
//!
//! M0 was scaffold-only. M1/M2 used the binary as a live demonstration
//! that the ORM worked end-to-end through the facade (CREATE TABLE +
//! seed, then a `GET /posts` route that ran `Post::objects().fetch()`
//! against the ambient pool). M5 grows the binary into the real
//! `manage.py` shape: clap-driven subcommand dispatch with `serve`
//! (the default) alongside the migration trio `makemigrations`,
//! `migrate`, and `showmigrations`.
//!
//! Every subcommand boots through the same `App::builder()` so the
//! ambient pool and the model registry get published before the
//! command runs. The non-serve subcommands skip the listener bind and
//! the demo seed; they only need the pool + registry, both of which
//! `App::build()` publishes synchronously.
//!
//! Later milestones add `worker`, `inspectdb`, a configurable bind
//! address, signal-based graceful shutdown, and (from M7) per-plugin
//! subcommands surfaced via `Plugin::commands()`. See
//! `docs/specs/06-migration-engine.md` and `docs/specs/07-inspectdb.md`.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use umbra::inspect::{InspectError, InspectOptions};
use umbra::migrate::MigrateError;
use umbra::orm::{Post, post};
use umbra::prelude::*;
use umbra::web::{Json, Router, StatusCode};

/// Top-level CLI surface. `command` is optional so a bare `umbra-cli`
/// invocation keeps booting the server (the M0/M1 default), matching
/// Django's `manage.py runserver` not being the implicit default but
/// the framework's current convention until M9 adds richer commands.
#[derive(Debug, Parser)]
#[command(
    name = "umbra-cli",
    about = "The manage.py equivalent for umbra.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Boot the HTTP server on 127.0.0.1:8000. The default when no
    /// subcommand is given.
    Serve,
    /// Diff the registered models against the latest snapshot and
    /// write a new migration file under `migrations/app/`.
    Makemigrations,
    /// Apply every pending migration file against the ambient pool.
    Migrate,
    /// List applied vs pending migrations.
    Showmigrations,
    /// Introspect the database, generate a `models.rs` plus an initial
    /// migration, and optionally mark it applied. See
    /// `docs/specs/07-inspectdb.md`.
    Inspectdb {
        /// Directory the generated files are written under. `models.rs`
        /// lands at the root; the migration lands at
        /// `<output>/migrations/app/0001_initial.json`.
        #[arg(long)]
        output: PathBuf,
        /// Record `0001_initial` in `umbra_migrations` after writing it,
        /// so the next `migrate` is a no-op against an already-populated
        /// database.
        #[arg(long, default_value_t = false)]
        mark_applied: bool,
    },
}

#[tokio::main]
async fn main() {
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

    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Makemigrations => makemigrations().await,
        Command::Migrate => migrate().await,
        Command::Showmigrations => showmigrations().await,
        Command::Inspectdb {
            output,
            mark_applied,
        } => inspectdb(output, mark_applied).await,
    };
    // Catch the error explicitly so the user-facing diagnostic uses
    // the `Display` impl (`umbra inspectdb: column \`x.y\` has
    // unsupported SQL type \`BLOB\`; ...`) rather than the `Debug`
    // dump (`UnsupportedColumnType { table: "x", ... }`) Rust would
    // otherwise print from `fn main() -> Result<_, E>`.
    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Boot the App and run the HTTP server. The default subcommand.
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_settings()?;

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
    // are idempotent. The M5 migrate path supersedes this for users who
    // declare the model and migrate; the seed sticks around so `serve`
    // is self-contained without a separate migrate step.
    init_post_table(&pool).await?;
    seed_post_rows(&pool).await?;

    // `App::serve` takes `impl Into<SocketAddr>`, which is implemented for
    // tuples and `SocketAddr` itself but not for `&str`. The bind address is
    // hardcoded at M0; making it configurable is a later concern.
    let addr: SocketAddr = "127.0.0.1:8000".parse()?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Post>()
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

/// `makemigrations`: diff the registry against the latest snapshot
/// for every registered plugin and write one migration file per plugin
/// that has changes. Prints one `Wrote <path>` line per written file,
/// or `no changes detected` on the `NoChanges` sentinel.
async fn makemigrations() -> Result<(), Box<dyn std::error::Error>> {
    boot_for_management().await?;
    match umbra::migrate::make().await {
        Ok(paths) => {
            for path in paths {
                println!("Wrote {}", path.display());
            }
            Ok(())
        }
        Err(MigrateError::NoChanges) => {
            println!("no changes detected");
            Ok(())
        }
        Err(err) => Err(Box::new(err)),
    }
}

/// `migrate`: apply every pending migration against the ambient pool.
async fn migrate() -> Result<(), Box<dyn std::error::Error>> {
    boot_for_management().await?;
    let n = umbra::migrate::run().await?;
    if n == 0 {
        println!("No pending migrations");
    } else {
        println!("Applied {n} migration(s)");
    }
    Ok(())
}

/// `showmigrations`: print per-migration applied/pending state.
async fn showmigrations() -> Result<(), Box<dyn std::error::Error>> {
    boot_for_management().await?;
    umbra::migrate::show().await?;
    Ok(())
}

/// `inspectdb`: introspect the ambient SQLite pool into a `models.rs`
/// and an initial migration under `--output`. On the empty-DB sentinel
/// (`InspectError::NoTables`) the binary prints a short note and exits
/// successfully; any other error propagates.
async fn inspectdb(output: PathBuf, mark_applied: bool) -> Result<(), Box<dyn std::error::Error>> {
    boot_for_management().await?;
    let opts = InspectOptions {
        output,
        mark_applied,
    };
    match umbra::inspect::inspectdb(opts).await {
        Ok(report) => {
            println!(
                "Inspected {} table(s), {} column(s)",
                report.tables, report.columns,
            );
            println!("Wrote {}", report.models_path.display());
            println!("Wrote {}", report.migration_path.display());
            Ok(())
        }
        Err(InspectError::NoTables) => {
            println!("no tables found in the database");
            Ok(())
        }
        Err(err) => Err(Box::new(err)),
    }
}

/// Shared boot path for the migration subcommands. Opens the pool,
/// builds the App so the ambient pool and the model registry get
/// published, and discards the resulting `App` value (no listener
/// bind). The published `OnceLock`s are what `umbra::migrate::*`
/// needs.
async fn boot_for_management() -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_settings()?;
    let pool = umbra::db::connect(&settings.database_url).await?;
    let _app = App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Post>()
        .router(Router::new())
        .build()?;
    Ok(())
}

fn load_settings() -> Result<Settings, Box<dyn std::error::Error>> {
    match Settings::from_env() {
        Ok(s) => Ok(s),
        Err(err) => {
            eprintln!("umbra-cli: failed to load settings: {err}");
            std::process::exit(1);
        }
    }
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
