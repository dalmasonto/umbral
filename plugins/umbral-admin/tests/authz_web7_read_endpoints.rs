//! WEB-7 authorization gate on the admin's cross-model READ endpoints.
//!
//! The FK-picker and bulk-action handlers were patched to enforce the
//! per-model `view_<model>` permission ("WEB-7"), but three read endpoints
//! were missed and only gated on `require_staff`:
//!
//!   * `GET /admin/api/palette/search` — ⌘K global record search across
//!     EVERY registered model (returns row labels + PKs).
//!   * `GET /admin/{table}/filter-dialog` — builds facets from distinct
//!     column values.
//!   * `GET /admin/{table}/{id}/history` — the object's audit trail.
//!
//! This suite proves a staff user WITHOUT `view_<model>` can no longer read
//! that model through any of the three, while a privileged user / superuser
//! still can. Mirrors the setup in `sidebar_perm_gate.rs`: bare table names so
//! `table_app_label` → "app", manual permission + grant rows keyed on the
//! exact codenames `permcheck::codename` produces.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_permissions::PermissionsPlugin;
use umbral_sessions::SessionsPlugin;

/// A model the restricted user may NOT view. Its `data` column carries a
/// distinctive marker so we can assert on its presence/absence in search.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "wsecret")]
pub struct WSecret {
    pub id: i64,
    pub data: String,
}

/// A model the restricted user MAY view.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "wpublic")]
pub struct WPublic {
    pub id: i64,
    pub name: String,
}

const SECRET_MARKER: &str = "zzsecretmarkerzz";
const PUBLIC_MARKER: &str = "zzpublicmarkerzz";

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("authz_web7.sqlite");
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
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(PermissionsPlugin)
            .plugin(AdminPlugin::default())
            .model::<WSecret>()
            .model::<WPublic>()
            .build()
            .expect("App::build");

        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbral::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbral::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        umbral_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");

        let pool = umbral::db::pool();

        // Seed one row per model with a distinctive searchable marker.
        sqlx::query("INSERT INTO wsecret (data) VALUES (?)")
            .bind(SECRET_MARKER)
            .execute(&pool)
            .await
            .expect("insert wsecret row");
        sqlx::query("INSERT INTO wpublic (name) VALUES (?)")
            .bind(PUBLIC_MARKER)
            .execute(&pool)
            .await
            .expect("insert wpublic row");

        // ContentType + Permission rows keyed on the exact codenames
        // `permcheck::codename` uses: "app.view_wsecret" / "app.view_wpublic".
        for (model, codename) in [("wsecret", "app.view_wsecret"), ("wpublic", "app.view_wpublic")]
        {
            sqlx::query(
                "INSERT OR IGNORE INTO permissions_contenttype (app_label, model) VALUES ('app', ?)",
            )
            .bind(model)
            .execute(&pool)
            .await
            .expect("insert content type");
            let ct_id: i64 = sqlx::query_scalar(
                "SELECT id FROM permissions_contenttype WHERE app_label = 'app' AND model = ?",
            )
            .bind(model)
            .fetch_one(&pool)
            .await
            .expect("fetch content type id");
            sqlx::query(
                "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name) \
                 VALUES (?, ?, 'view')",
            )
            .bind(codename)
            .bind(ct_id)
            .execute(&pool)
            .await
            .expect("insert permission");
        }

        // `restricted` — staff, only `view_wpublic`.
        let restricted = create_user("web7_restricted", "r@example.com", "pass123")
            .await
            .expect("create restricted");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(restricted.id)
            .execute(&pool)
            .await
            .expect("set staff");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.view_wpublic')",
        )
        .bind(restricted.id.to_string())
        .execute(&pool)
        .await
        .expect("grant view_wpublic");

        // `privileged` — staff, both view permissions.
        let privileged = create_user("web7_privileged", "p@example.com", "pass123")
            .await
            .expect("create privileged");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(privileged.id)
            .execute(&pool)
            .await
            .expect("set staff");
        for perm in ["app.view_wpublic", "app.view_wsecret"] {
            sqlx::query(
                "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
                 VALUES (?, ?)",
            )
            .bind(privileged.id.to_string())
            .bind(perm)
            .execute(&pool)
            .await
            .expect("grant privileged perm");
        }

        // `superuser` — bypasses the perm check entirely.
        let superuser = create_user("web7_super", "s@example.com", "pass123")
            .await
            .expect("create super");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(superuser.id)
            .execute(&pool)
            .await
            .expect("set superuser");

        app.into_router()
    })
    .await
}

