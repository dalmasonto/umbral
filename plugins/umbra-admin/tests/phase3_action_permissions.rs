//! Phase 3 action-permission enforcement tests (gaps2 #79).
//!
//! Verifies that `Action::permission(codename)` is enforced before a
//! bulk-action handler runs:
//!
//! 1. A staff user WITHOUT the required codename gets 403; the action
//!    handler does NOT run (row unchanged).
//! 2. A superuser (is_superuser = true) bypasses the codename check
//!    and the action runs successfully.
//! 3. A staff user WITH the required codename can run the action.
//! 4. An action with NO permission set runs for any staff user.
//!
//! The app boots with `PermissionsPlugin` so the permission tables
//! exist and the check is live (when the plugin isn't installed the
//! check is a no-op; that fallback is already covered by phase3_actions.rs).

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::{Action, ActionInvocation, ActionResult, AdminModel, AdminPlugin, ToastLevel};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_permissions::PermissionsPlugin;
use umbra_sessions::SessionsPlugin;

// ---------------------------------------------------------------------------
// Test model
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct Note {
    id: i64,
    body: String,
    archived: bool,
}

// ---------------------------------------------------------------------------
// Shared boot — one App per process.
// ---------------------------------------------------------------------------

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

/// Codename used by the permission-gated action.
const ARCHIVE_PERM: &str = "myapp.archive_note";

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_action_perms.sqlite");
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

        // An action that requires the `myapp.archive_note` codename.
        let archive_action = Action::new(
            "archive",
            "Archive",
            "archive",
            |inv: ActionInvocation| async move {
                Ok(ActionResult::Toast {
                    message: format!("Archived {} note(s).", inv.ids.len()),
                    level: ToastLevel::Success,
                })
            },
        )
        .permission(ARCHIVE_PERM);

        // A second action with NO permission requirement.
        let tag_action = Action::new(
            "tag",
            "Tag",
            "tag",
            |inv: ActionInvocation| async move {
                Ok(ActionResult::Toast {
                    message: format!("Tagged {} note(s).", inv.ids.len()),
                    level: ToastLevel::Success,
                })
            },
        );

        let note_config = AdminModel::new("note")
            .list_display(&["body", "archived"])
            .actions(vec![archive_action, tag_action]);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(PermissionsPlugin)
            .plugin(AdminPlugin::default().register(note_config))
            .model::<Note>()
            .build()
            .expect("App::build");

        // Run migrations to create all tables: auth_user, session,
        // permissions_*, and note (registered via .model::<Note>() above).
        // Do NOT manually create auth_user or session — migrations own them.
        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbra::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbra::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        // Re-run PermissionsPlugin::on_ready seed now that the tables exist.
        umbra_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");

        let pool = umbra::db::pool();

        // Seed: three notes.
        sqlx::query(
            "INSERT INTO note (body, archived) VALUES \
             ('Note A', 0), ('Note B', 0), ('Note C', 0)",
        )
        .execute(&pool)
        .await
        .expect("seed notes");

        // ---------- users ----------
        // 1. `act_staff`  — staff, NOT superuser, no special permissions.
        let staff = create_user("act_staff", "staff@example.com", "pass123")
            .await
            .expect("create act_staff");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        // 2. `act_privileged` — staff, NOT superuser, HAS the archive perm.
        let privileged = create_user("act_privileged", "priv@example.com", "pass123")
            .await
            .expect("create act_privileged");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(privileged.id)
            .execute(&pool)
            .await
            .expect("set privileged staff");

        // Insert a ContentType row for the custom permission so the FK
        // from permissions_permission.content_type_id is satisfied.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype (app_label, model) \
             VALUES ('myapp', 'note')",
        )
        .execute(&pool)
        .await
        .expect("insert contenttype");
        let ct_id: i64 =
            sqlx::query_scalar(
                "SELECT id FROM permissions_contenttype WHERE app_label = 'myapp' AND model = 'note'",
            )
            .fetch_one(&pool)
            .await
            .expect("fetch contenttype id");

        // Insert the custom permission row with a valid content_type_id.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name) \
             VALUES (?, ?, 'Can archive note')",
        )
        .bind(ARCHIVE_PERM)
        .bind(ct_id)
        .execute(&pool)
        .await
        .expect("insert archive permission row");

        // Grant `act_staff` the model-level change permission so they pass
        // the admin's built-in `permcheck::require(Change)` gate and reach
        // the action-permission check. The test asserts that they are then
        // blocked at the *action* level, not at the model level.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.change_note')",
        )
        .bind(staff.id.to_string())
        .execute(&pool)
        .await
        .expect("grant change_note to staff");

        // Grant `act_privileged` both the model-level change perm and the
        // custom archive perm so they pass both gates.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.change_note')",
        )
        .bind(privileged.id.to_string())
        .execute(&pool)
        .await
        .expect("grant change_note to privileged");

        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, ?)",
        )
        .bind(privileged.id.to_string())
        .bind(ARCHIVE_PERM)
        .execute(&pool)
        .await
        .expect("grant archive perm to privileged");

        // 3. `act_super`  — superuser (is_staff = 1, is_superuser = 1, no explicit perm rows).
        let superuser = create_user("act_super", "super@example.com", "pass123")
            .await
            .expect("create act_super");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(superuser.id)
            .execute(&pool)
            .await
            .expect("set superuser");

        app.into_router()
    })
    .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    let end = after.find('"').unwrap_or(0);
    after[..end].to_string()
}

fn extract_cookie(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login(router: axum::Router, username: &str) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("get login");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon_cookie = extract_cookie(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html);
    let form = serde_urlencoded::to_string([
        ("username", username),
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
                .header(header::COOKIE, format!("umbra_csrf_token={anon_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post login");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie)
        .unwrap_or(anon_cookie)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// (a) Staff user WITHOUT the required codename → 403, action did NOT run.
///
/// This is the primary regression test. Before the fix, this returned
/// 200 and the handler ran; now it must return 403.
#[tokio::test]
async fn action_with_permission_denies_user_without_codename() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "act_staff").await;

    let (status, _headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/note/actions/archive")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "staff user without codename must get 403, not {status}"
    );
}

/// (b) Superuser bypasses the codename check — action runs (200 + toast).
#[tokio::test]
async fn action_with_permission_allows_superuser() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "act_super").await;

    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/note/actions/archive")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "superuser must be allowed to run a permission-gated action"
    );
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "superuser: expected showToast trigger, got: {trigger}"
    );
}

/// (b2) Staff user WITH the required codename — action runs (200 + toast).
#[tokio::test]
async fn action_with_permission_allows_user_with_codename() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "act_privileged").await;

    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/note/actions/archive")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "privileged user with codename must be allowed"
    );
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "privileged user: expected showToast trigger, got: {trigger}"
    );
}

/// (c) Action with NO permission set — any staff user may run it.
#[tokio::test]
async fn action_without_permission_allows_any_staff_user() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    // act_staff has no special permissions at all.
    let session = login(router.clone(), "act_staff").await;

    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/note/actions/tag")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "action with no permission must run for any staff user"
    );
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "no-permission action: expected showToast trigger, got: {trigger}"
    );
}
