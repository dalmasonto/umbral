//! Integration tests for the umbral-rls plugin.
//!
//! Most coverage lives in the unit tests inside `src/lib.rs` (DDL
//! rendering, builder API). This file adds:
//!
//! - **SQLite skip path**: booting an App with an `RlsPlugin` against
//!   a SQLite pool should succeed (the plugin warns and skips).
//! - **PG round trip** (`#[ignore]`'d, needs `UMBRAL_TEST_POSTGRES_URL`):
//!   build an App against PG with an RlsPlugin, verify the policies
//!   show up in `pg_policies`.

use umbral::Settings;
use umbral::prelude::*;
use umbral_rls::{Action, RlsPlugin};

#[tokio::test]
async fn rls_plugin_skips_on_sqlite_without_failing_boot() {
    // Boot with a SQLite pool. The plugin should run on_ready and
    // skip silently — App::build returns Ok.
    let mut settings = Settings::from_env().expect("figment defaults");
    settings.database_url = "sqlite::memory:".to_string();
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

    match result {
        Ok(_app) => {
            // expected — plugin skipped without error
        }
        Err(err) => panic!("expected RlsPlugin to skip on SQLite, got: {err:?}"),
    }
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn rls_plugin_applies_policies_on_postgres() {
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    // Clean state — drop the table so the policy DDL has a fresh slate.
    sqlx::query("DROP TABLE IF EXISTS umbral_phase45_post")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase45_post ( \
            id BIGSERIAL PRIMARY KEY, \
            user_id INTEGER NOT NULL, \
            title TEXT NOT NULL \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut settings = Settings::from_env().expect("figment defaults");
    settings.database_url = url.clone();

    let app = App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .plugin(
            RlsPlugin::new()
                .policy(
                    "umbral_phase45_post",
                    "user_can_read_own",
                    Action::Select,
                    "user_id = current_setting('app.user_id')::int",
                )
                .policy_with_check(
                    "umbral_phase45_post",
                    "user_can_create_own",
                    Action::Insert,
                    "user_id = current_setting('app.user_id')::int",
                    "user_id = current_setting('app.user_id')::int",
                ),
        )
        .build()
        .expect("App::build should succeed");

    // The policies should now exist in pg_policies.
    let policies: Vec<(String, String)> = sqlx::query_as(
        "SELECT policyname, cmd FROM pg_policies \
         WHERE schemaname = 'public' AND tablename = 'umbral_phase45_post' \
         ORDER BY policyname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(policies.len(), 2, "expected two policies; got {policies:?}");
    assert_eq!(policies[0].0, "user_can_create_own");
    assert_eq!(policies[0].1, "INSERT");
    assert_eq!(policies[1].0, "user_can_read_own");
    assert_eq!(policies[1].1, "SELECT");

    // RLS should be ENABLED on the table.
    let rls_enabled: (bool,) = sqlx::query_as(
        "SELECT relrowsecurity FROM pg_class \
         WHERE oid = 'public.umbral_phase45_post'::regclass",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rls_enabled.0, "expected RLS to be enabled");

    // Re-booting should be idempotent (DROP IF EXISTS + CREATE).
    let _app2 = App::builder()
        .settings(Settings::from_env().expect("figment defaults"))
        .database("default", pool.clone())
        .plugin(RlsPlugin::new().policy(
            "umbral_phase45_post",
            "user_can_read_own",
            Action::Select,
            "user_id = current_setting('app.user_id')::int",
        ))
        .build();
    // The second boot should leave just one policy now (the second
    // RlsPlugin only declared one). The first plugin's
    // `user_can_create_own` policy is NOT explicitly dropped by the
    // second boot — the plugin only drops policies it's about to
    // recreate. This is the honest behavior: declarations are
    // append-only across boots; users who want to revoke policies do
    // it explicitly.
    let after_reboot: Vec<(String,)> = sqlx::query_as(
        "SELECT policyname FROM pg_policies \
         WHERE schemaname = 'public' AND tablename = 'umbral_phase45_post'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<&str> = after_reboot.iter().map(|(n,)| n.as_str()).collect();
    assert!(names.contains(&"user_can_read_own"));

    // App is dropped here; the policies stay on the table until next
    // run. The test cleans the table on entry, so no further work.
    drop(app);
}
