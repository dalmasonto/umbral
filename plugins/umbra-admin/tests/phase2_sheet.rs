//! Phase 2 Sheet tests.
//!
//! Covers:
//! 1. GET /admin/{table}/{id}/sheet returns preview sheet fragment.
//! 2. GET /admin/{table}/{id}/edit-sheet returns edit form with field editors.
//! 3. POST /admin/{table}/{id}/edit with valid body updates the row.
//! 4. DELETE /admin/{table}/{id} removes the row.
//! 5. HTMX vs full-page: with hx-request header → fragment; without → redirect.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Note {
    id: i64,
    title: String,
    #[umbra(widget = "markdown")]
    body: String,
    published: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

/// Serialises tests in this file that share the `note` table.
///
/// All tests share the boot'd router and its ambient pool. Some
/// tests read row 1 (preview/edit fragments); others delete or
/// update it. Running parallel races those, so we lock for the
/// duration of every test body.
static NOTE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase2_sheet.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite pool");

        let note_config = AdminModel::new("note")
            .list_display(&["title", "published"])
            .search_fields(&["title", "body"]);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(note_config))
            .model::<Note>()
            .build()
            .expect("App::build");

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
            "CREATE TABLE note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published INTEGER NOT NULL DEFAULT 0\
             )",
        )
        .execute(&pool)
        .await
        .expect("create note");

        let staff = create_user("sheet_admin", "sheet@example.com", "password123")
            .await
            .expect("create staff");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        sqlx::query(
            "INSERT INTO note (title, body, published) VALUES \
             ('Test Note', 'Original body', 1)",
        )
        .execute(&pool)
        .await
        .expect("seed note");

        app.into_router()
    })
    .await
}

async fn send(
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
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

fn opening_tag_for<'a>(html: &'a str, tag_name: &str, needle: &str) -> &'a str {
    let needle_pos = html.find(needle).unwrap_or_else(|| {
        panic!(
            "expected rendered HTML to contain `{needle}`; snippet={}",
            &html[..html.len().min(1200)]
        )
    });
    let tag_start = html[..needle_pos]
        .rfind(&format!("<{tag_name}"))
        .unwrap_or_else(|| panic!("expected `{needle}` to be inside a <{tag_name}> tag"));
    let tag_end = html[needle_pos..]
        .find('>')
        .map(|offset| needle_pos + offset + 1)
        .unwrap_or_else(|| panic!("unterminated <{tag_name}> tag around `{needle}`"));
    &html[tag_start..tag_end]
}

fn rendered_tag_has_attr(tag: &str, attr_name: &str) -> bool {
    tag.trim_matches(|c| c == '<' || c == '>')
        .split_whitespace()
        .skip(1)
        .any(|part| part.split_once('=').map_or(part, |(name, _)| name) == attr_name)
}

async fn login_session(router: axum::Router, username: &str, password: &str) -> String {
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
                .header(header::COOKIE, format!("umbra_csrf_token={anon_cookie}"))
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
async fn test_preview_sheet_htmx_returns_fragment() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/1/sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);

    // Should contain the Preview and Edit toggle buttons
    assert!(
        body.contains("Preview") && body.contains("Edit"),
        "Preview/Edit toggle present: snippet={}",
        &body[..body.len().min(1000)]
    );
    // Should not be a full HTML page
    assert!(!body.contains("<!doctype html>"), "not a full page: {body}");
    // Should contain the record title
    assert!(body.contains("Test Note"), "record data present: {body}");
}

#[tokio::test]
async fn test_edit_sheet_htmx_returns_form_with_editors() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/1/edit-sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);

    // Should contain form inputs (field editors)
    assert!(
        body.contains(r#"<input"#) || body.contains(r#"<textarea"#),
        "form inputs present: snippet={}",
        &body[..body.len().min(1000)]
    );
    // Should contain field name "title"
    assert!(body.contains("title"), "title field present: {body}");
    // Should not be a full HTML page
    assert!(!body.contains("<!doctype html>"), "not a full page: {body}");
}

