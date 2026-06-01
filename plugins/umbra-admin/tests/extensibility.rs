//! Tests for the AdminConfig extensibility surface (gap 18).
//!
//! Covers:
//! 1. `list_display` — filters which columns the list view renders.
//! 2. `list_filter` — filter facets appear in the response HTML.
//! 3. `search_fields` — `?q=` produces a correct WHERE LIKE clause.
//! 4. `ordering` — list rows come back in the configured ORDER BY.
//! 5. Custom action — runs the handler and returns the flash message.
//! 6. `readonly_fields` — form renders `<input ... readonly>` for those fields.
//!
//! Uses the same OnceCell-boot pattern as `tests/integration.rs`.

#![allow(dead_code, private_interfaces)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_admin::{Action, AdminConfig, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};

/// A simple model for these tests.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Article {
    id: i64,
    title: String,
    body: String,
    published: bool,
    created_at: Option<DateTime<Utc>>,
}

// =========================================================================
// Shared boot. Each test clones the router (same Arc'd state underneath).
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir for the test DB");
        let path = tmp.path().join("admin_extensibility.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        // AdminConfig for the `article` table.
        let article_config = AdminConfig::new("article")
            .list_display(&["title", "published"])
            .list_filter(&["published"])
            .search_fields(&["title", "body"])
            .ordering(&["-id"])
            .readonly_fields(&["created_at"])
            .actions(vec![
                Action::delete_selected(),
                Action::new("mark_published", "Mark published", |ids, _ctx| async move {
                    Ok(format!("Marked {} article(s) as published.", ids.len()))
                }),
            ]);

        let admin = AdminPlugin::default().register(article_config);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(admin)
            .model::<Article>()
            .build()
            .expect("App::build with AdminPlugin + AdminConfig");

        // Schema.
        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE auth_user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL UNIQUE,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                is_active INTEGER NOT NULL,\
                is_staff INTEGER NOT NULL,\
                is_superuser INTEGER NOT NULL,\
                date_joined TEXT NOT NULL,\
                last_login TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user");

        sqlx::query(
            "CREATE TABLE article (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published INTEGER NOT NULL DEFAULT 0,\
                created_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create article");

        // Seed a staff user.
        let staff = create_user("admin_ext", "admin_ext@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        // Seed two articles with distinct titles and published flags.
        sqlx::query(
            "INSERT INTO article (title, body, published) VALUES \
             ('Alpha article', 'alpha body text', 0), \
             ('Beta article', 'beta body text', 1)",
        )
        .execute(&pool)
        .await
        .expect("seed articles");

        app.into_router()
    })
    .await
}

fn basic_auth(username: &str, password: &str) -> String {
    let creds = format!("{username}:{password}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(creds);
    format!("Basic {encoded}")
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    (status, body)
}

// =========================================================================
// 1. list_display: only listed columns appear in the list HTML.
// =========================================================================

#[tokio::test]
async fn list_display_filters_columns() {
    let router = boot().await.clone();
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // list_display is ["title", "published"] — those headers should appear.
    assert!(body.contains("title"), "title column missing:\n{body}");
    assert!(
        body.contains("published"),
        "published column missing:\n{body}"
    );
    // "body" and "created_at" are not in list_display, so they should not appear
    // as table headers (they might appear in other contexts though, so we check the
    // th region by looking for the pattern the template uses).
    // The template renders `{% for col in model.fields %}<th>{{ col.name }}</th>`.
    // Since model.fields is filtered to list_display, "body" and "created_at"
    // headers should not appear.
    assert!(
        !body.contains("<th>body</th>"),
        "body column should be hidden:\n{body}"
    );
    assert!(
        !body.contains("<th>created_at</th>"),
        "created_at column should be hidden:\n{body}"
    );
    // Data rows should show article titles.
    assert!(
        body.contains("Alpha article"),
        "Alpha article missing:\n{body}"
    );
}

// =========================================================================
// 2. list_filter: filter facets appear in the sidebar HTML.
// =========================================================================

#[tokio::test]
async fn list_filter_shows_facets_in_sidebar() {
    let router = boot().await.clone();
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The sidebar should show the "published" filter field.
    // The template renders the field name as a header and links like
    // /admin/article/?filter_published=<value>.
    assert!(
        body.contains("filter_published"),
        "list_filter facet links missing:\n{body}"
    );
    // Both distinct values (0 = false, 1 = true rendered as "0"/"1" from SQLite
    // integer storage) should appear as filter links.
    // SQLite stores booleans as 0/1 integers; DISTINCT returns those.
    assert!(
        body.contains("filter_published=0") || body.contains("filter_published=1"),
        "published filter values missing:\n{body}"
    );
}

// =========================================================================
// 3. search_fields: ?q= narrows the list to matching rows.
// =========================================================================

#[tokio::test]
async fn search_fields_filters_rows() {
    let router = boot().await.clone();
    // Search for "alpha" — should match "Alpha article" but not "Beta article".
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/?q=alpha")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    assert!(
        body.contains("Alpha article"),
        "Alpha article should match 'alpha':\n{body}"
    );
    assert!(
        !body.contains("Beta article"),
        "Beta article should not match 'alpha':\n{body}"
    );
}

