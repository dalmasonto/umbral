//! A custom router that splits reads -> "replica", writes -> "default" proves
//! the read/write seam (#23) and that ctx flows into the router.

use std::sync::atomic::{AtomicUsize, Ordering};

use umbra::db::{Alias, DatabaseRouter, RouteContext};
use umbra::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "rw_widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

static READS: AtomicUsize = AtomicUsize::new(0);
static WRITES: AtomicUsize = AtomicUsize::new(0);

struct SplitRouter;
impl DatabaseRouter for SplitRouter {
    fn db_for_read(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        READS.fetch_add(1, Ordering::SeqCst);
        Alias::new("replica")
    }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias {
        WRITES.fetch_add(1, Ordering::SeqCst);
        Alias::new("default")
    }
}

async fn make_pool() -> sqlx::SqlitePool {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE rw_widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_hit_replica_writes_hit_default() {
    let default = make_pool().await;
    let replica = make_pool().await;

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings load"))
        .database("default", default)
        .database("replica", replica)
        .router(SplitRouter)
        .model::<Widget>()
        .build()
        .unwrap();

    // A write goes to "default".
    Widget::objects()
        .create(Widget {
            id: 0,
            name: "a".into(),
        })
        .await
        .unwrap();
    assert_eq!(WRITES.load(Ordering::SeqCst), 1);

    // A read goes to "replica" -- a SEPARATE empty pool, so the write above is
    // invisible. That divergence proves the split.
    let rows = Widget::objects().fetch().await.unwrap();
    assert!(READS.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        rows.len(),
        0,
        "read routed to the empty replica, not default"
    );
}
