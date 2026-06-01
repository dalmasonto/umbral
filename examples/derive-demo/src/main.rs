//! Standalone umbra example: `#[derive(Model)]` on a user-defined struct,
//! schema managed by the migration engine, served over HTTP with both
//! HTML and JSON endpoints.
//!
//! What this example demonstrates end-to-end:
//!
//! - Declaring a model with `#[derive(Model)]` and getting the trait impl,
//!   the `objects()` Manager, and the typed column constants for free.
//! - Registering the model with `App::builder().model::<Article>()` so the
//!   M5 migration engine tracks it.
//! - Running `umbra::migrate::make()` + `run()` in-process on startup so
//!   `cargo run` Just Works against a fresh database.
//! - Inserting rows without explicit IDs — SQLite's
//!   `INTEGER PRIMARY KEY AUTOINCREMENT` (the shape the migration engine
//!   renders for `i64` PKs) hands out monotonically increasing ids.
//! - Reading rows back through the ambient pool:
//!   `Article::objects().fetch().await` with no pool argument anywhere.
//! - Rendering HTML pages with `umbra::templates`: a `base.html` carrying
//!   the layout, child templates extending it with `{% block content %}`,
//!   and autoescape on by default for the XSS guarantee.
//! - Wiring the Django-shaped 404/500 fallback: drop a `404.html` and
//!   `500.html` in the templates dir, point at them with
//!   `not_found_template` / `server_error_template`, and the framework
//!   renders them for unhandled routes and handler panics. The user
//!   just authors the HTML — the request path and dev-mode error
//!   details show up in template context automatically.
//! - Coexisting HTML and JSON surfaces: `/articles` renders the list
//!   page, `/api/articles` returns the same data as JSON.

use umbra::migrate::MigrateError;
use umbra::prelude::*;
use umbra::templates::context;
use umbra::web::{Html, StatusCode};

/// A small article model.
///
/// The derive emits `impl Model for Article`, `Article::objects()`,
/// and the sibling column module `article` (with `article::ID`,
/// `::TITLE`, `::BODY`, `::PUBLISHED_AT`). `serde::Serialize` lets
/// the handler return `Json<Vec<Article>>` and lets the templates
/// engine render fields via `{{ article.title }}`. `sqlx::FromRow`
/// is required by `Model`'s supertrait bound.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow, Model)]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;

    // Override `sqlite::memory:` with a file-backed URL so the schema
    // and seed survive across `cargo run` invocations. Anything
    // serious overrides UMBRA_DATABASE_URL.
    let database_url = if settings.database_url == "sqlite::memory:" {
        "sqlite://derive-demo.db?mode=rwc".to_string()
    } else {
        settings.database_url.clone()
    };
    let pool = umbra::db::connect(&database_url).await?;

    let app = (App::builder()
        .settings(settings)
        .database("default", pool)
        // Hand the model to the migration engine.
        .model::<Article>()
        // Point the templates engine at the example's `templates/`
        // directory. `CARGO_MANIFEST_DIR` is set at compile time to
        // the crate root, so this works regardless of where the
        // binary is invoked from (a real downstream app would point
        // at an absolute path or use the default `./templates`
        // relative to its deploy layout).
        .templates_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/templates"))
        // Django-shaped 404: the framework renders `templates/404.html`
        // for any route that doesn't match. The request path lands in
        // the template's `path` context variable. Replaces the
        // pre-builtin `not_found` fallback handler this example used
        // to carry inline.
        .not_found_template("404.html")
        // Django-shaped 500: a `tower_http::catch_panic` layer
        // installed by the builder turns handler panics into a 500
        // response rendered through `templates/500.html`. In dev mode
        // the template receives `error_display`, `error_chain`, and
        // `request_path` for an expandable detail block.
        .server_error_template("500.html")
        // Auto-generated JSON CRUD at /api/article/. The RestPlugin
        // walks the same model registry the migration engine uses,
        // so the surface stays in lockstep with the schema for free.
        .plugin(umbra_rest::RestPlugin::default())
        .plugin(umbra_openapi::OpenApiPlugin::new())
        .router(
            Router::new()
                .route("/", get(home))
                .route("/articles", get(list_articles_html))
                .route("/articles/{id}", get(article_detail))
                // Backwards-compat alias from the pre-RestPlugin era.
                // New clients should hit /api/article/ instead.
                .route("/api/articles", get(list_articles_json)),
            // Unhandled routes fall through to the framework's
            // `not_found_template` fallback (installed above), which
            // renders 404.html with the request path in scope.
        ))
    .build()?;

    // Auto-migrate on startup. Demo-only convenience. Production
    // deployments split this from request-serving: `cargo run -p
    // umbra-cli -- makemigrations` and `migrate` are separate steps.
    auto_migrate().await?;
    seed_article_rows().await?;

    app.serve("127.0.0.1:3001".parse::<std::net::SocketAddr>()?)
        .await?;
    Ok(())
}

