//! Task 7 — `DynQuerySet` routes through `DatabaseRouter`.
//!
//! `DefaultRouter` returns the model's `database` alias when set, so a
//! model tagged `#[umbra(database = "analytics")]` should read from and
//! write to the `"analytics"` pool, not the `"default"` pool. This test
//! proves that `DynQuerySet::count()` (and by extension every dynamic
//! terminal that was previously pinned to `pool_dispatched()`) now
//! respects the per-model alias.
//!
//! Strategy: insert ONE row directly into the `analytics` pool, leave
//! `default` empty. If `count()` returns 1 the router sent the query to
//! `analytics`; if it returns 0 the old bug is alive.

use serde::{Deserialize, Serialize};
use umbra::orm::DynQuerySet;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dyn_event", database = "analytics")]
pub struct Event {
    pub id: i64,
    pub name: String,
}

async fn make_pool(table: &str) -> sqlx::SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:").await.unwrap();
    let ddl =
        format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)");
    sqlx::query(&ddl).execute(&pool).await.unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn dyn_count_routes_to_analytics_pool() {
    let default = make_pool("dyn_event").await;
    let analytics = make_pool("dyn_event").await;

    // Seed ONE row into `analytics` only.
    sqlx::query("INSERT INTO dyn_event (name) VALUES ('boom')")
        .execute(&analytics)
        .await
        .unwrap();

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings load"))
        .database("default", default)
        .database("analytics", analytics)
        // No custom router — DefaultRouter routes `database = "analytics"`
        // models to the analytics pool.
        .model::<Event>()
        .build()
        .unwrap();

    let meta = umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "dyn_event")
        .expect("Event model registered");

    // DynQuerySet::count() must hit `analytics` (the pool that has the row),
    // not `default` (which is empty). Before the fix this returns 0.
    let n = DynQuerySet::for_meta(&meta).count().await.expect("count");

    assert_eq!(
        n, 1,
        "DynQuerySet::count() should route to the analytics pool (1 row), not default (0 rows)"
    );
}
