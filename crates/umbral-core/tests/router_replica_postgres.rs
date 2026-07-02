//! REAL streaming-replication validation of the read/write split. `#[ignore]`,
//! gated on `UMBRAL_PRIMARY_URL` + `UMBRAL_REPLICA_URL` (a Postgres primary and a
//! read-only streaming replica).
//!
//! The proof is rigorous: it **pauses WAL replay** on the standby so the
//! replica is deliberately STALE while the primary is FRESH, then shows
//! (a) an umbral read returns the *stale replica* state — not the fresh
//! primary, definitively proving the read hit the replica; and (b)
//! `get_or_create` finds the un-replicated row on the *primary* (read-your-
//! writes under real lag), never inserting a duplicate.
//!
//! ```text
//! UMBRAL_PRIMARY_URL=postgres://app:apppass@localhost:5433/appdb \
//! UMBRAL_REPLICA_URL=postgres://app:apppass@localhost:5440/appdb \
//!   cargo test -p umbral-core --test router_replica_postgres -- --ignored --nocapture
//! ```
//!
//! Prerequisite: the app role needs EXECUTE on the replay-control functions
//! (run once on the PRIMARY as a superuser; it replicates to the standby):
//! `GRANT EXECUTE ON FUNCTION pg_catalog.pg_wal_replay_pause() TO app;`
//! `GRANT EXECUTE ON FUNCTION pg_catalog.pg_wal_replay_resume() TO app;`

#![allow(dead_code)]

use std::time::Duration;

use umbral::db::{Alias, DatabaseRouter, RouteContext};
use umbral::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "repl_note")]
pub struct RNote {
    pub id: i64,
    pub body: String,
}

struct ReplicaRouter;
impl DatabaseRouter for ReplicaRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new("replica")
    }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new("default")
    }
}

async fn count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM repl_note")
        .fetch_one(pool)
        .await
        .expect("count")
}

async fn wait_until<F>(pool: &sqlx::PgPool, mut done: F, what: &str)
where
    F: FnMut(i64) -> bool,
{
    for _ in 0..100 {
        if done(count(pool).await) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres primary + streaming replica (UMBRAL_PRIMARY_URL / UMBRAL_REPLICA_URL)"]
async fn read_write_split_against_real_streaming_replica() {
    let primary_url = std::env::var("UMBRAL_PRIMARY_URL").expect("UMBRAL_PRIMARY_URL");
    let replica_url = std::env::var("UMBRAL_REPLICA_URL").expect("UMBRAL_REPLICA_URL");

    let primary = sqlx::PgPool::connect(&primary_url).await.expect("primary");
    let replica = sqlx::PgPool::connect(&replica_url).await.expect("replica");

    let in_recovery: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&replica)
        .await
        .expect("recovery check");
    assert!(
        in_recovery,
        "UMBRAL_REPLICA_URL must be a read-only standby"
    );

    // Make sure replay is running, then (re)create the table on the PRIMARY
    // and wait for it to stream to the standby.
    let _ = sqlx::query("SELECT pg_wal_replay_resume()")
        .execute(&replica)
        .await; // ignore "not paused" error
    sqlx::query("DROP TABLE IF EXISTS repl_note")
        .execute(&primary)
        .await
        .ok();
    sqlx::query("CREATE TABLE repl_note (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL)")
        .execute(&primary)
        .await
        .expect("create on primary");
    // Wait for the table object to exist on the standby (DDL replicates too).
    for _ in 0..100 {
        if sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass('public.repl_note')::text")
            .fetch_one(&replica)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = primary_url.clone();

    umbral::App::builder()
        .settings(settings)
        .database("default", primary.clone()) // primary — writes
        .database("replica", replica.clone()) // real streaming replica — reads
        .router(ReplicaRouter)
        .model::<RNote>()
        .build()
        .expect("App::build");

    // 1) WRITE row "alpha" via umbral → primary, and let the standby catch up.
    RNote::objects()
        .create(RNote {
            id: 0,
            body: "alpha".into(),
        })
        .await
        .expect("create alpha");
    wait_until(&replica, |n| n == 1, "alpha replicated").await;

    // 2) FREEZE the standby: pause WAL replay. From here the replica's visible
    //    state is stuck at {alpha} no matter what the primary does.
    sqlx::query("SELECT pg_wal_replay_pause()")
        .execute(&replica)
        .await
        .expect("pause replay");

    // 3) WRITE row "beta" via umbral → primary. Primary now has {alpha, beta};
    //    the frozen replica still has only {alpha}.
    RNote::objects()
        .create(RNote {
            id: 0,
            body: "beta".into(),
        })
        .await
        .expect("create beta");

    // Sanity: the two databases have genuinely diverged.
    assert_eq!(count(&primary).await, 2, "primary has alpha+beta");
    assert_eq!(
        count(&replica).await,
        1,
        "frozen replica still has only alpha"
    );

    // 4) READ-YOUR-WRITES UNDER LAG: get_or_create on "beta" must find it on
    //    the PRIMARY (the write pool). If it probed the frozen replica it would
    //    miss beta and insert a duplicate (primary → 3 rows).
    let (_row, was_created) = RNote::objects()
        .get_or_create(
            RNote::BODY.eq("beta"),
            RNote {
                id: 0,
                body: "beta".into(),
            },
        )
        .await
        .expect("get_or_create");
    assert!(!was_created, "read-your-writes: found beta on the primary");
    assert_eq!(
        count(&primary).await,
        2,
        "no duplicate inserted on the primary"
    );

    // 5) THE KEY PROOF: an umbral READ routes to the replica, so it must return
    //    the STALE state {alpha} — NOT the primary's fresh {alpha, beta}. This
    //    is what proves the read hit the replica, not the primary.
    let rows = RNote::objects().fetch().await.expect("fetch from replica");
    assert_eq!(
        rows.len(),
        1,
        "umbral read the STALE replica (got {:?}), not the fresh primary",
        rows.iter().map(|r| &r.body).collect::<Vec<_>>()
    );
    assert_eq!(rows[0].body, "alpha");
    assert_eq!(RNote::objects().count().await.expect("count"), 1);

    // 6) RESUME replay; the replica catches up and umbral now reads both rows.
    sqlx::query("SELECT pg_wal_replay_resume()")
        .execute(&replica)
        .await
        .expect("resume replay");
    wait_until(&replica, |n| n == 2, "beta replicated after resume").await;
    let rows = RNote::objects()
        .fetch()
        .await
        .expect("fetch after catch-up");
    assert_eq!(rows.len(), 2, "replica caught up; umbral reads both");

    sqlx::query("DROP TABLE IF EXISTS repl_note")
        .execute(&primary)
        .await
        .ok();
    println!(
        "OK: write→primary; replica frozen via pg_wal_replay_pause; umbral fetch returned the \
         STALE replica state {{alpha}} while the primary held {{alpha,beta}}; read-your-writes \
         found beta on the primary (no duplicate); resume → umbral reads both."
    );
}