async fn cookie_for(username: &str) -> String {
    // Ensure the app (and thus the ambient DB pool + seeded users) is built.
    boot().await;
    let pool = umbral::db::pool();
    let user: AuthUser = sqlx::query_as(
        "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, \
         date_joined, last_login, email_verified_at FROM auth_user WHERE username = ?",
    )
    .bind(username)
    .fetch_one(&pool)
    .await
    .expect("lookup user");
    let tok = umbral_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    format!("umbral_session={tok}")
}

async fn get(uri: &str, cookie: &str) -> (StatusCode, String) {
    let router = boot().await.clone();
    let resp = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

// ---------------------------------------------------------------------------
// palette_search (#2)
// ---------------------------------------------------------------------------

/// A restricted staff user's ⌘K search must NOT return rows from a model they
/// can't view, while still returning rows from a model they can. This is the
/// core cross-model disclosure the WEB-7 gate closes.
#[tokio::test]
async fn palette_search_excludes_unviewable_model_for_restricted() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_restricted").await;
    let (status, body) = get("/admin/api/palette/search?q=zz", &cookie).await;
    assert_eq!(status, StatusCode::OK, "palette search should 200");
    assert!(
        !body.contains(SECRET_MARKER),
        "restricted user must NOT see wsecret rows in palette search; body: {body}"
    );
    assert!(
        body.contains(PUBLIC_MARKER),
        "restricted user SHOULD see wpublic rows they can view; body: {body}"
    );
}

/// A privileged user with both view perms sees rows from both models.
#[tokio::test]
async fn palette_search_includes_both_models_for_privileged() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_privileged").await;
    let (status, body) = get("/admin/api/palette/search?q=zz", &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(SECRET_MARKER) && body.contains(PUBLIC_MARKER),
        "privileged user should see both markers; body: {body}"
    );
}

/// A superuser bypasses the per-model check and sees both.
#[tokio::test]
async fn palette_search_includes_both_models_for_superuser() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_super").await;
    let (status, body) = get("/admin/api/palette/search?q=zz", &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(SECRET_MARKER) && body.contains(PUBLIC_MARKER),
        "superuser should see both markers; body: {body}"
    );
}

// ---------------------------------------------------------------------------
// filter_dialog_handler (#3)
// ---------------------------------------------------------------------------

/// The filter dialog for an unviewable model must 403 for a restricted user,
/// but resolve for the model they can view.
#[tokio::test]
async fn filter_dialog_gated_by_view_permission() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_restricted").await;

    let (secret_status, _) = get("/admin/wsecret/filter-dialog", &cookie).await;
    assert_eq!(
        secret_status,
        StatusCode::FORBIDDEN,
        "restricted user must be 403 on the unviewable model's filter dialog"
    );

    let (public_status, _) = get("/admin/wpublic/filter-dialog", &cookie).await;
    assert_eq!(
        public_status,
        StatusCode::OK,
        "restricted user CAN open the filter dialog for a model they can view"
    );
}

/// A privileged user reaches the filter dialog for the previously-blocked model.
#[tokio::test]
async fn filter_dialog_allowed_for_privileged() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_privileged").await;
    let (status, _) = get("/admin/wsecret/filter-dialog", &cookie).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// history_handler (#4)
// ---------------------------------------------------------------------------

/// The audit trail for an unviewable model must 403 for a restricted user; the
/// gate fires before any audit lookup, so any id works.
#[tokio::test]
async fn history_gated_by_view_permission() {
    let _g = LOCK.lock().await;
    let cookie = cookie_for("web7_restricted").await;

    let (secret_status, _) = get("/admin/wsecret/1/history", &cookie).await;
    assert_eq!(
        secret_status,
        StatusCode::FORBIDDEN,
        "restricted user must be 403 on the unviewable model's history"
    );

    // For the viewable model the request passes the permission gate (it does
    // not 403); the concrete status past the gate is not the subject here.
    let (public_status, _) = get("/admin/wpublic/1/history", &cookie).await;
    assert_ne!(
        public_status,
        StatusCode::FORBIDDEN,
        "restricted user must clear the permission gate for a viewable model"
    );
}