/// Home page. Counts the rows so the template has something to show
/// without re-listing the full set.
async fn home() -> Result<Html<String>, (StatusCode, String)> {
    let count = Article::objects().count().await.map_err(internal_error)?;
    let body = umbra::templates::render("home.html", &context!(article_count => count))
        .map_err(internal_error)?;
    Ok(Html(body))
}

/// HTML list view. Same QuerySet the JSON endpoint uses; the only
/// difference is which template runs over the result.
async fn list_articles_html() -> Result<Html<String>, (StatusCode, String)> {
    let articles = Article::objects()
        .order_by(article::ID.asc())
        .fetch()
        .await
        .map_err(internal_error)?;
    let body = umbra::templates::render("articles_list.html", &context!(articles))
        .map_err(internal_error)?;
    Ok(Html(body))
}

/// HTML detail view. The path param is the article id; a row that
/// doesn't exist renders the `not_found.html` template with a 404.
async fn article_detail(
    Path(id): Path<i64>,
) -> Result<(StatusCode, Html<String>), (StatusCode, String)> {
    let found = Article::objects()
        .filter(article::ID.eq(id))
        .first()
        .await
        .map_err(internal_error)?;

    match found {
        Some(article) => {
            let body = umbra::templates::render("article_detail.html", &context!(article))
                .map_err(internal_error)?;
            Ok((StatusCode::OK, Html(body)))
        }
        None => {
            let body = umbra::templates::render("not_found.html", &context!(id))
                .map_err(internal_error)?;
            Ok((StatusCode::NOT_FOUND, Html(body)))
        }
    }
}

/// JSON endpoint. The original `/articles` from before HTML routes
/// landed; kept under `/api/articles` so the JSON shape stays
/// reachable for clients (and for the documentation pages that
/// reference it).
async fn list_articles_json() -> Result<Json<Vec<Article>>, (StatusCode, String)> {
    let articles = Article::objects()
        .order_by(article::ID.asc())
        .fetch()
        .await
        .map_err(internal_error)?;
    Ok(Json(articles))
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

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

async fn seed_article_rows() -> Result<(), Box<dyn std::error::Error>> {
    // Use the ORM's count() instead of a raw COUNT(*) so this stays
    // backend-agnostic — the same call runs against either SQLite or
    // Postgres without table-name escaping or dialect tweaks.
    let count = Article::objects().count().await?;
    if count > 0 {
        return Ok(());
    }
    // sqlx::query(
    //     "INSERT INTO article (title, body, published_at) VALUES \
    //      (?, ?, ?), \
    //      (?, ?, ?)",
    // )
    // .bind("Deriving Model")
    // .bind("Article::objects().fetch() returned this row.")
    // .bind("2026-05-30T12:00:00Z")
    // .bind("User-defined struct")
    // .bind("No hand-written impl Model anywhere in this file.")
    // .bind(None::<String>)
    // .execute(&pool)
    // .await?;

    let articles = vec![
        Article {
            id: 1,
            title: "Deriving Model".to_string(),
            body: "Article::objects().fetch() returned this row.".to_string(),
            published_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-05-30T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        },
        Article {
            id: 2,
            title: "User-defined struct".to_string(),
            body: "No hand-written impl Model anywhere in this file.".to_string(),
            published_at: None::<chrono::DateTime<chrono::Utc>>,
        },
    ];
    Article::objects().bulk_create(articles).await?;
    Ok(())
}
