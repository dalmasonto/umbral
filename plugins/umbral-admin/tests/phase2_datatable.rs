//! Phase 2 DataTable tests.
//!
//! Covers:
//! 1. Changelist returns rows with `list_display` columns only.
//! 2. `?search=foo` returns just the matching rows.
//! 3. `?sort=title&order=desc` returns rows in reverse title order.
//! 4. `?filter[published]=true` filters rows (new `?filter=field=value` format).
//! 5. `?page=2&page_size=1` returns the second page.
//! 6. The HTMX `hx-target` markup is present in the rendered HTML.
//! 7. `GET /admin/{table}/rows` (HTMX fragment) returns just the tbody content.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
    published: bool,
    created_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase2_dt.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite pool");

        let post_config = AdminModel::new("post")
            .list_display(&["title", "published"])
            .list_filter(&["published"])
            .search_fields(&["title"])
            .ordering(&["title"]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(post_config))
            .model::<Post>()
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
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
                last_login TEXT,\
                email_verified_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user");

        sqlx::query(
            "CREATE TABLE session (\
                id TEXT PRIMARY KEY,\
                user_id TEXT,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create session");

        sqlx::query(
            "CREATE TABLE post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                published INTEGER NOT NULL DEFAULT 0,\
                created_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post");

        let staff = create_user("dt_admin", "dt_admin@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        // Seed posts: Alpha (published=false), Beta (published=true), Gamma (published=true)
        sqlx::query(
            "INSERT INTO post (title, published) VALUES \
             ('Alpha post', 0), ('Beta post', 1), ('Gamma post', 1)",
        )
        .execute(&pool)
        .await
        .expect("seed posts");

        app.into_router()
    })
    .await
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn send_full(
    router: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn extract_csrf(html: &str) -> Option<String> {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker)?;
    let window = &html[pos..pos + 200];
    let val_marker = r#"value=""#;
    let vpos = window.find(val_marker)?;
    let after = &window[vpos + val_marker.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn extract_cookie_value(set_cookie: &str) -> String {
    // e.g. "umbral_session=abc123; Path=/; HttpOnly"
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login_session(router: axum::Router, username: &str, password: &str) -> String {
    // GET login page → session cookie + CSRF
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("login get");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon_cookie = extract_cookie_value(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html).unwrap_or_default();

    // POST credentials
    let form_body = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
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
                .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
                .body(Body::from(form_body))
                .unwrap(),
        )
        .await
        .expect("login post");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie_value)
        .unwrap_or(anon_cookie)
}

#[tokio::test]
async fn test_changelist_list_display_columns_only() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Should show "title" and "published" columns (list_display)
    assert!(body.contains("title"), "title column present: {body}");
    assert!(
        body.contains("published"),
        "published column present: {body}"
    );
    // "created_at" is not in list_display, should not be a column header
    // (may appear in other contexts but not as a table header)
}

/// gaps2 #44: the changelist tbody must carry `data-rows-url` with the
/// server-authoritative rows endpoint, so the post-save `refreshTable`
/// handler re-fetches THAT url instead of string-synthesizing one from
/// `window.location` (which silently 404'd under a custom base path or a
/// different trailing-slash shape — no refresh, no error).
#[tokio::test]
async fn test_changelist_tbody_carries_authoritative_rows_url() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"data-rows-url="/admin/post/rows""#),
        "tbody must expose the authoritative rows URL for the refreshTable handler: {body}"
    );
}

#[tokio::test]
async fn test_changelist_search_returns_matching_rows() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?search=alpha")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.to_lowercase().contains("alpha"),
        "alpha row present: {body}"
    );
    assert!(
        !body.to_lowercase().contains("beta"),
        "beta row absent: {body}"
    );
}

#[tokio::test]
async fn test_changelist_sort_order_desc() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=desc&page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Gamma should appear before Alpha in desc order
    let gamma_pos = body.find("Gamma").unwrap_or(usize::MAX);
    let alpha_pos = body.find("Alpha").unwrap_or(usize::MAX);
    assert!(
        gamma_pos < alpha_pos,
        "Gamma before Alpha in desc order. body snippet: {}",
        &body[..body.len().min(500)]
    );
}

