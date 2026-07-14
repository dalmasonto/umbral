//! gaps2 #12 part 2: per-field WriteError rendering in the admin form.
//!
//! When a form submit triggers a DB-level error that can be attributed to a
//! specific column (UNIQUE violation on a known column, structured WriteError
//! with `field_errors()`), the re-rendered form must show that column's
//! message directly beneath its input — not only as a joined string at the
//! top of the page.
//!
//! Asserted shape: the rendered HTML contains the error text AND the input
//! for the offending field nearby (i.e. the `name="<col>"` input appears
//! after `class="field-error"` text that contains the error). We verify
//! both that the per-field error is present *and* that it is adjacent to
//! the field — not only in a top-of-form banner with no field context.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

/// A model with a UNIQUE column so we can force a constraint violation on a
/// specific, known column. `pub` per test conventions (private_interfaces is
/// allowed above but the fields must be visible to sqlx::FromRow).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "write_err_item")]
pub struct WriteErrItem {
    pub id: i64,
    /// UNIQUE — submitting a duplicate triggers a per-field error.
    ///
    /// The attribute is load-bearing and used to be missing: the doc comment claimed
    /// "UNIQUE in the schema" while the MODEL declared no such thing, and only the
    /// hand-written test table carried the constraint. The suite was proving the admin's
    /// per-field UNIQUE-violation rendering against a schema no migration would produce.
    #[umbral(unique)]
    pub slug: String,
    pub title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("write_error_per_field.sqlite");
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
            .expect("sqlite pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .model::<WriteErrItem>()
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();

        // UNIQUE on slug is the constraint we'll violate to get a per-field error.

        // Seed one row so the second create with the same slug triggers UNIQUE.
        sqlx::query("INSERT INTO write_err_item (slug, title) VALUES ('hello', 'Hello')")
            .execute(&pool)
            .await
            .expect("seed first item");

        let staff = create_user("admin_wef", "admin_wef@example.com", "pw")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark as staff");

        app.into_router()
    })
    .await
}

// ── helpers ──────────────────────────────────────────────────────────────────

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

fn extract_csrf_token(html: &str) -> Option<String> {
    let name_marker = r#"name="csrf_token""#;
    let pos = html.find(name_marker)?;
    let window_end = pos.saturating_add(400).min(html.len());
    let window = &html[pos..window_end];
    let value_marker = "value=\"";
    let vstart = window.find(value_marker)? + value_marker.len();
    let vend = window[vstart..].find('"')?;
    Some(window[vstart..vstart + vend].to_string())
}

async fn login_session(router: &axum::Router, username: &str, password: &str) -> String {
    let (_, headers, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let anon_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .map(|(_, v)| v.to_string())
        })
        .expect("GET /admin/login must set a session cookie");
    let csrf_token =
        extract_csrf_token(&body).expect("login page must contain a csrf_token hidden input");

    let form_body = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
        ("csrf_token", &csrf_token),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (_, headers2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form_body))
            .unwrap(),
    )
    .await;
    headers2
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .map(|(_, v)| v.to_string())
        })
        .expect("POST /admin/login must set a session cookie on success")
}

