//! Boot-time hard-fail for a `CookieStore` keyed off an empty / insecure
//! `secret_key` in production (audit finding #2).
//!
//! `CookieStore` derives its AEAD key from the ambient `secret_key`. If that
//! secret is empty or still the insecure dev default, every session cookie is
//! sealed under a key any attacker can reproduce — a full auth bypass. The
//! `SessionsPlugin::on_ready` boot check must refuse to boot in `Prod` when a
//! secret-derived `CookieStore` is the configured store and the secret is not
//! a real key. Non-prod only warns; a `CookieStore` pinned with an explicit
//! key, and stores that keep the session server-side (`DbStore`), are exempt.
//!
//! Own test binary so the `on_ready`-driven `install_store` OnceLock write
//! doesn't collide with sibling suites. The boot check itself runs *before*
//! `install_store`, so these assertions hold regardless of install ordering.

use sqlx::sqlite::SqlitePoolOptions;

use umbral::Environment;
use umbral::plugin::{AppContext, Plugin};
use umbral_sessions::store::DbStore;
use umbral_sessions::{CookieStore, SessionsPlugin};

/// Build an `AppContext` with the given ambient secret + environment. The pool
/// is a throwaway in-memory SQLite handle; the boot check never touches it.
async fn ctx(secret: &str, env: Environment) -> AppContext {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("figment defaults load");
    settings.secret_key = secret.to_string();
    settings.environment = env;
    AppContext {
        pool: umbral::db::DbPool::Sqlite(pool),
        settings,
    }
}

/// The exact insecure dev default from `umbral-core`'s settings defaults.
const INSECURE_DEV_SECRET: &str = "umbral-insecure-dev-key-change-me";

// -- the failing cases: CookieStore keyed off a bad ambient secret in Prod --

#[tokio::test]
async fn cookie_store_prod_empty_secret_boot_fails() {
    let plugin = SessionsPlugin::default().store(CookieStore::new());
    let ctx = ctx("", Environment::Prod).await;
    let res = plugin.on_ready(&ctx);
    assert!(
        res.is_err(),
        "CookieStore + empty secret_key in Prod must hard-fail boot"
    );
}

#[tokio::test]
async fn cookie_store_prod_dev_default_secret_boot_fails() {
    let plugin = SessionsPlugin::default().store(CookieStore::new());
    let ctx = ctx(INSECURE_DEV_SECRET, Environment::Prod).await;
    let res = plugin.on_ready(&ctx);
    assert!(
        res.is_err(),
        "CookieStore + insecure dev-default secret in Prod must hard-fail boot"
    );
}

// -- the allowed cases: don't break any legitimate configuration --

#[tokio::test]
async fn cookie_store_prod_real_secret_boots() {
    let plugin = SessionsPlugin::default().store(CookieStore::new());
    let ctx = ctx(
        "a-real-32-byte-plus-production-secret-key",
        Environment::Prod,
    )
    .await;
    assert!(
        plugin.on_ready(&ctx).is_ok(),
        "CookieStore + a real secret in Prod must boot"
    );
}

#[tokio::test]
async fn cookie_store_dev_empty_secret_boots() {
    let plugin = SessionsPlugin::default().store(CookieStore::new());
    let ctx = ctx("", Environment::Dev).await;
    assert!(
        plugin.on_ready(&ctx).is_ok(),
        "CookieStore + empty secret in Dev only warns, must still boot"
    );
}

#[tokio::test]
async fn cookie_store_explicit_key_boots_in_prod_with_empty_ambient() {
    // A CookieStore pinned with its own key doesn't depend on the ambient
    // secret, so an empty ambient secret in Prod is irrelevant to it.
    let plugin = SessionsPlugin::default().store(CookieStore::with_secret("explicit-pinned-key"));
    let ctx = ctx("", Environment::Prod).await;
    assert!(
        plugin.on_ready(&ctx).is_ok(),
        "explicit-key CookieStore doesn't use the ambient secret; must boot"
    );
}

#[tokio::test]
async fn dbstore_prod_empty_secret_boots() {
    // DbStore keeps the session server-side; it never derives anything from
    // secret_key, so an empty secret is not its concern.
    let plugin = SessionsPlugin::default().store(DbStore::default());
    let ctx = ctx("", Environment::Prod).await;
    assert!(
        plugin.on_ready(&ctx).is_ok(),
        "DbStore doesn't use secret_key; empty secret in Prod must still boot"
    );
}
