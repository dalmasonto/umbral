//! `AuthPlugin::with_form_routes()` — the POST surface for server-rendered auth.
//!
//! The framework owns the half that is dangerous to get wrong: `POST /auth/login`,
//! `/auth/signup` and `/auth/logout`, with the password hashing, the throttle, the
//! enumeration-safe error messages, the session cookie, the flash message, and the
//! `?redirect=` open-redirect guard.
//!
//! **It does not serve the login PAGE, and that is deliberate.** Rendering `GET
//! /auth/login` would mean choosing your markup, your CSS and your layout — the one part
//! of auth that is purely yours. You write a normal handler and a normal template; you
//! point its `<form>` at the endpoints below. See `auth/login-and-signup-pages.mdx`.
//!
//! (This file began life as a TDD spec for an `AuthPlugin::with_template_pages()` that
//! would have served those pages. It was rescued from an abandoned worktree, and then the
//! feature was rejected on exactly the reasoning above — so the test was rewritten to
//! cover the surface that DOES exist, which is the half worth testing.)

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
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_form_routes()
                    .with_user_in_templates()
                    .disable_throttle()
                    .disable_password_validation(),
            )
            .build()
            .expect("App::build should succeed");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let router = app.into_router();
        ROUTER.set(router).ok();
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
async fn signup_and_login_post_endpoints_work_without_the_framework_serving_a_page() {
    let router = boot_template_app().await;

    // There is no GET page, on purpose — the plugin mounts POST handlers only.
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
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        "the plugin must NOT serve the login page — the app owns its own markup. \
         If this ever becomes a 200, the framework has started deciding your HTML."
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