/// Log in as a staff user unique to THIS call.
///
/// The admin persists per-user UI state (gaps2 #11): a bare list visit 303-redirects to
/// the query string that USER last used, and `/admin/` redirects to the path they last
/// visited. Tests asserting the DEFAULT view therefore cannot share a user with a test
/// that filters — whichever runs first decides what the other sees. The state is per-user,
/// so a fresh user per login is where the isolation belongs.
///
/// It never used to matter: these suites never created `admin_user_pref`, so the lookups
/// errored and the restore silently never fired. Deriving the schema from the models
/// created the table and switched a shipped feature on here for the first time.
async fn fresh_staff(router: &axum::Router) -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let username = format!("staff_iso{n}");
    let user = create_user(&username, &format!("{username}@test.com"), "pw")
        .await
        .expect("create staff user");
    sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
        .bind(user.id)
        .execute(&umbral::db::pool())
        .await
        .expect("mark staff");
    login_session(router, &username, "pw").await
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Submitting a duplicate `slug` on the create form must render the UNIQUE
/// violation error adjacent to the `slug` input — not only at the top of
/// the page with no field context.
///
/// The rendered HTML contract (form.html lines 9 and 130):
///   - Per-field error: `<p class="field-error" ...>…already exists…</p>`
///     appears somewhere BEFORE the next `name="slug"` input.
///   - If the fix is absent, the error only appears as a top-of-form
///     `<p style="color:#b8453d">…</p>` with no per-field attribution.
#[tokio::test]
async fn unique_violation_on_create_renders_error_under_slug_field() {
    let router = boot().await.clone();
    let cookie = fresh_staff(&router).await;
    let auth_cookie = format!("umbral_session={cookie}");

    // Submit a row whose slug already exists in the seeded DB.
    let body_str =
        serde_urlencoded::to_string([("slug", "hello"), ("title", "Duplicate")]).unwrap();
    let (status, html) = send(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/write_err_item/new")
            .header(header::COOKIE, &auth_cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body_str))
            .unwrap(),
    )
    .await;

    // The form must re-render (not redirect) on error.
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "duplicate-slug create must return 400; body:\n{html}"
    );

    // The per-field error marker must be present in the rendered HTML.
    assert!(
        html.contains("class=\"field-error\""),
        "expected a field-error element in the form; body:\n{html}"
    );

    // The per-field error text must reference `slug`.
    assert!(
        html.contains("slug") && html.contains("already exists"),
        "expected 'slug' and 'already exists' in per-field error; body:\n{html}"
    );

    // Structural assertion: the `field-error` paragraph must appear BEFORE
    // the next occurrence of `name="slug"` — i.e. it's inside the field's
    // `<div class="field">` block, not only at the top of the page.
    //
    // form.html layout (per-field block):
    //   <div class="field">
    //     <label for="f_slug">slug…</label>
    //     <input … name="slug" …>      ← input rendered first
    //     <p class="field-error" …>…</p>   ← error rendered after input
    //   </div>
    //
    // So the field-error must appear AFTER `name="slug"` and BEFORE the
    // next `<div class="field">` that starts a new column's block.
    let slug_input_pos = html
        .find(r#"name="slug""#)
        .expect("slug input must be in form");
    let field_error_pos = html
        .find("class=\"field-error\"")
        .expect("field-error must be in form after the fix");

    assert!(
        field_error_pos > slug_input_pos,
        "field-error must appear after the slug input (i.e. beneath it), \
         not only at the top; slug_input at {slug_input_pos}, field-error at {field_error_pos}"
    );
}

/// Submitting a duplicate slug on the EDIT form (update handler) must
/// also render the per-field error under the slug input, not only at top.
#[tokio::test]
async fn unique_violation_on_update_renders_error_under_slug_field() {
    let router = boot().await.clone();
    let cookie = fresh_staff(&router).await;
    let auth_cookie = format!("umbral_session={cookie}");

    // First create a second row with slug "world".
    let create_body = serde_urlencoded::to_string([("slug", "world"), ("title", "World")]).unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/write_err_item/new")
                .header(header::COOKIE, &auth_cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "creating 'world' row must succeed"
    );

    // Now try to edit the "world" row to use the slug "hello" (already taken).
    // The seeded row id=1 has slug="hello"; the newly created row has id=2.
    let edit_body =
        serde_urlencoded::to_string([("slug", "hello"), ("title", "World renamed")]).unwrap();
    let (status, html) = send(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/write_err_item/2/edit")
            .header(header::COOKIE, &auth_cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(edit_body))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "duplicate-slug edit must return 400; body:\n{html}"
    );

    assert!(
        html.contains("class=\"field-error\""),
        "expected a field-error element in the edit form; body:\n{html}"
    );

    assert!(
        html.contains("slug") && html.contains("already exists"),
        "expected 'slug' and 'already exists' in per-field error on edit; body:\n{html}"
    );

    // Same structural check as the create test.
    let slug_input_pos = html
        .find(r#"name="slug""#)
        .expect("slug input must be in edit form");
    let field_error_pos = html
        .find("class=\"field-error\"")
        .expect("field-error must be in edit form after the fix");

    assert!(
        field_error_pos > slug_input_pos,
        "field-error must appear after the slug input on edit form; \
         slug_input at {slug_input_pos}, field-error at {field_error_pos}"
    );
}