#[tokio::test]
async fn test_edit_sheet_renders_markdown_body_without_native_required() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/1/edit-sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);

    let tag = opening_tag_for(&body, "textarea", r#"name="body""#);
    assert!(
        tag.contains(r#"data-widget="markdown""#),
        "body textarea should render as markdown widget: {tag}"
    );
    assert!(
        tag.contains(r#"aria-required="true""#),
        "required markdown body should keep aria-required hint: {tag}"
    );
    assert!(
        tag.contains(r#"data-required="true""#),
        "required markdown body should keep non-native data-required hint: {tag}"
    );
    assert!(
        !rendered_tag_has_attr(tag, "required"),
        "enhanced markdown textarea must not render native required because the editor hides it: {tag}"
    );
}

#[tokio::test]
async fn test_edit_sheet_renders_ambient_csrf_input() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/1/edit-sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = umbra::templates::with_current_csrf(
        Some("sheet-csrf-token".to_string()),
        send(router, req),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let tag = opening_tag_for(&body, "input", r#"name="csrf_token""#);
    assert!(
        tag.contains(r#"type="hidden""#),
        "csrf input should render as a hidden input: {tag}"
    );
    assert!(
        tag.contains(r#"value="sheet-csrf-token""#),
        "csrf input should use the ambient token from SecurityPlugin scope: {tag}"
    );
}

#[tokio::test]
async fn test_preview_sheet_without_htmx_redirects() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    // Non-HTMX request to /sheet endpoint
    let req = Request::builder()
        .uri("/admin/note/1/sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(router, req).await;
    // Should redirect to the changelist with ?row= param
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        location.contains("/admin/note/") && location.contains("row="),
        "redirect to changelist with row param: location={location}"
    );
}

#[tokio::test]
async fn test_update_row_via_post() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    // POST to legacy edit endpoint (still works, used by non-HTMX flows)
    let body = "title=Updated+Title&body=Updated+body&published=true";
    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/1/edit")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let (status, headers, _body) = send(router.clone(), req).await;
    // Should redirect to detail page on success
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "redirect after update: status={status}"
    );

    // Verify the row was actually updated by checking the list
    let req2 = Request::builder()
        .uri("/admin/note/rows?search=Updated&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status2, _h, body2) = send(router, req2).await;
    assert_eq!(status2, StatusCode::OK);
    assert!(
        body2.contains("Updated Title"),
        "Updated Title present after update: {body2}"
    );
    let _ = headers;
}

#[tokio::test]
async fn test_delete_row_via_post() {
    // Use the pre-seeded note (id=1, "Test Note") for a simpler delete test.
    // We verify the delete endpoint returns a redirect, then check the note is gone.
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    // First: create a fresh note specifically to delete (won't affect other tests
    // since tests run sequentially with --test-threads=1).
    let create_body = "title=ToDeleteNote&body=CanDelete&published=false";
    let req_create = Request::builder()
        .method("POST")
        .uri("/admin/note/new")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(create_body))
        .unwrap();
    let (create_status, _h, _) = send(router.clone(), req_create).await;
    assert!(
        create_status == StatusCode::SEE_OTHER || create_status == StatusCode::FOUND,
        "create succeeded: status={create_status}"
    );

    // Find the ID via list search
    let req_list = Request::builder()
        .uri("/admin/note/rows?search=ToDeleteNote&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (_, _h2, list_body) = send(router.clone(), req_list).await;
    assert!(
        list_body.contains("ToDeleteNote"),
        "note found before delete: {list_body}"
    );

    // Extract the ID from data-row-id
    let id_marker = "data-row-id=\"";
    let note_id = list_body.find(id_marker).and_then(|pos| {
        let after = &list_body[pos + id_marker.len()..];
        let end = after.find('"')?;
        let id = &after[..end];
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    });

    let note_id = note_id.expect("should find note id in list fragment");
    assert!(
        !note_id.is_empty(),
        "id should be non-empty: got '{note_id}'"
    );

    // Delete via POST /admin/note/{id}/delete
    let req_del = Request::builder()
        .method("POST")
        .uri(format!("/admin/note/{note_id}/delete"))
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (del_status, _h3, _) = send(router.clone(), req_del).await;
    assert!(
        del_status == StatusCode::SEE_OTHER || del_status == StatusCode::FOUND,
        "delete redirected: status={del_status}"
    );

    // Verify deletion via rows endpoint: should show no matching row (empty state).
    // We check there's no data-row-id for the deleted note specifically.
    let req_check = Request::builder()
        .uri("/admin/note/rows?search=ToDeleteNote&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (_, _h4, check_body) = send(router, req_check).await;
    // The fragment will contain "ToDeleteNote" in pagination URL params, but
    // should NOT contain it inside a <span> data cell (actual row content).
    let has_row_with_title = check_body
        .contains("<span class=\"text-on-surface text-body-md tabular-nums\">ToDeleteNote<");
    assert!(
        !has_row_with_title,
        "ToDeleteNote row cell gone after delete. check_body snippet={}",
        &check_body[..check_body.len().min(500)]
    );
}

#[tokio::test]
async fn test_new_sheet_returns_create_form() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/new-sheet")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Should contain the create form structure
    assert!(
        body.contains("create") || body.contains("New"),
        "create form markers present: snippet={}",
        &body[..body.len().min(500)]
    );
    // Should contain form inputs
    assert!(body.contains(r#"<input"#), "form inputs present: {body}");
}

#[tokio::test]
async fn test_confirm_delete_dialog_fragment() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/note/1/_confirm-delete")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _headers, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Should contain Delete and Cancel buttons
    assert!(body.contains("Delete"), "Delete button present: {body}");
    assert!(body.contains("Cancel"), "Cancel button present: {body}");
    // Should not be a full HTML page
    assert!(!body.contains("<!doctype html>"), "not full page: {body}");
}

// =========================================================================
// gaps2 #13 — success-path HX-Trigger pins
//
// Each CRUD success path must emit the `showToast` event alongside
// `closeSheet` + `refreshTable`. Without `showToast` the user gets no
// visible feedback that the action worked; with all three the sheet
// closes, the table refreshes the new row, and a toast confirms.
// =========================================================================

#[tokio::test]
async fn test_sheet_create_success_emits_show_toast_trigger() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let body = "title=ToastCreateNote&body=Hello&published=false";
    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/create")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .body(Body::from(body))
        .unwrap();
    let (status, headers, _body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "HTMX create returns 200");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "create success must fire showToast: {trigger}"
    );
    assert!(
        trigger.contains("closeSheet") && trigger.contains("refreshTable"),
        "create success must also close sheet + refresh table: {trigger}"
    );
    assert!(
        trigger.contains("\"level\":\"success\""),
        "toast level = success: {trigger}"
    );
}

#[tokio::test]
async fn test_sheet_update_success_emits_show_toast_trigger() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let body = "title=ToastUpdateNote&body=ChangedBody&published=true";
    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/1/edit")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .body(Body::from(body))
        .unwrap();
    let (status, headers, _body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "HTMX update returns 200");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "update success must fire showToast: {trigger}"
    );
    assert!(
        trigger.contains("\"level\":\"success\""),
        "toast level = success: {trigger}"
    );
}

#[tokio::test]
async fn test_sheet_delete_success_emits_show_toast_trigger() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    // Create a fresh note specifically to delete so we don't compete
    // with other tests for note id 1.
    let req_create = Request::builder()
        .method("POST")
        .uri("/admin/note/create")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .body(Body::from(
            "title=ToastDeleteNote&body=GoneSoon&published=false",
        ))
        .unwrap();
    send(router.clone(), req_create).await;

    // Find the just-created note's id.
    let req_list = Request::builder()
        .uri("/admin/note/rows?search=ToastDeleteNote&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (_, _h, list_body) = send(router.clone(), req_list).await;
    let id_marker = "data-row-id=\"";
    let note_id = list_body
        .find(id_marker)
        .and_then(|pos| {
            let after = &list_body[pos + id_marker.len()..];
            let end = after.find('"')?;
            Some(after[..end].to_string())
        })
        .filter(|s| !s.is_empty())
        .expect("created note id present in list");

    // The HTMX delete handler is mounted at `DELETE /admin/{table}/{id}`
    // (not the legacy `POST /admin/{table}/{id}/delete` form-POST path,
    // which returns 303 Redirect and predates the HX-Trigger contract).
    let req_delete = Request::builder()
        .method("DELETE")
        .uri(format!("/admin/note/{note_id}"))
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _body) = send(router, req_delete).await;
    assert_eq!(status, StatusCode::OK, "HTMX delete returns 200");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "delete success must fire showToast: {trigger}"
    );
    assert!(
        trigger.contains("\"level\":\"success\""),
        "toast level = success: {trigger}"
    );
}

/// gaps2 #12 Part 2: a sheet-create validation failure renders a PER-FIELD error
/// (next to the offending input), not just a flattened top banner. An empty
/// required `title` (NOT NULL, no default) is the trigger.
#[tokio::test]
async fn test_sheet_create_renders_per_field_error_for_empty_required() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    let body = "title=&body=some+body&published=false";
    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/create")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .body(Body::from(body))
        .unwrap();
    let (status, _h, html) = send(router, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty required field is a 400: {html}");
    assert!(
        html.contains("This field is required."),
        "the per-field required message must render: {html}"
    );
    assert!(
        html.contains("mt-xs text-error"),
        "the per-field error span (below the field, not the top banner) must render: {html}"
    );
}