#[tokio::test]
async fn search_fields_no_match_shows_empty() {
    let router = boot().await.clone();
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/?q=zzznomatch")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    assert!(
        !body.contains("Alpha article"),
        "Alpha article should not appear:\n{body}"
    );
    assert!(
        !body.contains("Beta article"),
        "Beta article should not appear:\n{body}"
    );
}

// =========================================================================
// 4. ordering: list rows appear in configured ORDER BY order.
// =========================================================================

#[tokio::test]
async fn ordering_applies_to_list() {
    let router = boot().await.clone();
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // ordering is ["-id"] — descending by id means Beta (id=2) comes before Alpha (id=1).
    let alpha_pos = body.find("Alpha article").unwrap_or(usize::MAX);
    let beta_pos = body.find("Beta article").unwrap_or(usize::MAX);
    assert!(
        beta_pos < alpha_pos,
        "Beta (id=2) should appear before Alpha (id=1) with ORDER BY id DESC; \
         alpha_pos={alpha_pos}, beta_pos={beta_pos}"
    );
}

// =========================================================================
// 5. Custom action: runs the handler and redirects with flash message.
// =========================================================================

#[tokio::test]
async fn custom_action_runs_and_returns_flash() {
    let router = boot().await.clone();
    let auth = basic_auth("admin_ext", "password123");

    // POST /admin/article/action with action=mark_published and selected PKs.
    let form_body = serde_urlencoded::to_string([
        ("action", "mark_published"),
        ("selected", "1"),
        ("selected", "2"),
    ])
    .unwrap();

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/article/action")
                .header(header::AUTHORIZATION, auth)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
        )
        .await
        .unwrap();

    // The handler redirects to the list with a flash in the query string.
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "action should redirect"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    // Location should contain "flash=" with the handler's message encoded.
    assert!(
        location.contains("flash="),
        "redirect should include flash: {location}"
    );
    assert!(
        location.contains("article"),
        "redirect should point back to the article list: {location}"
    );
}

// =========================================================================
// 6. readonly_fields: form renders <input ... readonly> for those fields.
// =========================================================================

#[tokio::test]
async fn readonly_fields_render_readonly_input() {
    let router = boot().await.clone();
    // GET the edit form for article id=1.
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/1/edit")
            .header(
                header::AUTHORIZATION,
                basic_auth("admin_ext", "password123"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit form body:\n{body}");
    // The form template renders `readonly` on the input when field.readonly is true.
    // created_at is the readonly field.
    // The template pattern: name="{{ field.name }}" ... readonly
    assert!(
        body.contains("readonly"),
        "readonly attribute missing from form:\n{body}"
    );
}

// =========================================================================
// Quiet unused import.
// =========================================================================
#[allow(dead_code)]
fn _unused_pathbuf_marker() -> Option<PathBuf> {
    None
}
