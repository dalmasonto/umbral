//! audit_2 plugin-sessions #7 — the session cookie's `SameSite` attribute is
//! configurable via `SessionsPlugin::same_site(..)`, defaulting to `Lax`.
//! `SameSite=None` additionally forces `Secure` (browsers reject it otherwise).

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_sessions::{SameSite, SessionsPlugin, clear_cookie_header, set_cookie_header};

async fn boot_with(policy: SameSite) {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("same_site.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(SessionsPlugin::default().same_site(policy))
        .build()
        .expect("App::build with SessionsPlugin");
}

#[tokio::test]
async fn same_site_defaults_to_lax_then_honors_config() {
    // Before any plugin seals the policy, the cookie is `SameSite=Lax` — the
    // behavior-preserving default.
    let before = set_cookie_header("tok", None);
    assert!(
        before.contains("SameSite=Lax"),
        "the default SameSite must be Lax; got {before}"
    );

    // Boot with SameSite=None (the case that also forces Secure).
    boot_with(SameSite::None).await;

    let after = set_cookie_header("tok", None);
    assert!(
        after.contains("SameSite=None"),
        "the configured SameSite must thread into the Set-Cookie; got {after}"
    );
    assert!(
        after.contains("Secure"),
        "SameSite=None must force Secure (browsers reject None without it); got {after}"
    );

    // The clear-cookie header carries the same policy.
    let cleared = clear_cookie_header();
    assert!(
        cleared.contains("SameSite=None") && cleared.contains("Secure"),
        "the clear-cookie header must honor the SameSite policy too; got {cleared}"
    );
}
