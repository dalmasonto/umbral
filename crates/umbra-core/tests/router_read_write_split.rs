//! Read/write-split router coverage: reads -> "replica", writes -> "default".
//! Exercises the #23 split across several terminals (fetch/count = read,
//! create/delete = write), proves ctx flows into the router, and proves
//! `.on(&pool)` is a HARD override that bypasses the router entirely.

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
async fn read_write_split_across_terminals_and_on_override() {
    let default = make_pool().await;
    let replica = make_pool().await;

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings load"))
        .database("default", default.clone())
        .database("replica", replica.clone())
        .router(SplitRouter)
        .model::<Widget>()
        .build()
        .unwrap();

    // create() is a WRITE -> "default".
    Widget::objects()
        .create(Widget {
            id: 0,
            name: "a".into(),
        })
        .await
        .unwrap();
    assert_eq!(WRITES.load(Ordering::SeqCst), 1, "create routed as a write");

    // fetch() is a READ -> "replica" (a separate, empty pool), so the write
    // above is invisible. That divergence proves the split.
    let rows = Widget::objects().fetch().await.unwrap();
    assert!(READS.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        rows.len(),
        0,
        "fetch routed to the empty replica, not default"
    );

    // `.on(&pool)` is a HARD override: it bypasses the router, so this read
    // hits `default` (where the write landed) and does NOT consult db_for_read.
    let reads_before = READS.load(Ordering::SeqCst);
    let pinned = Widget::objects().on(&default).fetch().await.unwrap();
    assert_eq!(
        pinned.len(),
        1,
        ".on() must bypass the router and read `default`"
    );
    assert_eq!(pinned[0].name, "a");
    assert_eq!(
        READS.load(Ordering::SeqCst),
        reads_before,
        ".on() must not consult db_for_read"
    );

    // count() is a READ -> "replica". Seed the replica directly with two rows
    // so the count is distinguishable from default's single row.
    sqlx::query("INSERT INTO rw_widget (name) VALUES ('r1'), ('r2')")
        .execute(&replica)
        .await
        .unwrap();
    let n = Widget::objects().count().await.unwrap();
    assert_eq!(
        n, 2,
        "count() routed to the replica (2 rows), not default (1 row)"
    );

    // delete() is a WRITE -> "default": it removes default's row and leaves the
    // replica untouched.
    let writes_before = WRITES.load(Ordering::SeqCst);
    Widget::objects()
        .filter(widget::NAME.eq("a"))
        .delete()
        .await
        .unwrap();
    assert!(
        WRITES.load(Ordering::SeqCst) > writes_before,
        "delete routed as a write"
    );
    let default_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rw_widget")
        .fetch_one(&default)
        .await
        .unwrap();
    assert_eq!(default_left, 0, "delete removed default's row");
    let replica_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rw_widget")
        .fetch_one(&replica)
        .await
        .unwrap();
    assert_eq!(replica_left, 2, "delete did not touch the replica");
}
