//! End-to-end PKCE wiring (RFC 7636 / gaps2 #74).
//!
//! Driving the *real* authorize route through the *real* session layer
//! must:
//!   (a) redirect to the provider carrying `code_challenge` +
//!       `code_challenge_method=S256`, and
//!   (b) persist a `code_verifier` in the session whose S256 hash EQUALS
//!       the challenge that was actually sent to the provider.
//!
//! That equality is the whole point of PKCE: the secret kept server-side
//! (the verifier) is bound to the public value sent on the redirect (its
//! hash), so an intercepted `code` is useless without the verifier. We
//! prove it through the mounted HTTP route + persisted session row, not by
//! reaching into private state.
//!
//! Its own test binary (separate process → its own ambient pool
//! `OnceLock`) so the single-row `SELECT data FROM session` is clean.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral::web::header;
use umbral_oauth::OAuthPlugin;
use umbral_oauth::pkce::challenge_s256;
use umbral_oauth::providers::GoogleProvider;
use umbral_sessions::SessionsPlugin;

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("pkce_flow.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("sqlite tempfile pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(SessionsPlugin::default())
        .build()
        .expect("App::build with SessionsPlugin");

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE session (\
            id TEXT PRIMARY KEY,\
            user_id TEXT,\
            data TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            expires_at TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create session");
}

#[tokio::test]
async fn login_redirect_challenge_is_the_hash_of_the_persisted_verifier() {
    boot().await;

    let oauth = OAuthPlugin::new("https://app.example.com")
        .provider(GoogleProvider::new("client123", "secret"));
    let router = SessionsPlugin::default().wrap_router(oauth.routes());

    // Drive the real authorize route, cookie-less → a fresh flow.
    let req = Request::builder()
        .uri("/oauth/google/login")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot login");

    // (a) Redirect to Google carrying the S256 challenge.
    assert!(
        resp.status().is_redirection(),
        "login must redirect, got {}",
        resp.status()
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location header")
        .to_str()
        .expect("ascii Location");
    let url = url::Url::parse(location).expect("Location is an absolute URL");
    assert_eq!(
        url.host_str(),
        Some("accounts.google.com"),
        "redirect targets the provider"
    );
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("code_challenge_method").map(String::as_str),
        Some("S256"),
        "only the S256 method is ever emitted"
    );
    let challenge = query
        .get("code_challenge")
        .cloned()
        .expect("code_challenge present on the redirect");

    // (b) The session persisted a verifier; its S256 hash must equal the
    //     challenge that was actually sent. `begin_flow`'s write (set_data)
    //     materialised exactly one session row.
    let data: String = sqlx::query_scalar("SELECT data FROM session LIMIT 1")
        .fetch_one(&umbral::db::pool())
        .await
        .expect("begin_flow wrote one session row");
    let json: serde_json::Value = serde_json::from_str(&data).expect("session data is JSON");
    let verifier = json["oauth_flow"]["code_verifier"]
        .as_str()
        .expect("flow persisted a code_verifier");

    assert!(!verifier.is_empty(), "the verifier is a real secret");
    assert_ne!(
        verifier, challenge,
        "the challenge is the HASH, never the verifier itself"
    );
    assert_eq!(
        challenge_s256(verifier),
        challenge,
        "the challenge sent to the provider must be S256(persisted verifier)"
    );
}
