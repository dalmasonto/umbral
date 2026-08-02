//! features #84: reverse-FK one-to-many list expansion in the template
//! `user` context.
//!
//! `AuthPlugin::expand_list::<Purchase>()` opts the `purchase` child table
//! into the `user` context, so a template can iterate `user.purchase_set`
//! without the handler pre-resolving it. The list is capped, so a user with
//! more rows than the cap never loads all of them into a render.
//!
//! Behavioural, through the real request path: a real user with 25 real
//! `Purchase` rows, a real session cookie, the real `user_context_layer`
//! (mounted with the opt-in table as state, exactly as `wrap_router` does),
//! and a handler that renders an inline template reading the expanded value.
//! We read the object graph back out of the rendered output, not an internal.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::orm::ForeignKey;
use umbral_auth::{AuthPlugin, AuthUser, user_context_layer};

/// A child with a NON-unique FK to `AuthUser` — a reverse-FK one-to-many.
/// Table `purchase`, so the list surfaces under `user.purchase_set`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct Purchase {
    pub id: i64,
    pub buyer: ForeignKey<AuthUser>,
    pub amount: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_reverse_fk_list.sqlite");
        std::mem::forget(tmp);
        let opts = SqliteConnectOptions::new()
            .busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .expect("sqlite connect");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(AuthPlugin::<AuthUser>::default())
            .model::<Purchase>()
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

async fn insert_user(username: &str) -> i64 {
    let pool = umbral::db::pool();
    let hash = umbral_auth::hash_password("testpass").expect("hash");
    let now = chrono::Utc::now().to_rfc3339();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO auth_user
           (username, email, password_hash, is_active, is_staff, is_superuser, date_joined)
         VALUES (?, ?, ?, 1, 0, 0, ?)
         RETURNING id",
    )
    .bind(username)
    .bind(format!("{username}@example.com"))
    .bind(&hash)
    .bind(&now)
    .fetch_one(&pool)
    .await
    .expect("insert user");
    row.0
}

async fn create_session_for(user_id: i64) -> String {
    use uuid::Uuid;
    let pool = umbral::db::pool();
    let raw = Uuid::new_v4().to_string();
    let stored = hash_token(&raw);
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::days(14);
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at)
         VALUES (?, ?, '{}', ?, ?)",
    )
    .bind(&stored)
    .bind(user_id.to_string())
    .bind(now.to_rfc3339())
    .bind(expires.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert session");
    raw
}

/// Renders `is_authenticated` and the length of the injected list, so the
/// assertion reads the real expanded graph out of the rendered output.
async fn whoami() -> axum::response::Html<String> {
    let body = umbral::templates::render_str(
        "{{ user.is_authenticated }}|{{ user.purchase_set | default([]) | length }}",
        &serde_json::json!({}),
    )
    .expect("render");
    axum::response::Html(body)
}

async fn get_whoami(cookie: Option<&str>) -> (StatusCode, String) {
    let app = axum::Router::new()
        .route("/whoami", get(whoami))
        // Exactly what `wrap_router` mounts under with_user_in_templates()
        // + expand_list::<Purchase>(): the opt-in table as layer state.
        .layer(axum::middleware::from_fn_with_state(
            std::sync::Arc::new(vec!["purchase".to_string()]),
            user_context_layer,
        ));
    let mut builder = Request::builder().method("GET").uri("/whoami");
    if let Some(c) = cookie {
        builder = builder.header("cookie", format!("umbral_session={c}"));
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).trim().to_string())
}

/// The opted-in reverse-FK list appears under `user.<table>_set`, capped:
/// 25 real purchases render as a 20-element list, never all 25.
#[tokio::test(flavor = "multi_thread")]
async fn reverse_fk_list_is_injected_and_capped() {
    boot().await;
    let uid = insert_user("rfl_buyer").await;

    let pool = umbral::db::pool();
    for i in 0..25i64 {
        sqlx::query("INSERT INTO purchase (buyer, amount) VALUES (?, ?)")
            .bind(uid)
            .bind(i)
            .execute(&pool)
            .await
            .expect("insert purchase");
    }

    let token = create_session_for(uid).await;
    let (status, body) = get_whoami(Some(&token)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "true|20",
        "the authenticated user should carry a purchase_set capped at 20 (25 rows exist); got: {body}",
    );
}

/// An anonymous request has no `user.purchase_set` at all — `length` of an
/// undefined value is 0, and nothing queries the child table.
#[tokio::test(flavor = "multi_thread")]
async fn anonymous_user_has_no_list() {
    boot().await;
    let (status, body) = get_whoami(None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "false|0",
        "anonymous → is_authenticated false and an empty/absent list; got: {body}",
    );
}
