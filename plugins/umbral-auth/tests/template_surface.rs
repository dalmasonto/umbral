//! TDD spec — server-rendered auth PAGES (gaps3 #63). Currently RED, and `#[ignore]`d.
//!
//! Rescued from an abandoned agent worktree during the 2026-07-12 branch cleanup: it was
//! an untracked file that existed in no branch and no commit, and deleting the worktree
//! would have destroyed it.
//!
//! It does not pass, and it is not meant to yet. `AuthPlugin::with_form_routes()` mounts
//! the form **POST** handlers (`POST /auth/login`, `POST /auth/signup`) but serves no
//! **GET** page — `GET /auth/login` is a 405 — so every app still hand-writes its own
//! login and signup page handlers plus their templates. (The original test called
//! `with_template_pages()`, a method that never existed; `with_form_routes` is what
//! actually shipped, and it only covers half the surface.)
//!
//! Un-`#[ignore]` this the moment the plugin serves the pages. It IS the acceptance
//! criterion — that is why it is kept rather than deleted.
//!
//! Boots a real App with `AuthPlugin::with_template_pages()` and drives the
//! GET /auth/login and POST /auth/signup routes via `tower::ServiceExt::oneshot`.
//! CSRF is not active in the test app (no SecurityPlugin), so plain form-encoded
//! POSTs work without tokens.

use axum::Router;
use tokio::sync::OnceCell;
use umbral_auth::{AuthPlugin, AuthUser};

static BOOT: OnceCell<()> = OnceCell::const_new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot_template_app() -> Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_template_surface.sqlite");
        std::mem::forget(tmp);

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .busy_timeout(std::time::Duration::from_secs(30)),
            )
            .await
            .expect("sqlite tempfile pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_form_routes()
                    .with_user_in_templates()
                    .disable_throttle()
                    .disable_password_validation(),
            )
            .build()
            .expect("App::build should succeed");

        let router = app.into_router();
        ROUTER.set(router).ok();

        let pool = umbral::db::pool();

        sqlx::query(
            "CREATE TABLE auth_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1,
                is_staff INTEGER NOT NULL DEFAULT 0,
                is_superuser INTEGER NOT NULL DEFAULT 0,
                date_joined TEXT NOT NULL,
                last_login TEXT,
                email_verified_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user table");

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
        .expect("create session table");

        sqlx::query(
            "CREATE TABLE auth_challenge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                purpose TEXT NOT NULL,
                secret_hash TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                attempts INTEGER NOT NULL,
                used_at TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_challenge table");

        sqlx::query(
            "CREATE TABLE auth_token (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                key_hash TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_used_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_token table");
    })
    .await;

    ROUTER.get().expect("router set during boot").clone()
}

async fn post_form(
    router: &Router,
    uri: &str,
    body: &str,
) -> axum::http::Response<axum::body::Body> {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
#[ignore = "gaps3 #63: AuthPlugin serves the form POSTs but not the GET pages — this is the spec for that feature, not a regression"]
async fn template_pages_render_and_signup_redirects() {
    let router = boot_template_app().await;

    // GET /auth/login → 200, body contains the username input.
    use tower::ServiceExt;
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/auth/login")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("name=\"username\""),
        "login form should contain username input"
    );

    // POST /auth/signup creates the user and redirects (303/302).
    let resp = post_form(
        &router,
        "/auth/signup",
        "username=fred&email=fred%40x.com&password=G00d%24Pass%21",
    )
    .await;
    assert!(
        resp.status().is_redirection(),
        "signup should redirect after success; got {}",
        resp.status()
    );
    assert!(
        umbral_auth::authenticate::<AuthUser>("fred", "G00d$Pass!")
            .await
            .is_ok(),
        "fred should be authenticatable after signup"
    );
}
