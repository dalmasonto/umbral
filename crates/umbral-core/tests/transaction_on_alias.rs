//! audit_2 core-app-config #5 — `begin_for(alias)` / `transaction_on(alias)`
//! run a transaction against the NAMED pool, not always `"default"`. Without
//! them, `transaction()` + `on_tx` silently writes a replica/tenant-routed
//! model to the default database.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

#[derive(Debug)]
#[allow(dead_code)] // variants carry the error for Debug on unexpected failure
enum TxError {
    Sqlx(sqlx::Error),
    Write(umbral::orm::write::WriteError),
}
impl From<sqlx::Error> for TxError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}
impl From<umbral::orm::write::WriteError> for TxError {
    fn from(e: umbral::orm::write::WriteError) -> Self {
        Self::Write(e)
    }
}

async fn mk_pool(name: &str) -> sqlx::SqlitePool {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join(format!("{name}.sqlite"));
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");
    pool
}

async fn count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query("SELECT COUNT(*) AS n FROM widget")
        .fetch_one(pool)
        .await
        .expect("count")
        .get::<i64, _>("n")
}

#[tokio::test(flavor = "multi_thread")]
async fn begin_for_and_transaction_on_target_the_named_pool() {
    let default_pool = mk_pool("default").await;
    let secondary_pool = mk_pool("secondary").await;
    // Keep handles to assert against each pool directly (SqlitePool is Arc —
    // the clone shares the one underlying connection, so it sees the same rows).
    let default_handle = default_pool.clone();
    let secondary_handle = secondary_pool.clone();

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbral::App::builder()
        .settings(settings)
        .database("default", default_pool)
        .database("secondary", secondary_pool)
        .model::<Widget>()
        .build()
        .expect("App::build");

    // The schema comes from the models, on EVERY registered alias — a replica with
    // no tables is not a replica.
    umbral_core::migrate::create_tables_for_tests_on("default")
        .await
        .expect("create the test schema on `default`");
    umbral_core::migrate::create_tables_for_tests_on("secondary")
        .await
        .expect("create the test schema on `secondary`");

    // (1) begin_for("secondary") — the manual-control primitive.
    let mut tx = umbral::db::begin_for("secondary")
        .await
        .expect("begin_for secondary");
    Widget::objects()
        .on_tx(&mut tx)
        .create(Widget {
            id: 0,
            name: "via-begin-for".into(),
        })
        .await
        .expect("create on secondary tx");
    tx.commit().await.expect("commit");

    // (2) transaction_on("secondary") — the closure helper.
    umbral::db::transaction_on("secondary", |tx| {
        Box::pin(async move {
            Widget::objects()
                .on_tx(tx)
                .create(Widget {
                    id: 0,
                    name: "via-transaction-on".into(),
                })
                .await?;
            Ok::<_, TxError>(())
        })
    })
    .await
    .expect("transaction_on secondary");

    // Both rows landed in SECONDARY; DEFAULT is untouched. Before begin_for
    // existed, both would have gone to the default pool.
    assert_eq!(
        count(&secondary_handle).await,
        2,
        "both writes must land in the `secondary` pool"
    );
    assert_eq!(
        count(&default_handle).await,
        0,
        "the `default` pool must be untouched — no silent wrong-DB write"
    );
}
