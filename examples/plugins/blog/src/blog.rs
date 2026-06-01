// The `#[derive(Model)]` macro emits one `pub const NAME` per field
// under the snake-case sibling module. We use only a couple of them
// here (`ID`, `CREATED_AT`); the others are still part of the public
// surface for downstream consumers but Rust warns dead_code on binary
// crates. Suppress at module level so the example reads cleanly.
#![allow(dead_code)]

//! The `BlogPlugin` — every umbra-rest plugin concern in one place.
//!
//! What this plugin contributes:
//!
//! - **One model** (`Post`) — auto-registered via `Plugin::models()`, so the
//!   `main.rs` builder doesn't need a separate `.model::<Post>()`.
//! - **Two HTTP routes** — `GET /blog` (HTML-ish text), `GET /blog/{id}`
//!   (JSON detail). Both read through the ambient ORM pool.
//! - **REST customisation** — `rest_resource()` returns a `ResourceConfig`
//!   that hides the raw `author_email`, transforms it into a masked
//!   shape on the way out, and adds a computed `summary` field.
//! - **`@action` endpoints** — `POST /api/post/{id}/publish/` flips a
//!   row's `published` flag; `GET /api/post/recent/?limit=N` returns
//!   the N newest posts. Both run under the resource's
//!   `Permission::check`, with `Action::Custom("publish")` /
//!   `Custom("recent")` as the dispatched action.
//! - **A seed-on-boot `on_ready`** — bridges the sync trait method
//!   into async sqlx via `Handle::current().block_on(...)`. Inserts
//!   three rows the first time the binary runs against an empty
//!   database.

use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::web::{Html, Json, Path, Router, StatusCode, get};

use umbra_rest::{ActionError, ActionScope, ResourceConfig};

// =============================================================================
// Model
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, umbra::orm::Model)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub author_email: String,
    /// Boolean flag flipped by the `publish` `@action`.
    pub published: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// =============================================================================
// Plain handlers (the HTML-ish side)
// =============================================================================

