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
use umbra_auth::AuthUser;

/// A closed-set enum used as a model field via `#[umbra(choices)]`.
///
/// `#[derive(Choices)]` emits the trait impls + the sqlx Type / Encode /
/// Decode pair so `Article::create(.. status: ArticleStatus::Draft ..)`
/// round-trips through the database as `'draft'`. The admin renders a
/// `<select>` with the variant labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ArticleStatus {
    Draft,
    Review,
    Published,
    Archived,
}

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
    #[umbra(string, max_length = 50)]
    pub title: String,
    pub body: String,
    #[umbra(choices, default = "draft")]
    pub status: ArticleStatus,
    #[umbra(noedit)]
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        .plugin(umbra_auth::AuthPlugin::<AuthUser>::default())
        .plugin(umbra_sessions::SessionsPlugin::default())
        // Register Article with the admin so the datatable renders a
        // search box (search_fields driven) and the list view shows the
        // columns we actually care about. Without `.register(...)` the
        // admin falls back to auto-discovery — every column listed, no
        // search input, no per-column tweaks.
        .plugin(
            umbra_admin::AdminPlugin::default()
                .register(
                    umbra_admin::AdminModel::new("article")
                        .label("Articles")
                        .icon("newspaper")
                        .list_display(&["id", "title", "status", "published_at"])
                        .search_fields(&["title", "body"])
                        .ordering(&["-published_at", "id"])
                        // Double-click any of these cells in the list view to
                        // edit it inline — no sheet, no extra round-trip.
                        .inline_edit_fields(&["title", "status"]),
                )
                // Surface the AuthUser model and turn on the "Change password"
                // affordance on its edit sheet. The route + handler are always
                // wired (in `umbra-admin`); `password_field` is what makes the
                // button render in the footer.
                .register(
                    umbra_admin::AdminModel::new("auth_user")
                        .label("Users")
                        .icon("users")
                        .password_field("password_hash"),
                ),
        )
        .plugin(umbra_permissions::PermissionsPlugin)
        .router(
            Router::new()
                .route("/", get(home))
                .route("/500", get(test_500_html))
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

    // Auto-migrate on startup — demo-only convenience. Skipped when
    // the user explicitly runs a non-serve CLI subcommand, so that
    // `cargo run -- makemigrations` / `migrate` / `inspectdb` /
    // `createsuperuser` etc. drive the migration flow themselves
    // without auto-apply stepping on the inputs. Production
    // deployments split migrate from request-serving regardless:
    // `cargo run -- makemigrations`, then `cargo run -- migrate`,
    // then `cargo run -- serve`.
    if is_serve_invocation() {
        auto_migrate().await?;
        seed_article_rows().await?;
    }

    // Hand argv to the CLI dispatcher. With no subcommand it serves on
    // `settings.bind_addr`; with a subcommand like `createsuperuser`
    // (contributed by umbra-auth via Plugin::commands()), `migrate`,
    // `makemigrations`, etc., it routes to the matching handler.
    // Calling `app.serve(...)` directly would bypass the dispatcher
    // and ignore argv entirely.
    umbra_cli::dispatch(app).await?;
    Ok(())
}

/// True when argv has no subcommand (`cargo run` / `cargo run --`) or
/// the subcommand is `serve`. False for `makemigrations`, `migrate`,
/// `inspectdb`, `createsuperuser`, etc. — those drive the migration
/// flow themselves and would conflict with eager auto-migrate.
fn is_serve_invocation() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        None | Some("serve")
    )
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

async fn test_500_html() -> Result<Html<String>, (StatusCode, String)> {
    // let body = umbra::templates::render("test-500.html", &context!()).map_err(internal_error)?;
    panic!("Something went south ofcourse");
    // Ok(Html(body))
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

async fn auto_migrate() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match umbra::migrate::make().await {
        Ok(paths) => {
            for path in paths {
                eprintln!("auto-migrate: wrote {}", path.display());
            }
        }
        Err(MigrateError::NoChanges) => {}
        Err(err) => return Err(Box::new(err)),
    }
    // Self-heal the "DB exists from before umbra started tracking it"
    // case: for every plugin whose first migration's tables are already
    // present in the DB, record that migration as applied without
    // running its CREATE TABLE. Idempotent — does nothing on a fresh DB
    // or one that's already in sync. This is the same recovery path the
    // CLI exposes as `migrate --fake-initial`.
    let faked = umbra::migrate::fake_initial().await?;
    if faked > 0 {
        eprintln!("auto-migrate: fake-applied initial migration for {faked} plugin(s)");
    }
    let n = umbra::migrate::run().await?;
    if n > 0 {
        eprintln!("auto-migrate: applied {n} migration(s)");
    }
    Ok(())
}

async fn seed_article_rows() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Idempotent seed via `Manager::get_or_create`: filter by a unique
    // field (the title here), insert if missing, do nothing on a hit.
    // Replaces the old count-and-bulk_create dance, which would skip
    // the seed entirely if the first row had been hand-deleted but
    // others survived. With get_or_create each row stands alone.
    let seeds = [
        Article {
            id: 0,
            title: "Deriving Model".to_string(),
            body: "Article::objects().fetch() returned this row.".to_string(),
            status: ArticleStatus::Published,
            published_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-05-30T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        },
        Article {
            id: 0,
            title: "User-defined struct".to_string(),
            body: "No hand-written impl Model anywhere in this file.".to_string(),
            status: ArticleStatus::Draft,
            published_at: None,
        },
    ];
    for article in seeds {
        let title = article.title.clone();
        Article::objects()
            .get_or_create(article::TITLE.eq(&title), article)
            .await?;
    }
    Ok(())
}
