//! REAL streaming-replication validation of the read/write split. `#[ignore]`,
//! gated on `UMBRA_PRIMARY_URL` + `UMBRA_REPLICA_URL` (a Postgres primary and a
//! read-only streaming replica). Proves umbra routes writes to the primary and
//! reads to the ACTUAL replica (not the same pool), including read-your-writes
//! against replica lag.
//!
//! ```text
//! UMBRA_PRIMARY_URL=postgres://app:apppass@localhost:5433/appdb \
//! UMBRA_REPLICA_URL=postgres://app:apppass@localhost:5440/appdb \
//!   cargo test -p umbra-core --test router_replica_postgres -- --ignored --nocapture
//! ```

#![allow(dead_code)]

use std::time::Duration;

use umbra::db::{Alias, DatabaseRouter, RouteContext};
use umbra::migrate::ModelMeta;

#[derive(
    Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "repl_note")]
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

async fn wait_for(pool: &sqlx::PgPool, predicate_sql: &str, what: &str) {
    for _ in 0..100 {
        if let Ok(true) = sqlx::query_scalar::<_, bool>(predicate_sql)
            .fetch_one(pool)
            .await
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres primary + streaming replica (UMBRA_PRIMARY_URL / UMBRA_REPLICA_URL)"]
async fn read_write_split_against_real_streaming_replica() {
    let primary_url = std::env::var("UMBRA_PRIMARY_URL").expect("UMBRA_PRIMARY_URL");
    let replica_url = std::env::var("UMBRA_REPLICA_URL").expect("UMBRA_REPLICA_URL");

    let primary = sqlx::PgPool::connect(&primary_url).await.expect("primary");
    let replica = sqlx::PgPool::connect(&replica_url).await.expect("replica");

    // Sanity: the replica really is a read-only standby.
    let in_recovery: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&replica)
        .await
        .expect("recovery check");
    assert!(in_recovery, "UMBRA_REPLICA_URL must point at a read-only standby");

    // Schema is created on the PRIMARY only — the read-only replica gets it via
    // replication (you cannot CREATE TABLE on a standby).
    sqlx::query("DROP TABLE IF EXISTS repl_note")
        .execute(&primary)
        .await
        .ok();
    sqlx::query("CREATE TABLE repl_note (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL)")
        .execute(&primary)
        .await
        .expect("create on primary");
    wait_for(
        &replica,
        "SELECT to_regclass('public.repl_note') IS NOT NULL",
        "table replicated to standby",
    )
    .await;

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = primary_url.clone(); // keep the boot backend-check happy

    umbra::App::builder()
        .settings(settings)
        .database("default", primary.clone()) // primary — writes
        .database("replica", replica.clone()) // real streaming replica — reads
        .router(ReplicaRouter)
        .model::<RNote>()
        .build()
        .expect("App::build");

    // WRITE via umbra → db_for_write → "default" (the primary).
    let created = RNote::objects()
        .create(RNote {
            id: 0,
            body: "hello-from-primary".into(),
        })
        .await
        .expect("create");
    assert!(created.id > 0, "primary assigned a PK");

    // Read-your-writes: probe immediately — `get_or_create` must find the row
    // on the WRITE pool, never insert a duplicate (router_upsert_readwrite is
    // the rigorous empty-replica proof; here it must also hold against a real
    // replica that may not have caught up yet).
    let (_row, was_created) = RNote::objects()
        .get_or_create(
            RNote::BODY.eq("hello-from-primary"),
            RNote {
                id: 0,
                body: "hello-from-primary".into(),
            },
        )
        .await
        .expect("get_or_create");
    assert!(
        !was_created,
        "read-your-writes: found on the write DB, no duplicate"
    );

    // Wait for the write to actually stream to the standby.
    wait_for(
        &replica,
        "SELECT count(*) = 1 FROM repl_note WHERE body = 'hello-from-primary'",
        "row replicated to standby",
    )
    .await;

    // READ via umbra → db_for_read → "replica" → reads the ACTUAL standby.
    let rows = RNote::objects().fetch().await.expect("fetch from replica");
    assert_eq!(rows.len(), 1, "umbra read the row back from the streaming replica");
    assert_eq!(rows[0].body, "hello-from-primary");

    // And the count terminal (also a read) hits the replica too.
    let n = RNote::objects().count().await.expect("count from replica");
    assert_eq!(n, 1);

    sqlx::query("DROP TABLE IF EXISTS repl_note")
        .execute(&primary)
        .await
        .ok();
    println!("OK: writes→primary, reads→real streaming replica, read-your-writes held");
}
