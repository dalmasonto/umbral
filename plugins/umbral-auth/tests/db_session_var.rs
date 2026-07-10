//! gaps3 #45 — publish the authenticated user id to the database connection.
//!
//! umbral's only *bypass-proof* permission layer is Postgres row-level
//! security: `umbral-rls` declares policies whose `USING` / `WITH CHECK`
//! expressions read `current_setting('app.user_id')`. Nothing set that variable.
//!
//! `RouteContext::with_session_var` existed, and the Postgres pool's acquire
//! hook already ran `set_config(name, value, false)` for every entry — but the
//! only hook that could populate it was `AppBuilder::route_context`, whose
//! resolver is **synchronous** (`Fn(&Request) -> RouteContext`). Resolving the
//! session user needs an async DB read. So the wiring the `umbral-rls` docs
//! called "REQUIRED" was not expressible, and `RouteContext::add_session_var`
//! — written for "middleware that augments an already-scoped context" — had
//! zero callers.
//!
//! An unset GUC is not a soft failure. Verified against a real Postgres:
//! `SELECT` against an RLS-enabled table whose policy reads an unset
//! `app.user_id` fails with `ERROR: unrecognized configuration parameter`. Every
//! request 500s. So the layer sets the variable on **every** request, to the
//! empty string when nobody is logged in, and a policy spells that
//! `NULLIF(current_setting('app.user_id'), '')`.
//!
//! Identity comes from `current_user()`, not `current_user_id_str()`: the former
//! filters on `is_active`, so a deactivated account carries no row-access
//! identity.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::db::{RouteContext, TenantKey};
use umbral::plugin::Plugin;
use umbral_auth::{AuthPlugin, AuthUser, hash_password};

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_db_session_var.sqlite");
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
            .plugin(AuthPlugin::<AuthUser>::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS auth_user (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                username      TEXT NOT NULL UNIQUE,
                email         TEXT NOT NULL,
                password_hash TEXT NOT NULL,
                is_active     INTEGER NOT NULL DEFAULT 1,
                is_staff      INTEGER NOT NULL DEFAULT 0,
                is_superuser  INTEGER NOT NULL DEFAULT 0,
                date_joined   TEXT NOT NULL,
                last_login    TEXT,
                email_verified_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user");

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session (
                id         TEXT PRIMARY KEY,
                user_id    TEXT,
                data       TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create session");
    })
    .await;
}

async fn insert_user(username: &str, is_active: bool) -> i64 {
    let pool = umbral::db::pool();
    let hash = hash_password("testpass").expect("hash");
    let now = chrono::Utc::now().to_rfc3339();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO auth_user
           (username, email, password_hash, is_active, is_staff, is_superuser, date_joined)
         VALUES (?, ?, ?, ?, 0, 0, ?)
         RETURNING id",
    )
    .bind(username)
    .bind(format!("{username}@example.com"))
    .bind(&hash)
    .bind(is_active)
    .bind(&now)
    .fetch_one(&pool)
    .await
    .expect("insert user");
    row.0
}

fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

async fn create_session_for(user_id: Option<i64>) -> String {
    let pool = umbral::db::pool();
    let raw = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::days(14);
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at) VALUES (?, ?, '{}', ?, ?)",
    )
    .bind(hash_token(&raw))
    .bind(user_id.map(|id| id.to_string()))
    .bind(now.to_rfc3339())
    .bind(expires.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert session");
    raw
}

// ---------------------------------------------------------------------------
// Harness — a handler that reports what the DB connection would be told.
// ---------------------------------------------------------------------------

/// Echoes the request's Postgres session variables as `name=value;` pairs. This
/// is exactly what the PG pool's acquire hook feeds to `set_config`.
async fn echo_session_vars() -> String {
    umbral::db::route_context::current()
        .session_vars()
        .iter()
        .map(|(n, v)| format!("{n}={v};"))
        .collect()
}

fn app(plugin: AuthPlugin<AuthUser>) -> Router {
    plugin.wrap_router(Router::new().route("/vars", get(echo_session_vars)))
}

