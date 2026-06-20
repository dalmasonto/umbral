//! gaps2 #88b — `set_junction_dynamic` / `load_junction_selection` route
//! through `DatabaseRouter` using the parent model's alias.
//!
//! Strategy: install a read/write-split router (reads → "replica", writes →
//! "default"), then show that:
//!   a) `set_junction_dynamic(..., Some("JnParent"))` writes to "default"
//!   b) `load_junction_selection(..., Some("JnParent"))` reads from "replica"
//!      (which is empty), not from "default" (which holds the row we wrote)
//!
//! The divergence between what `default` holds and what `replica` returns is
//! the proof — it's the same technique used in `router_read_write_split.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};

use umbra::db::{Alias, DatabaseRouter, RouteContext};
use umbra::migrate::ModelMeta;
use umbra::orm::SqlType;

/// A parent model whose name the router can look up in the registry.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "jn_parent")]
pub struct JnParent {
    pub id: i64,
    pub name: String,
}

static READS: AtomicUsize = AtomicUsize::new(0);
static WRITES: AtomicUsize = AtomicUsize::new(0);

struct JnSplitRouter;
impl DatabaseRouter for JnSplitRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        READS.fetch_add(1, Ordering::SeqCst);
        Alias::new("replica")
    }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        WRITES.fetch_add(1, Ordering::SeqCst);
        Alias::new("default")
    }
}

async fn make_pool_with_junction() -> sqlx::SqlitePool {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap();
    // The junction table the helpers will operate on.
    sqlx::query(
        "CREATE TABLE jn_parent_tags (\
            parent_id INTEGER NOT NULL, \
            child_id  INTEGER NOT NULL, \
            PRIMARY KEY (parent_id, child_id)\
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn junction_write_routes_to_write_pool_and_read_routes_to_read_pool() {
    // Two separate pools. "default" gets the junction table; "replica" is
    // empty (just the schema). Both are in-memory so their data never leaks
    // across test runs.
    let default_pool = make_pool_with_junction().await;
    let replica_pool = make_pool_with_junction().await;

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings load"))
        .database("default", default_pool.clone())
        .database("replica", replica_pool.clone())
        .router(JnSplitRouter)
        .model::<JnParent>()
        .build()
        .unwrap();

    let parent_id = sea_query::Value::BigInt(Some(1));
    let child_id = sea_query::Value::BigInt(Some(42));

    // --- write: should route to "default" ---
    let writes_before = WRITES.load(Ordering::SeqCst);
    umbra::orm::set_junction_dynamic(
        "jn_parent_tags",
        parent_id.clone(),
        vec![child_id],
        Some("JnParent"),
    )
    .await
    .expect("set_junction_dynamic");

    assert!(
        WRITES.load(Ordering::SeqCst) > writes_before,
        "set_junction_dynamic must consult db_for_write"
    );

    // The row must be in "default".
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jn_parent_tags WHERE parent_id = 1 AND child_id = 42")
            .fetch_one(&default_pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "row written to the default (write) pool");

    // --- read: should route to "replica" (empty) ---
    let reads_before = READS.load(Ordering::SeqCst);
    let selected = umbra::orm::load_junction_selection(
        "jn_parent_tags",
        parent_id,
        SqlType::BigInt,
        Some("JnParent"),
    )
    .await
    .expect("load_junction_selection");

    assert!(
        READS.load(Ordering::SeqCst) > reads_before,
        "load_junction_selection must consult db_for_read"
    );
    assert!(
        selected.is_empty(),
        "load_junction_selection read from the empty replica pool, not default"
    );
}
