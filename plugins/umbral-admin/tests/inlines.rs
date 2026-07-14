#![allow(dead_code, private_interfaces)]
//! Admin inlines: edit a child model's reverse-FK rows on the parent's
//! change form (add / edit / delete), saved atomically with the parent.
//!
//! Parent `Post`, child `Comment` with `post: ForeignKey<Post>` (the FK
//! column is named `post` — umbral maps a `ForeignKey<T>` field to a
//! column of the field's name, no `_id` suffix) plus a `rating: i64` so
//! a non-numeric submit can force a child write error and prove the
//! transaction rolls back the parent write too.
//!
//! Behaviors covered (not the HTML):
//!   1. Render — the edit form shows the inline section, the child
//!      fields, a row per existing child, plus an `extra` blank row.
//!   2. Create children via the parent POST.
//!   3. Edit an existing child via the parent POST.
//!   4. Delete a child via DELETE checkbox.
//!   5. Atomicity — a valid parent + invalid child rolls BOTH back.
//!   6. No-inline model behaves exactly as before (no regression).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use umbral::orm::ForeignKey;

use umbral_admin::{AdminModel, AdminPlugin, InlineKind, InlineModel};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    #[umbral(string)]
    title: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Comment {
    id: i64,
    post: ForeignKey<Post>,
    text: String,
    rating: i64,
}

/// A standalone model with NO inlines, to prove the no-regression path.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Plain {
    id: i64,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("admin_inlines.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let post_config = AdminModel::new("post").inlines(vec![
            InlineModel::new("comment", "post", &["text", "rating"])
                .kind(InlineKind::Tabular)
                .extra(1),
        ]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(post_config))
            .model::<Post>()
            .model::<Comment>()
            .model::<Plain>()
            .build()
            .expect("build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();

        let staff = create_user("alice", "alice@example.com", "hunter2")
            .await
            .expect("staff");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        app.into_router()
    })
    .await
}

// ---------------------------------------------------------------------------
// Helpers (mirrors integration.rs).
// ---------------------------------------------------------------------------

async fn send_full(
    router: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn cookie_of(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .map(|(_, v)| v.to_string())
        })
}

