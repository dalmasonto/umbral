//! audit_2 plugin-storage-tasks #6 — `QuerySet::for_update_skip_locked()` adds
//! `FOR UPDATE SKIP LOCKED` on Postgres (a no-op on SQLite). Concurrent claimers
//! then skip rows another transaction already locked instead of blocking on
//! them, so N workers claim N different rows — the canonical job-queue primitive.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct FulJob {
    pub id: i64,
    pub status: String,
}

#[test]
fn renders_the_clause_on_postgres_only() {
    let pg = FulJob::objects()
        .filter(ful_job::STATUS.eq("pending"))
        .order_by(ful_job::ID.asc())
        .limit(1)
        .for_update_skip_locked()
        .to_sql_pg();
    assert!(
        pg.to_uppercase().contains("FOR UPDATE SKIP LOCKED"),
        "Postgres render must carry FOR UPDATE SKIP LOCKED; got {pg}"
    );

    let lite = FulJob::objects()
        .filter(ful_job::STATUS.eq("pending"))
        .order_by(ful_job::ID.asc())
        .limit(1)
        .for_update_skip_locked()
        .to_sql();
    assert!(
        !lite.to_uppercase().contains("FOR UPDATE") && !lite.to_uppercase().contains("SKIP LOCKED"),
        "SQLite render must NOT carry the lock clause (unsupported); got {lite}"
    );
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn a_second_claimer_skips_the_locked_row_instead_of_blocking() {
    use std::time::Duration;
    use umbral::db::begin_pg;

    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<FulJob>()
        .build()
        .expect("App::build");

    sqlx::query("DROP TABLE IF EXISTS ful_job")
        .execute(&pool)
        .await
        .expect("drop");
    sqlx::query("CREATE TABLE ful_job (id BIGINT PRIMARY KEY, status TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create");
    sqlx::query("INSERT INTO ful_job (id, status) VALUES (1, 'pending'), (2, 'pending')")
        .execute(&pool)
        .await
        .expect("seed");

    fn claim() -> umbral::orm::QuerySet<FulJob> {
        FulJob::objects()
            .filter(ful_job::STATUS.eq("pending"))
            .order_by(ful_job::ID.asc())
            .limit(1)
            .for_update_skip_locked()
    }

    // Transaction A claims + LOCKS the head row (id=1) and holds it open.
    let mut tx_a = begin_pg(&pool).await.expect("begin A");
    let a = claim()
        .on_tx(&mut tx_a)
        .first()
        .await
        .expect("A claim")
        .expect("A gets a row");
    assert_eq!(a.id, 1, "A claims the head row");

    // Transaction B, with SKIP LOCKED, must SKIP the locked id=1 and get id=2 —
    // and must NOT block on A. If the clause were missing, B would block on id=1
    // until A commits and this `timeout` would elapse (→ None → test fails).
    let mut tx_b = begin_pg(&pool).await.expect("begin B");
    let b = tokio::time::timeout(Duration::from_secs(3), async {
        claim().on_tx(&mut tx_b).first().await.expect("B claim")
    })
    .await
    .expect("B must NOT block on A's locked row — SKIP LOCKED should return immediately")
    .expect("B gets a row");
    assert_eq!(
        b.id, 2,
        "B must skip the locked row (id=1) and claim the next available (id=2)"
    );

    tx_a.rollback().await.ok();
    tx_b.rollback().await.ok();
    sqlx::query("DROP TABLE IF EXISTS ful_job")
        .execute(&pool)
        .await
        .expect("cleanup");
}