async fn vars_for(app: Router, cookie: Option<&str>) -> String {
    let mut b = Request::builder().uri("/vars");
    if let Some(raw) = cookie {
        b = b.header("cookie", format!("umbral_session={raw}"));
    }
    let resp = app
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .expect("request");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------

/// The whole point: a logged-in request carries its user id to the connection,
/// where an RLS policy can read it.
#[tokio::test(flavor = "multi_thread")]
async fn a_logged_in_request_publishes_its_user_id() {
    boot().await;
    let id = insert_user("dbvar_alice", true).await;
    let raw = create_session_for(Some(id)).await;

    let vars = vars_for(
        app(AuthPlugin::<AuthUser>::default().with_db_session_var("app.user_id")),
        Some(&raw),
    )
    .await;

    assert_eq!(vars, format!("app.user_id={id};"), "got: {vars}");
}

/// An anonymous request must still SET the variable, to the empty string.
///
/// This is not cosmetic. Postgres's `current_setting('app.user_id')` raises
/// `unrecognized configuration parameter` when the GUC was never set on the
/// connection — so leaving it unset for anonymous callers would turn every
/// logged-out request into a 500 rather than a clean "you see nothing".
#[tokio::test(flavor = "multi_thread")]
async fn an_anonymous_request_still_sets_the_variable_empty() {
    boot().await;

    let plugin = || AuthPlugin::<AuthUser>::default().with_db_session_var("app.user_id");

    // No cookie at all.
    assert_eq!(vars_for(app(plugin()), None).await, "app.user_id=;");

    // A real session row that belongs to nobody.
    let raw = create_session_for(None).await;
    assert_eq!(vars_for(app(plugin()), Some(&raw)).await, "app.user_id=;");
}

/// A deactivated account carries no identity. `current_user()` filters on
/// `is_active`, and using `current_user_id_str()` here would have handed a
/// disabled user's id to every RLS policy in the app.
#[tokio::test(flavor = "multi_thread")]
async fn a_deactivated_user_publishes_no_identity() {
    boot().await;
    let id = insert_user("dbvar_banned", false).await;
    let raw = create_session_for(Some(id)).await;

    let vars = vars_for(
        app(AuthPlugin::<AuthUser>::default().with_db_session_var("app.user_id")),
        Some(&raw),
    )
    .await;

    assert_eq!(
        vars, "app.user_id=;",
        "an inactive user must look anonymous"
    );
}

/// Opt-in. An app that doesn't ask for it pays nothing — no layer, no session
/// read, no variable.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_builder_no_variable_is_set() {
    boot().await;
    let id = insert_user("dbvar_optout", true).await;
    let raw = create_session_for(Some(id)).await;

    let vars = vars_for(app(AuthPlugin::<AuthUser>::default()), Some(&raw)).await;
    assert_eq!(vars, "", "got: {vars}");
}

/// The layer *augments* the ambient context rather than replacing it. An app
/// that routes by tenant must keep its tenant — and any variable an outer
/// resolver already set — through the auth layer.
#[tokio::test(flavor = "multi_thread")]
async fn the_ambient_route_context_survives() {
    boot().await;
    let id = insert_user("dbvar_tenant", true).await;
    let raw = create_session_for(Some(id)).await;

    async fn echo_tenant_and_vars() -> String {
        let ctx = umbral::db::route_context::current();
        let tenant = ctx.tenant().map(|t| t.as_str()).unwrap_or("<none>");
        let vars: String = ctx
            .session_vars()
            .iter()
            .map(|(n, v)| format!("{n}={v};"))
            .collect();
        format!("tenant={tenant} {vars}")
    }

    let inner = AuthPlugin::<AuthUser>::default()
        .with_db_session_var("app.user_id")
        .wrap_router(Router::new().route("/vars", get(echo_tenant_and_vars)));

    // Stand in for the framework's own route-context layer, which wraps
    // outside every plugin layer.
    let outer = inner.layer(axum::middleware::from_fn(
        |req: axum::extract::Request, next: axum::middleware::Next| async move {
            let ctx = RouteContext::new()
                .with_tenant(TenantKey::new("acme"))
                .with_session_var("app.tenant", "acme");
            umbral::db::route_context_scope(ctx, next.run(req)).await
        },
    ));

    let body = vars_for(outer, Some(&raw)).await;
    assert_eq!(
        body,
        format!("tenant=acme app.tenant=acme;app.user_id={id};"),
        "the auth layer must add to the context, not replace it",
    );
}