#[tokio::test]
async fn test_changelist_filter_published() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?filter=published=true&page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Beta and Gamma are published=1, Alpha is not.
    // The filter "published=true" uses string match — in SQLite, true=1.
    // The "published" facet values will be "0" and "1".
    // Check we don't get Alpha (published=0):
    assert!(
        !body.contains("Alpha post"),
        "Alpha (unpublished) absent: {body}"
    );
}

#[tokio::test]
async fn test_changelist_pagination() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // page_size=1, page=2 should give Beta (second in alpha order)
    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=asc&page=2&page_size=1")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Beta"), "Beta on page 2: {body}");
    assert!(!body.contains("Alpha"), "Alpha absent on page 2: {body}");
    assert!(!body.contains("Gamma"), "Gamma absent on page 2: {body}");
}

#[tokio::test]
async fn test_changelist_htmx_target_markup_present() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("hx-target=\"#table-body\""),
        "hx-target=\"#table-body\" present in changelist: snippet={}",
        &body[..body.len().min(2000)]
    );
}

/// Bug 1 regression: the rows fragment (HTMX swap target) must include the
/// sticky-right action column with eye / pencil / trash buttons so clicking a
/// sortable column header never makes the actions disappear.
#[tokio::test]
async fn test_rows_fragment_includes_action_column() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=asc&page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // All three action icons must be present in the fragment.
    assert!(
        body.contains("data-lucide=\"eye\""),
        "eye icon present in rows fragment: {body}"
    );
    assert!(
        body.contains("data-lucide=\"pencil\""),
        "pencil icon present in rows fragment: {body}"
    );
    assert!(
        body.contains("data-lucide=\"trash-2\""),
        "trash icon present in rows fragment: {body}"
    );
    // The action <td> must be there even after a sort swap.
    assert!(
        body.contains("sticky right-0"),
        "sticky-right action cell present in rows fragment: {body}"
    );
}

#[tokio::test]
async fn test_rows_htmx_fragment_only() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // HTMX request to /rows endpoint
    let req = Request::builder()
        .uri("/admin/post/rows?page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router.clone(), req).await;
    assert_eq!(status, StatusCode::OK);
    // Fragment should have rows but NOT the full HTML shell
    assert!(!body.contains("<!doctype html>"), "not a full page: {body}");
    assert!(!body.contains("<html"), "not a full page: {body}");

    // Non-HTMX request to the same endpoint REDIRECTS to the
    // changelist with the same query string preserved. The /rows
    // endpoint returns a naked tbody fragment that has no <head>,
    // no fonts, no Tailwind — useless when a user navigates to it
    // directly (back button, bookmark, copy-paste). The changelist
    // page itself HTMX-loads the rows on mount.
    let req2 = Request::builder()
        .uri("/admin/post/rows?page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status2, headers2, _) = send_full(router, req2).await;
    assert!(
        status2 == StatusCode::SEE_OTHER
            || status2 == StatusCode::TEMPORARY_REDIRECT
            || status2 == StatusCode::FOUND,
        "expected redirect status, got: {status2}"
    );
    let location = headers2
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        location.starts_with("/admin/post/"),
        "redirect target should be the changelist; got: {location}"
    );
}

