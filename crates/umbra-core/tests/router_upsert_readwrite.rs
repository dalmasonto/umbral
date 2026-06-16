//! Read-your-writes for the upsert paths under a read/write-split router.
//!
//! `get_or_create` / `update_or_create` must run their existence probe (and
//! the post-update re-fetch) on the WRITE database, not a read replica.
//! Otherwise a split router probes a not-yet-replicated replica, misses a
//! just-written row, and inserts a duplicate. Regression guard for the final-
//! review follow-up on the DatabaseRouter foundation (gaps2 #69).

#![allow(dead_code)]

use umbra::db::{Alias, DatabaseRouter, RouteContext};
use umbra::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "rwu_widget")]
pub struct Widget {
    pub id: i64,
    pub slug: String,
    pub label: String,
}

/// Reads → the (empty) replica, writes → default. With the bug, the existence
/// probe reads the empty replica and a duplicate is created.
struct SplitRouter;
impl DatabaseRouter for SplitRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new("replica")
    }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new("default")
    }
}

async fn make_pool() -> sqlx::SqlitePool {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    sqlx::query(
        "CREATE TABLE rwu_widget (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             slug TEXT NOT NULL UNIQUE,\
             label TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create rwu_widget");
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn upsert_existence_probe_uses_write_db_not_replica() {
    let default = make_pool().await;
    let replica = make_pool().await;

    // Seed the WRITE (default) pool only; the replica stays empty — simulating
    // a row that hasn't replicated yet.
    sqlx::query("INSERT INTO rwu_widget (slug, label) VALUES ('alpha', 'Alpha')")
        .execute(&default)
        .await
        .expect("seed write db");

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings"))
        .database("default", default.clone())
        .database("replica", replica)
        .router(SplitRouter)
        .model::<Widget>()
        .build()
        .expect("App::build");

    // get_or_create: must FIND the seeded row via the write DB, not miss it on
    // the empty replica and insert a duplicate.
    let (row, created) = Widget::objects()
        .get_or_create(
            widget::SLUG.eq("alpha"),
            Widget {
                id: 0,
                slug: "alpha".into(),
                label: "Dup".into(),
            },
        )
        .await
        .expect("get_or_create");
    assert!(
        !created,
        "must find the existing row on the write DB, not create a duplicate"
    );
    assert_eq!(row.slug, "alpha");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rwu_widget")
        .fetch_one(&default)
        .await
        .expect("count");
    assert_eq!(count, 1, "no duplicate inserted on the write DB");

    // update_or_create: must find + UPDATE the seeded row (created=false), and
    // its re-fetch must read the updated row back from the write DB.
    let (updated, created2) = Widget::objects()
        .update_or_create(
            widget::SLUG.eq("alpha"),
            Widget {
                id: 0,
                slug: "alpha".into(),
                label: "Updated".into(),
            },
        )
        .await
        .expect("update_or_create");
    assert!(
        !created2,
        "must update the existing row, not insert a new one"
    );
    assert_eq!(
        updated.label, "Updated",
        "re-fetch must read the updated row from the write DB"
    );

    let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rwu_widget")
        .fetch_one(&default)
        .await
        .expect("count");
    assert_eq!(count2, 1, "still exactly one row after update_or_create");
}