async fn list_posts() -> Result<Html<String>, (StatusCode, String)> {
    let posts = Post::objects()
        .order_by(post::ID.desc())
        .limit(50)
        .fetch()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut html = String::from("<h1>Posts</h1><ul>");
    for p in &posts {
        html.push_str(&format!(
            "<li>{} — {} ({})</li>",
            p.id,
            html_escape(&p.title),
            if p.published { "published" } else { "draft" }
        ));
    }
    html.push_str("</ul>");
    html.push_str(r#"<hr><p>JSON: <a href="/api/post/">/api/post/</a> · "#);
    html.push_str(r#"recent: <a href="/api/post/recent/?limit=3">/api/post/recent/?limit=3</a></p>"#);
    Ok(Html(html))
}

async fn post_detail(Path(id): Path<i64>) -> Result<Json<Post>, (StatusCode, String)> {
    match Post::objects().get(post::ID.eq(id)).await {
        Ok(p) => Ok(Json(p)),
        Err(umbra::orm::GetError::NotFound) => {
            Err((StatusCode::NOT_FOUND, format!("no post with id {id}")))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Very small HTML escape so the listing doesn't blow up on `<` in
/// titles. Real apps use `umbra::templates` (autoescape on by default).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// =============================================================================
// REST customisation — bundled with the plugin, not in main.rs
// =============================================================================

/// `RestPlugin::default().resource(blog::rest_resource())` in main.rs
/// picks up everything below. The plugin OWNS the customisation; main.rs
/// just plugs it in.
pub fn rest_resource() -> ResourceConfig {
    ResourceConfig::new("post")
        // Replace the raw author email with the domain only — DRF's
        // `get_email(obj)` pattern.
        .transform("author_email", |v| {
            let s = v.as_str().unwrap_or("");
            match s.split_once('@') {
                Some((_, d)) => json!(format!("***@{d}")),
                None => v.clone(),
            }
        })
        // Add a `summary` field derived from the body's first 120 chars.
        .computed("summary", |row: &Map<String, Value>| {
            let body = row.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let first_line = body.lines().next().unwrap_or("");
            json!(first_line.chars().take(120).collect::<String>())
        })
        // ----- DRF @action — collection-scope --------------------------------
        // GET /api/post/recent/?limit=N → newest N posts.
        .action(
            "recent",
            Method::GET,
            ActionScope::Collection,
            |ctx| async move {
                let limit: u64 = ctx
                    .query
                    .get("limit")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(5);
                let rows = Post::objects()
                    .order_by(post::CREATED_AT.desc())
                    .limit(limit)
                    .fetch()
                    .await
                    .map_err(ActionError::internal)?;
                Ok(json!({ "results": rows, "limit": limit }))
            },
        )
        // ----- DRF @action — detail-scope ------------------------------------
        // POST /api/post/{id}/publish/ → flip `published` to true.
        .action(
            "publish",
            Method::POST,
            ActionScope::Detail,
            |ctx| async move {
                let id: i64 = ctx
                    .pk
                    .as_deref()
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| ActionError::BadInput("bad id".into()))?;

                let mut patch = serde_json::Map::new();
                patch.insert("published".into(), json!(true));

                let affected = Post::objects()
                    .filter(post::ID.eq(id))
                    .update_values(patch)
                    .await
                    .map_err(ActionError::internal)?;
                if affected == 0 {
                    return Err(ActionError::NotFound(format!("no post with id {id}")));
                }
                Ok(json!({ "id": id, "published": true }))
            },
        )
}

// =============================================================================
// The Plugin trait — the contract App::builder() walks at startup
// =============================================================================

pub struct BlogPlugin;

impl Plugin for BlogPlugin {
    fn name(&self) -> &'static str {
        "blog"
    }

    /// Auto-register the plugin's models. `App::builder()` collects
    /// these alongside any `.model::<T>()` registrations and hands
    /// the merged set to the migration engine. The user does NOT
    /// call `.model::<Post>()` separately.
    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<Post>()]
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/blog", get(list_posts))
            .route("/blog/{id}", get(post_detail))
    }

    /// Boot-time hook. Runs after every other plugin has booted and
    /// the ambient pool + model registry are live, but BEFORE the
    /// migration engine has applied schema (that's a separate
    /// `cargo run -- migrate` step). So this is the right place for
    /// in-memory setup (tracing fields, log subscribers, sanity
    /// checks) and the wrong place for `INSERT`s — that's what the
    /// `seed()` helper below is for, called from `main.rs` after
    /// `umbra::migrate::run()`.
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        tracing::info!(plugin = "blog", "BlogPlugin booted; routes mounted at /blog");
        Ok(())
    }
}

/// Seed three rows the first time the binary boots against an empty
/// database. Called from `main.rs` AFTER `umbra::migrate::run()`
/// applies the schema. Real apps put this behind a `seed` management
/// subcommand rather than running it on every start.
///
/// Lives on the plugin module (not in `main.rs`) because it's part
/// of the plugin's "what does it ship with" surface — same way
/// Django's data migrations live in the app, not the project.
pub async fn seed() -> Result<(), Box<dyn std::error::Error>> {
    if Post::objects().count().await? > 0 {
        return Ok(());
    }
    let now = chrono::Utc::now();
    Post::objects()
        .bulk_create(vec![
            Post {
                id: 0,
                title: "Hello, umbra".into(),
                body: "First post on this blog. Look around!".into(),
                author_email: "alice@example.com".into(),
                published: true,
                created_at: now,
            },
            Post {
                id: 0,
                title: "Draft: thoughts on plugins".into(),
                body: "Plugins are apps. Apps are plugins.".into(),
                author_email: "bob@example.com".into(),
                published: false,
                created_at: now,
            },
            Post {
                id: 0,
                title: "Why @action matters".into(),
                body: "CRUD covers most resources, not all. @action is the escape hatch.".into(),
                author_email: "alice@example.com".into(),
                published: true,
                created_at: now,
            },
        ])
        .await?;
    Ok(())
}
