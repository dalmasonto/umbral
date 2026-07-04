//! audit_2 C2/R3 + H18 — RLS on SQLite provides NO row isolation, so under
//! `Environment::Prod` the boot must FAIL CLOSED (not silently skip), rather
//! than hand the operator a false isolation guarantee.
//!
//! Own test binary: `App::build()` sets the process-global settings `OnceLock`
//! exactly once, so this can't share a binary with another App-building test.

use umbral::Settings;
use umbral::prelude::*;
use umbral_rls::{Action, RlsPlugin};

#[tokio::test]
async fn rls_plugin_fails_closed_on_sqlite_in_prod() {
    let mut settings = Settings::from_env().expect("figment defaults");
    settings.database_url = "sqlite::memory:".to_string();
    settings.environment = umbral::Environment::Prod;
    settings.secret_key = "x".repeat(48); // satisfy the Prod secret-key floor
    settings.allowed_hosts = vec!["example.com".to_string()];
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RlsPlugin::new().enable_on("post").policy(
            "post",
            "owner_read",
            Action::Select,
            "user_id = 1",
        ))
        .build();

    let err = match result {
        Ok(_) => panic!("Prod + SQLite + RLS must fail the boot, not skip"),
        Err(e) => e,
    };
    let msg = format!("{err:?}").to_lowercase();
    assert!(
        msg.contains("postgres-only") || msg.contains("row isolation"),
        "boot should fail on the RLS/SQLite misconfig; got: {err:?}"
    );
}