/// Gap #115 regression: removing a filter must update the
/// "Active filters:" chip strip, not just the table rows.
///
/// Before the fix: the chip strip lived OUTSIDE `#table-body`, the
/// HTMX swap target. Clicking the x on a chip refreshed the rows
/// but left the chip strip visible with the just-removed chip
/// still showing. The fix: rows_fragment.html emits an OOB
/// (`hx-swap-oob="outerHTML"`) block targeting `#dt-active-filters-strip`
/// so the strip updates in lock-step with the row swap.
///
/// This test exercises three transitions:
///   1) Initial filter-applied request → strip contains "published"
///      chip + the OOB swap marker.
///   2) Same endpoint with the filter removed → strip wrapper is
///      empty (no chip text, no "Active filters:" label).
///   3) The OOB swap wrapper is ALWAYS rendered (even when empty),
///      so subsequent OOB swaps still have a target.
#[tokio::test]
async fn test_chip_strip_oob_swap_clears_when_filter_removed() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // (1) Filter applied — strip should have the chip + OOB marker.
    let req_with_filter = Request::builder()
        .uri("/admin/post/rows?filter_published=true&page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router.clone(), req_with_filter).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"id="dt-active-filters-strip""#),
        "OOB wrapper must be present so HTMX has a target: {}",
        &body[..body.len().min(2000)]
    );
    assert!(
        body.contains(r#"hx-swap-oob="outerHTML""#),
        "OOB swap directive must be present so the strip updates with the rows: {}",
        &body[..body.len().min(2000)]
    );
    assert!(
        body.contains("Active filters:"),
        "label must be present when at least one filter is active: {}",
        &body[..body.len().min(2000)]
    );
    // CRITICAL: the OOB <div> MUST be inside a <template> tag.
    // The fragment is HTMX-swapped into <tbody>; a bare <div> at
    // that position aborts the browser's table-mode parsing and
    // collapses every subsequent cell into column 1 (the original
    // ship had this bug — empty-state and pagination cramped into
    // one column). The <template> wrap keeps HTMX able to find
    // the OOB by id while telling the parser "this isn't table
    // content."
    let oob_pos = body
        .find(r#"id="dt-active-filters-strip""#)
        .expect("OOB block present");
    let pre_oob = &body[..oob_pos];
    let last_template_open = pre_oob.rfind("<template");
    let last_template_close = pre_oob.rfind("</template>");
    let inside_template = match (last_template_open, last_template_close) {
        (Some(open), Some(close)) => open > close,
        (Some(_), None) => true,
        _ => false,
    };
    assert!(
        inside_template,
        "OOB <div id=\"dt-active-filters-strip\"> MUST be inside a \
         <template> wrapper or the browser strips it from tbody \
         context and the table layout collapses. Body so far:\n{}",
        &body[..body.len().min(800)]
    );

    // (2) Same endpoint with the filter removed — strip should
    //     come back EMPTY (no label, no chip text) but still
    //     present so future OOB swaps land.
    let req_without_filter = Request::builder()
        .uri("/admin/post/rows?page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status2, body2) = send(router.clone(), req_without_filter).await;
    assert_eq!(status2, StatusCode::OK);
    // Wrapper still rendered.
    assert!(
        body2.contains(r#"id="dt-active-filters-strip""#),
        "wrapper must persist even with zero filters so the next OOB swap has a target"
    );
    assert!(
        body2.contains(r#"hx-swap-oob="outerHTML""#),
        "OOB directive must still be present"
    );
    // Label gone — "Active filters:" must NOT appear inside the strip.
    // Find the strip block and assert the label isn't in it.
    let strip_start = body2
        .find(r#"id="dt-active-filters-strip""#)
        .expect("wrapper present");
    // Look at the next ~600 chars (enough for one chip + closing
    // </div>). When the strip is empty, this slice has no "Active
    // filters:" text.
    let strip_window = &body2[strip_start..body2.len().min(strip_start + 600)];
    assert!(
        !strip_window.contains("Active filters:"),
        "with no active filters, the label MUST NOT appear inside the \
         OOB strip — chip-removal regression. Strip window:\n{strip_window}"
    );
}

/// Task 4: The /rows HTMX swap must render the numbered/windowed pagination
/// footer (the same long form as the first load), not the compact `page / total`
/// form that was previously in rows_fragment.html.
#[tokio::test]
async fn test_rows_swap_keeps_numbered_pagination() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // page_size=10 over the 3 seeded posts — only 1 page, but the numbered
    // nav always renders a page-1 button, so the assertion holds.
    let req = Request::builder()
        .uri("/admin/post/rows?page=1&page_size=10")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Numbered nav present: the always-rendered page-1 button.
    assert!(
        body.contains(">1</button>"),
        "numbered page button must render in the /rows swap: {body}"
    );
    // The compact `page / total_pages` span must be gone.
    assert!(
        !body.contains("/ {{ pagination.total_pages }}") && !body.contains("} / {"),
        "compact 'page / total' footer must not appear in the swap: {body}"
    );
}
