//! Standalone umbra example: `#[derive(Model)]` on a user-defined struct,
//! schema managed by the migration engine, served over HTTP.
//!
//! Every umbra symbol comes through the `umbra` facade. There is no
//! `umbra_core::` or `umbra_macros::` anywhere. The derive itself is
//! re-exported as `umbra::orm::Model` (the macro), sharing its name with
//! the `Model` trait via Rust's separate type and macro namespaces — both
//! ride in on `use umbra::prelude::*;` together.
//!
//! What this example demonstrates end-to-end:
//!
//! - Declaring a model with `#[derive(Model)]` and getting the trait impl,
//!   the `objects()` Manager, and the typed column constants for free.
//! - Registering the model with `App::builder().model::<Article>()` so the
//!   M5 migration engine tracks it.
//! - Running `umbra::migrate::make()` + `run()` in-process on startup so
//!   `cargo run` Just Works against a fresh database. This is a demo
//!   pattern; production deployments run `cargo run -p umbra-cli --
//!   migrate` as a separate step.
//! - Inserting rows without explicit IDs (`INSERT INTO article (title,
//!   body) VALUES (...)`) — SQLite's ROWID alias picks up the
//!   monotonically increasing PKs that umbra's migration engine asks for.
//! - Reading rows back through the ambient pool: `Article::objects().
//!   fetch().await` with no pool argument anywhere.

use umbra::migrate::MigrateError;
use umbra::prelude::*;
use umbra::web::StatusCode;

/// A small article model.
///
/// The derive emits:
///
/// - `impl umbra::orm::Model for Article` with `TABLE = "article"` (the
///   snake_case of the struct name) and `FIELDS` populated from the
///   struct's fields in declaration order.
/// - `Article::objects() -> Manager<Article>` so handlers can reach the
///   QuerySet without threading a pool through.
/// - A sibling `mod article` of typed column constants in
///   SCREAMING_SNAKE_CASE: `article::ID`, `article::TITLE`,
///   `article::BODY`, `article::PUBLISHED_AT`.
///
/// `serde::Serialize` lets the handler return `Json<Vec<Article>>`.
/// `sqlx::FromRow` is required by `Model`'s supertrait bound; the
/// QuerySet terminals (`fetch`, `first`) use it to materialise rows.
// `pub` on the struct rather than the default private visibility: the derive
// emits `pub const` column constants in the sibling `mod article`, and
// rustc's `private_interfaces` lint flags those constants for referencing a
// less-visible type. In a real downstream crate the user-defined model is
// almost always `pub` anyway; this matches that shape.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow, Model)]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `App::serve` emits its bind line via `tracing::info!`. Without a
    // subscriber that line is dropped. `EnvFilter` honours `RUST_LOG` so
    // the default verbosity can be tuned without rebuilding.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;

    // Override `sqlite::memory:` with a file-backed URL so the schema and
    // seed data survive across `cargo run` invocations. Bare `:memory:`
    // also gives each pool connection its own isolated database, which
    // would defeat the demo: the seed rows installed on one connection
    // would be invisible to handlers running on another. Anything serious
    // overrides UMBRA_DATABASE_URL.
    let database_url = if settings.database_url == "sqlite::memory:" {
        "sqlite://derive-demo.db?mode=rwc".to_string()
    } else {
        settings.database_url.clone()
    };
    let pool = umbra::db::connect(&database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // M5 wiring: hand the model to the migration engine so
        // `make()` / `run()` below can generate and apply the
        // CreateTable migration on first run.
        .model::<Article>()
        .router(
            Router::new()
                .route("/", get(|| async { "umbra-derive-demo" }))
                .route("/articles", get(list_articles)),
        )
        .build()?;

    // Auto-migrate on startup. `make()` writes one JSON file per plugin
    // that has changes (NoChanges if everything's already up to date),
    // `run()` applies every pending file inside one transaction per
    // file. On the first run this creates the `article` table; on
    // re-runs both calls are no-ops. The migrations directory lands
    // alongside the binary (`./migrations/app/0001_create_article.json`)
    // and the user is expected to commit it.
    //
    // Demo-only convenience. Production deployments split this from
    // the request-serving path: `cargo run -p umbra-cli -- makemigrations`
    // and `migrate` are separate steps in CI.
    auto_migrate().await?;

    // Demo seed. The migration engine renders `id: i64` as
    // `INTEGER PRIMARY KEY AUTOINCREMENT` on SQLite, so the INSERT
    // doesn't supply an `id` value; SQLite hands out 1, 2, ... .
    // `INSERT OR IGNORE` against a clean schema is a no-op on the
    // unique constraints, but kept so a second `cargo run` doesn't
    // duplicate the seed.
    seed_article_rows().await?;

    // 3001 keeps the port distinct from `examples/hello/` (3000) and
    // umbra-cli (8000) so all three can run side-by-side on the same host.
    app.serve("127.0.0.1:3001".parse::<std::net::SocketAddr>()?)
        .await?;

    Ok(())
}

/// Run `makemigrations` and `migrate` in-process. Tolerates `NoChanges`
/// from `make` (the schema is already up to date) and propagates any
/// real error.
async fn auto_migrate() -> Result<(), Box<dyn std::error::Error>> {
    match umbra::migrate::make().await {
        Ok(paths) => {
            for path in paths {
                eprintln!("auto-migrate: wrote {}", path.display());
            }
        }
        Err(MigrateError::NoChanges) => {}
        Err(err) => return Err(Box::new(err)),
    }
    let n = umbra::migrate::run().await?;
    if n > 0 {
        eprintln!("auto-migrate: applied {n} migration(s)");
    }
    Ok(())
}

/// The Django-shape handler. No pool parameter, no `.on(&pool)` on the
/// QuerySet, no `State<DbPool>` extractor. The `Article::objects()`
/// Manager picks up the ambient pool the `App::build()` installed in
/// `umbra::db`'s `OnceLock`, and the terminal `.fetch().await` runs
/// against it.
///
/// `Article::objects()` is generated by the derive. `article::ID`
/// lives in the sibling column module the derive also generates.
async fn list_articles() -> Result<Json<Vec<Article>>, (StatusCode, String)> {
    let articles = Article::objects()
        .order_by(article::ID.asc())
        .fetch()
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(articles))
}

/// Two seed rows so `GET /articles` returns something on a fresh DB.
/// `INSERT OR IGNORE` keeps re-runs idempotent: the unique constraint
/// on the auto-generated PK isn't violated since these inserts let
/// SQLite assign the IDs.
async fn seed_article_rows() -> Result<(), sqlx::Error> {
    let pool = umbra::db::pool();

    // Bail if the table already has rows. Cheaper than two INSERT OR
    // IGNORE roundtrips when the seed has run before, and avoids
    // depending on UNIQUE constraints we don't actually declare on
    // the title.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM article")
        .fetch_one(&pool)
        .await?;
    if count.0 > 0 {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO article (title, body, published_at) VALUES \
         (?, ?, ?), \
         (?, ?, ?)",
    )
    .bind("Deriving Model")
    .bind("this row came back through Article::objects().fetch()")
    .bind("2026-05-30T12:00:00Z")
    .bind("User-defined struct")
    .bind("no hand-written impl Model anywhere in this file")
    .bind(None::<String>)
    .execute(&pool)
    .await?;
    Ok(())
}
