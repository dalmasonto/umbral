//! gaps2 #72 — M2M form option fetch is bounded (was full-table).
//!
//! Verifies two properties:
//!
//!   1. When the target table has more rows than `M2M_OPTION_CAP` (200),
//!      the edit form renders a bounded set of options (≤ cap + selected
//!      extras), NOT every row in the table.
//!
//!   2. A currently-selected item that falls beyond the cap window still
//!      appears in the form as a pre-checked option — it is NOT silently
//!      dropped.
//!
//! Strategy: seed 210 target rows (> cap of 200).  Link parent row #1
//! to target row #210 (the last, beyond the cap).  GET the edit form
//! and assert:
//!   - The HTML contains `<input type="checkbox"` entries, but well
//!     under 210 (proving the cap fires).
//!   - Target row #210 IS present and marked checked (proving selected-
//!     beyond-cap items are backfilled).

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use umbral::orm::M2M;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// =========================================================================
// Test models
// =========================================================================

/// Target model — many of these will be seeded.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "m2mb_item")]
pub struct Item {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

/// Parent model — holds an M2M relation to Item.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "m2mb_group")]
pub struct Group {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    /// Junction table will be `m2mb_group_items`.
    #[umbral(m2m = "m2mb_item")]
    pub items: M2M<Item>,
}

// =========================================================================
// Boot & helpers
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("m2m_bounded.sqlite");
        std::mem::forget(tmp);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .model::<Item>()
            .model::<Group>()
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();

        // Auth / session tables (no migrate runner in test harness).
        sqlx::query(
            "CREATE TABLE auth_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL,
                password_hash TEXT NOT NULL,
                is_active INTEGER NOT NULL,
                is_staff INTEGER NOT NULL,
                is_superuser INTEGER NOT NULL,
                date_joined TEXT NOT NULL,
                last_login TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("auth_user");

        sqlx::query(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                data TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("session");

        // Domain tables.
        sqlx::query(
            "CREATE TABLE m2mb_item (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("m2mb_item");

        sqlx::query(
            "CREATE TABLE m2mb_group (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("m2mb_group");

        // Junction table uses the framework's `parent_id` / `child_id`
        // column names (see crates/umbral-core/src/orm/m2m.rs).
        sqlx::query(
            "CREATE TABLE m2mb_group_items (
                parent_id INTEGER NOT NULL REFERENCES m2mb_group(id),
                child_id  INTEGER NOT NULL REFERENCES m2mb_item(id),
                PRIMARY KEY (parent_id, child_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("m2mb_group_items");

        // Seed 210 items — more than the M2M_OPTION_CAP of 200.
        for i in 1..=210i64 {
            sqlx::query("INSERT INTO m2mb_item (name) VALUES (?)")
                .bind(format!("item-{i:04}"))
                .execute(&pool)
                .await
                .expect("seed item");
        }

        // Seed one group.
        sqlx::query("INSERT INTO m2mb_group (title) VALUES ('test-group')")
            .execute(&pool)
            .await
            .expect("seed group");

        // Link group #1 → item #210 (the last item, beyond the cap window).
        sqlx::query(
            "INSERT INTO m2mb_group_items (parent_id, child_id) VALUES (1, 210)",
        )
        .execute(&pool)
        .await
        .expect("seed junction");

        // Staff user.
        let staff = create_user("m2m_admin", "m2m@example.com", "pass123")
            .await
            .expect("create user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        app.into_router()
    })
    .await
}

async fn body_of(resp: axum::response::Response) -> String {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    after[..after.find('"').unwrap_or(0)].to_string()
}

async fn login_session(router: axum::Router) -> String {
    // GET the login page to get a CSRF token cookie.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("GET /admin/login");

    let csrf_cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_csrf_token" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("csrf cookie from GET /admin/login");

    let html = body_of(resp).await;
    let csrf = extract_csrf(&html);

    let form = serde_urlencoded::to_string([
        ("username", "m2m_admin"),
        ("password", "pass123"),
        ("csrf_token", csrf.as_str()),
        ("next", "/admin/"),
    ])
    .unwrap();

    let resp2 = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, format!("umbral_csrf_token={csrf_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("POST /admin/login");

    resp2
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_session" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

// =========================================================================
// Tests
// =========================================================================

/// The edit form for a Group with 210 Items in the target table must
/// render a bounded set of checkboxes (≤ cap + one extra for the
/// selected-beyond-cap item), not all 210 rows.
#[tokio::test]
async fn test_m2m_option_fetch_is_bounded() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone()).await;

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/m2mb_group/1/edit")
                .header(header::COOKIE, format!("umbral_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("GET edit");

    let status = resp.status();
    let body = body_of(resp).await;

    assert_eq!(status, StatusCode::OK, "edit form status:\n{body}");

    // Count checkbox inputs inside the M2M section.  The form renders
    // one <input type="checkbox" per candidate.
    let checkbox_count = body
        .matches(r#"name="m2m_items""#)
        .count();

    // With 210 target rows and cap=200, we expect at most 201 checkboxes:
    // 200 from the bounded fetch + 1 extra (item-0210, the selected one
    // that lives beyond the cap).  Strictly less than 210 proves the cap
    // fired.
    assert!(
        checkbox_count < 210,
        "expected <210 checkboxes (cap={cap}) but got {checkbox_count};\
         the full-table fetch was not bounded",
        cap = 200,
    );

    // The selected item (item-0210) must appear in the HTML regardless
    // of whether it fell inside or outside the initial cap window.
    assert!(
        body.contains("item-0210"),
        "item-0210 (selected, beyond cap) is missing from the edit form;\
         selected-beyond-cap backfill did not fire"
    );
}

/// A currently-selected item that sits beyond the cap still renders
/// with its checkbox pre-checked (`checked` attribute present alongside
/// the item's value in the form).
#[tokio::test]
async fn test_m2m_selected_beyond_cap_is_pre_checked() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone()).await;

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/m2mb_group/1/edit")
                .header(header::COOKIE, format!("umbral_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("GET edit");

    let body = body_of(resp).await;

    // The template emits something like:
    //   <input type="checkbox" name="m2m_items" value="210" checked />
    // We look for value="210" (the PK, not the label "item-0210") and
    // then verify "checked" appears within the same tag (~120 chars).
    let needle = r#"value="210""#;
    let Some(pos) = body.find(needle) else {
        panic!(
            "checkbox with value=\"210\" not found in form body;\
             (item-0210 present: {})",
            body.contains("item-0210")
        );
    };
    let window_end = (pos + 200).min(body.len());
    let window = &body[pos..window_end];
    assert!(
        window.contains("checked"),
        "item-0210 (selected beyond cap) does not appear as checked in the form;\
         window from value=\"210\": {window:?}"
    );
}
