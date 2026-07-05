//! audit_2 core-migrate #7 — concurrent migrators must not race the same DDL.
//! A session-level Postgres advisory lock serializes them: one applies the
//! migration and records it, the other waits, then sees it applied and skips —
//! instead of both reading the empty applied-set and colliding on
//! `CREATE TABLE` ("relation already exists"), which aborts a replica's deploy.
//!
//! `#[ignore]` + `UMBRAL_TEST_POSTGRES_URL` like every other live-PG test.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use umbral::migrate::{make_in, run_in};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "lock_race_post")]
pub struct LockRacePost {
    pub id: i64,
    pub title: String,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn concurrent_migrators_serialize_and_apply_once() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");

    // Clean slate so the test is idempotent across re-runs against the same DB:
    // drop the table AND its tracking rows, so `make_in` writes a fresh
    // migration and `run_in` genuinely applies it.
    sqlx::query("DROP TABLE IF EXISTS lock_race_post")
        .execute(&pool)
        .await
        .expect("drop table");
    // The tracking table may not exist yet on a fresh DB — ignore the error.
    let _ = sqlx::query("DELETE FROM umbral_migrations WHERE name LIKE '%lock_race_post%'")
        .execute(&pool)
        .await;

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<LockRacePost>()
        .build()
        .expect("App::build");

    // Write the CreateTable migration once (both migrators read the same dir).
    let dir = tempfile::tempdir().expect("tempdir");
    make_in(dir.path()).await.expect("make_in writes migration");

    // Two migrators run the SAME pending migration CONCURRENTLY against the same
    // database — exactly the multi-replica deploy race. The advisory lock must
    // serialize them.
    let d1 = dir.path().to_path_buf();
    let d2 = dir.path().to_path_buf();
    let (r1, r2) = tokio::join!(run_in(&d1), run_in(&d2));

    let applied1 = r1.expect("first migrator must not error");
    let applied2 = r2.expect("second migrator must not error — the lock prevents the DDL race");

    // Exactly one of them applied the migration; the other saw it already
    // applied and did nothing. Without the lock, both would read the empty
    // applied-set and one would abort on a duplicate CREATE TABLE.
    assert_eq!(
        applied1 + applied2,
        1,
        "the migration must be applied exactly once across both concurrent \
         migrators (got {applied1} + {applied2})"
    );

    // The table exists exactly once and is usable.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_name = 'lock_race_post')",
    )
    .fetch_one(&pool)
    .await
    .expect("query information_schema");
    assert!(exists, "the table must have been created");

    sqlx::query("DROP TABLE IF EXISTS lock_race_post")
        .execute(&pool)
        .await
        .expect("cleanup");
}