fn extract_csrf_token(html: &str) -> Option<String> {
    let pos = html.find(r#"name="csrf_token""#)?;
    let window = &html[pos..(pos + 400).min(html.len())];
    let vstart = window.find("value=\"")? + "value=\"".len();
    let vend = window[vstart..].find('"')?;
    Some(window[vstart..vstart + vend].to_string())
}

async fn login(router: &axum::Router) -> String {
    let (status, headers, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login GET");
    let anon = cookie_of(&headers).expect("anon cookie");
    let csrf = extract_csrf_token(&body).expect("csrf");
    let form = serde_urlencoded::to_string([
        ("username", "alice"),
        ("password", "hunter2"),
        ("csrf_token", &csrf),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (s2, h2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;
    assert_eq!(s2, StatusCode::SEE_OTHER, "login POST");
    cookie_of(&h2).expect("session cookie")
}

async fn post_form(
    router: &axum::Router,
    session: &str,
    uri: &str,
    pairs: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let body = serde_urlencoded::to_string(pairs).unwrap();
    send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

async fn get_page(router: &axum::Router, session: &str, uri: &str) -> (StatusCode, String) {
    let (s, _, body) = send_full(
        router.clone(),
        Request::builder()
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, body)
}

/// GET with the `hx-request` header so the sheet handler returns the
/// partial fragment (not a redirect).
async fn get_htmx(router: &axum::Router, session: &str, uri: &str) -> (StatusCode, String) {
    let (s, _, body) = send_full(
        router.clone(),
        Request::builder()
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, body)
}

/// POST a urlencoded form WITH the `hx-request` header — the shape the
/// slide-over sheet submits. Exercises the sheet-aware code paths
/// (success HX-Trigger, sheet-fragment error re-render).
async fn post_form_htmx(
    router: &axum::Router,
    session: &str,
    uri: &str,
    pairs: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let body = serde_urlencoded::to_string(pairs).unwrap();
    send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("hx-request", "true")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

async fn count(table: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&pool)
        .await
        .unwrap()
}

async fn seed_post(title: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO post (title) VALUES (?)")
        .bind(title)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

async fn seed_comment(post_id: i64, text: &str, rating: i64) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO comment (post, text, rating) VALUES (?, ?, ?)")
        .bind(post_id)
        .bind(text)
        .bind(rating)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_form_renders_inline_section_with_existing_and_blank_rows() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("Renderable").await;
    seed_comment(post_id, "first child", 5).await;

    let (status, html) = get_page(&router, &session, &format!("/admin/post/{post_id}/edit")).await;
    assert_eq!(status, StatusCode::OK, "edit form should 200:\n{html}");

    // Inline section + child fields present.
    assert!(
        html.contains("inline-comment-TOTAL"),
        "inline management count missing:\n{html}"
    );
    // One existing child row prefilled with its value + one extra blank.
    assert!(
        html.contains("first child"),
        "existing child value not prefilled:\n{html}"
    );
    assert!(
        html.contains(r#"name="inline-comment-0-id""#)
            && html.contains(r#"name="inline-comment-0-text""#),
        "row 0 field names missing:\n{html}"
    );
    // Existing (row 0) + 1 extra (row 1) => TOTAL=2.
    assert!(
        html.contains(r#"name="inline-comment-TOTAL" value="2""#),
        "expected TOTAL=2 (1 existing + 1 extra):\n{html}"
    );
}

#[tokio::test]
async fn parent_post_creates_new_children() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("AddChildren").await;
    let before = count("comment").await;

    let (status, _h, body) = post_form(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "AddChildren"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", ""),
            ("inline-comment-0-text", "brand new"),
            ("inline-comment-0-rating", "3"),
        ],
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "parent edit should succeed, got {status}:\n{body}"
    );

    assert_eq!(count("comment").await, before + 1, "one child added");
    let pool = umbral::db::pool();
    let (text, fk): (String, i64) =
        sqlx::query_as("SELECT text, post FROM comment WHERE text = 'brand new'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(text, "brand new");
    assert_eq!(fk, post_id, "FK set to parent automatically");
}

#[tokio::test]
async fn parent_post_edits_existing_child() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("EditChild").await;
    let child_id = seed_comment(post_id, "old text", 1).await;

    let (status, _h, body) = post_form(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "EditChild"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", &child_id.to_string()),
            ("inline-comment-0-text", "new text"),
            ("inline-comment-0-rating", "9"),
        ],
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "edit should succeed: {status}\n{body}"
    );

    let pool = umbral::db::pool();
    let (text, rating): (String, i64) =
        sqlx::query_as("SELECT text, rating FROM comment WHERE id = ?")
            .bind(child_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(text, "new text", "child text updated");
    assert_eq!(rating, 9, "child rating updated");
}

#[tokio::test]
async fn parent_post_deletes_child_via_checkbox() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("DeleteChild").await;
    let child_id = seed_comment(post_id, "doomed", 2).await;
    let before = count("comment").await;

    let (status, _h, body) = post_form(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "DeleteChild"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", &child_id.to_string()),
            ("inline-comment-0-text", "doomed"),
            ("inline-comment-0-rating", "2"),
            ("inline-comment-0-DELETE", "on"),
        ],
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "delete should succeed: {status}\n{body}"
    );

    assert_eq!(count("comment").await, before - 1, "child removed");
    let pool = umbral::db::pool();
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM comment WHERE id = ?")
        .bind(child_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(exists.is_none(), "child row gone");
}

#[tokio::test]
async fn invalid_child_rolls_back_parent_and_children() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("Original Title").await;

    // Parent is valid (new title) but the child carries a non-numeric
    // rating, which fails `rating: i64` validation in the child INSERT.
    // The whole transaction must roll back: the parent title must NOT
    // change AND no child may be written.
    let before_comments = count("comment").await;
    let (status, _h, body) = post_form(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "Changed Title"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", ""),
            ("inline-comment-0-text", "should not persist"),
            ("inline-comment-0-rating", "not-a-number"),
        ],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid child should re-render with an error, not redirect:\n{body}"
    );

    let pool = umbral::db::pool();
    // Parent title rolled back.
    let title: String = sqlx::query_scalar("SELECT title FROM post WHERE id = ?")
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        title, "Original Title",
        "parent title must roll back, not persist 'Changed Title'"
    );
    // No child written.
    assert_eq!(
        count("comment").await,
        before_comments,
        "no child may persist when the transaction rolled back"
    );
    let leaked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM comment WHERE text = 'should not persist'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(leaked.is_none(), "the invalid child row leaked");
}

#[tokio::test]
async fn model_without_inlines_has_no_regression() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    // The `plain` model has no inlines configured: its create form must
    // render without any inline section and a plain create must work.
    let (status, html) = get_page(&router, &session, "/admin/plain/new").await;
    assert_eq!(status, StatusCode::OK, "plain new form 200:\n{html}");
    assert!(
        !html.contains("inline-group") && !html.contains("-TOTAL"),
        "no inline formset markup on a model without inlines:\n{html}"
    );

    let before = count("plain").await;
    let (s, _h, body) = post_form(
        &router,
        &session,
        "/admin/plain/new",
        &[("name", "no-inlines")],
    )
    .await;
    assert!(
        s == StatusCode::SEE_OTHER || s == StatusCode::OK,
        "plain create should succeed: {s}\n{body}"
    );
    assert_eq!(count("plain").await, before + 1, "plain row created");
}

// ---------------------------------------------------------------------------
// Sheet (HTMX slide-over) inline tests. The sheet edit form posts to the
// SAME `crud::update` handler the full-page form does, and the sheet
// create form posts to `sheet::sheet_create` — both now save inlines
// atomically through the shared path. These prove the inline section
// renders in the sheet partial and the save round-trips identically.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_sheet_renders_inline_section_with_existing_and_blank_rows() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("SheetRenderable").await;
    seed_comment(post_id, "sheet child", 5).await;

    let (status, html) = get_htmx(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit-sheet"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit-sheet should 200:\n{html}");

    // Same inline assertions as the full-page render test, against the
    // sheet partial.
    assert!(
        html.contains("inline-comment-TOTAL"),
        "inline management count missing in sheet:\n{html}"
    );
    assert!(
        html.contains("sheet child"),
        "existing child value not prefilled in sheet:\n{html}"
    );
    assert!(
        html.contains(r#"name="inline-comment-0-id""#)
            && html.contains(r#"name="inline-comment-0-text""#),
        "row 0 field names missing in sheet:\n{html}"
    );
    // 1 existing + 1 extra blank => TOTAL=2.
    assert!(
        html.contains(r#"name="inline-comment-TOTAL" value="2""#),
        "expected TOTAL=2 (1 existing + 1 extra) in sheet:\n{html}"
    );
}

#[tokio::test]
async fn new_sheet_renders_blank_inline_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let (status, html) = get_htmx(&router, &session, "/admin/post/new-sheet").await;
    assert_eq!(status, StatusCode::OK, "new-sheet should 200:\n{html}");
    assert!(
        html.contains("inline-comment-TOTAL"),
        "inline section missing on create sheet:\n{html}"
    );
    // No parent yet → only the `extra=1` blank row.
    assert!(
        html.contains(r#"name="inline-comment-TOTAL" value="1""#),
        "expected TOTAL=1 (only the blank extra) on create sheet:\n{html}"
    );
}

#[tokio::test]
async fn sheet_create_creates_children() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let before = count("comment").await;
    let before_posts = count("post").await;

    // The sheet create form posts to /admin/{table}/create.
    let (status, _h, body) = post_form_htmx(
        &router,
        &session,
        "/admin/post/create",
        &[
            ("title", "SheetCreated"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", ""),
            ("inline-comment-0-text", "child via sheet create"),
            ("inline-comment-0-rating", "7"),
        ],
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "sheet create should succeed, got {status}:\n{body}"
    );
    assert_eq!(count("post").await, before_posts + 1, "parent created");
    assert_eq!(count("comment").await, before + 1, "one child created");

    let pool = umbral::db::pool();
    let (text, fk): (String, i64) = sqlx::query_as(
        "SELECT c.text, c.post FROM comment c JOIN post p ON p.id = c.post \
         WHERE p.title = 'SheetCreated'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(text, "child via sheet create");
    let parent: i64 = sqlx::query_scalar("SELECT id FROM post WHERE title = 'SheetCreated'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(fk, parent, "child FK set to the new parent automatically");
}

#[tokio::test]
async fn sheet_edit_updates_and_deletes_children() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("SheetEdit").await;
    let keep_id = seed_comment(post_id, "keep me old", 1).await;
    let doomed_id = seed_comment(post_id, "delete me", 2).await;
    let before = count("comment").await;

    // Via the sheet edit POST (same /edit endpoint, hx-request): update
    // one existing child and delete the other.
    let keep = keep_id.to_string();
    let doomed = doomed_id.to_string();
    let (status, _h, body) = post_form_htmx(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "SheetEdit"),
            ("inline-comment-TOTAL", "2"),
            ("inline-comment-0-id", &keep),
            ("inline-comment-0-text", "keep me new"),
            ("inline-comment-0-rating", "8"),
            ("inline-comment-1-id", &doomed),
            ("inline-comment-1-text", "delete me"),
            ("inline-comment-1-rating", "2"),
            ("inline-comment-1-DELETE", "on"),
        ],
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "sheet edit should succeed, got {status}:\n{body}"
    );

    assert_eq!(count("comment").await, before - 1, "one child deleted");
    let pool = umbral::db::pool();
    let (text, rating): (String, i64) =
        sqlx::query_as("SELECT text, rating FROM comment WHERE id = ?")
            .bind(keep_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(text, "keep me new", "kept child updated via sheet");
    assert_eq!(rating, 8, "kept child rating updated via sheet");
    let gone: Option<i64> = sqlx::query_scalar("SELECT id FROM comment WHERE id = ?")
        .bind(doomed_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(gone.is_none(), "doomed child removed via sheet");
}

#[tokio::test]
async fn sheet_edit_invalid_child_rolls_back_and_rerenders_sheet() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(&router).await;

    let post_id = seed_post("SheetRollbackOriginal").await;
    let before_comments = count("comment").await;

    // Valid parent change + invalid child rating → the whole tx rolls
    // back, AND the error re-render is the SHEET fragment (not a full
    // page), carrying the inline section + the submitted values.
    let (status, _h, body) = post_form_htmx(
        &router,
        &session,
        &format!("/admin/post/{post_id}/edit"),
        &[
            ("title", "SheetRollbackChanged"),
            ("inline-comment-TOTAL", "1"),
            ("inline-comment-0-id", ""),
            ("inline-comment-0-text", "must not persist via sheet"),
            ("inline-comment-0-rating", "not-a-number"),
        ],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid child should re-render the sheet with an error:\n{body}"
    );
    // Sheet-aware re-render: the fragment is the sheet panel (with the
    // inline formset), NOT the full-page form (no <html>/breadcrumb).
    assert!(
        body.contains("umbral-sheet-panel") && body.contains("inline-comment-TOTAL"),
        "error re-render must be the sheet fragment with inlines:\n{body}"
    );
    assert!(
        body.contains("must not persist via sheet"),
        "submitted inline value must be repopulated in the sheet:\n{body}"
    );

    let pool = umbral::db::pool();
    let title: String = sqlx::query_scalar("SELECT title FROM post WHERE id = ?")
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        title, "SheetRollbackOriginal",
        "parent title must roll back on the sheet path too"
    );
    assert_eq!(
        count("comment").await,
        before_comments,
        "no child may persist when the sheet submit rolled back"
    );
}
