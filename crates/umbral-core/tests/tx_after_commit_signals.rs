//! gaps6 #14/#15 — `on_tx()` writes (`create`, `update_values`, `delete`) fire
//! their signals AFTER the transaction commits, not inline mid-transaction.
//!
//! A naive fix that emits inline is a phantom-signal-on-rollback bug: side
//! effects fire, then the tx rolls back and the row never existed. The safe
//! design buffers each write's signal on the `Transaction` and the four
//! `db::transaction*` runners flush the buffer only after a successful
//! commit — never on rollback.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;

use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tx_note")]
pub struct TxNote {
    pub id: i64,
    pub body: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tx_secret")]
pub struct TxSecret {
    pub id: i64,
    pub name: String,
    #[umbral(signal_skip)]
    pub secret: String,
}

#[derive(Debug)]
struct BoomError;
impl std::fmt::Display for BoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boom")
    }
}
impl std::error::Error for BoomError {}
impl From<sqlx::Error> for BoomError {
    fn from(_: sqlx::Error) -> Self {
        BoomError
    }
}
impl From<umbral::orm::write::WriteError> for BoomError {
    fn from(_: umbral::orm::write::WriteError) -> Self {
        BoomError
    }
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tx_after_commit_signals.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<TxNote>()
            .model::<TxSecret>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await
    .clone()
}

/// A tx `create()` must fire `post_save:<table>` — but only AFTER the
/// transaction commits. A subscriber that reads the row back must find it
/// already committed, proving the signal didn't fire mid-transaction.
#[tokio::test]
async fn tx_create_fires_post_save_after_commit() {
    let _g = lock().lock().await;
    let pool = boot().await;
    umbral::signals::clear_for_tests();

    let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_save:tx_note", move |payload| {
        sink.lock().unwrap().push(payload.clone());
    });

    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxNote::objects()
                .on_tx(tx)
                .create(TxNote {
                    id: 0,
                    body: "committed".into(),
                })
                .await?;
            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "on_tx().create() must fire post_save exactly once after commit; got {got:?}"
    );
    assert_eq!(got[0]["instance"]["body"], "committed");

    let row_count = TxNote::objects()
        .filter(tx_note::BODY.eq("committed"))
        .count()
        .await
        .expect("count");
    assert_eq!(
        row_count, 1,
        "the row must already be visible outside the tx by the time the \
         subscriber runs — proves after-commit ordering, not just after-return"
    );
}

/// The whole point: a tx `create()` that then ROLLS BACK must fire ZERO
/// signals. If the buffer isn't dropped-on-rollback, the subscriber sees a
/// phantom row that was never actually persisted.
#[tokio::test]
async fn tx_create_fires_nothing_on_rollback() {
    let _g = lock().lock().await;
    let pool = boot().await;
    umbral::signals::clear_for_tests();

    let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_save:tx_note", move |payload| {
        sink.lock().unwrap().push(payload.clone());
    });

    let result: Result<(), BoomError> = db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxNote::objects()
                .on_tx(tx)
                .create(TxNote {
                    id: 0,
                    body: "rolled-back".into(),
                })
                .await?;
            Err(BoomError)
        })
    })
    .await;

    assert!(result.is_err(), "the closure's Err must propagate");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        0,
        "a rolled-back tx must fire NO signals — the buffer is dropped, not \
         flushed; got {got:?}"
    );

    let row_count = TxNote::objects()
        .filter(tx_note::BODY.eq("rolled-back"))
        .count()
        .await
        .expect("count");
    assert_eq!(row_count, 0, "the row must genuinely not exist");
}

/// A tx `delete()` must fire the per-row `post_delete` after commit, and
/// nothing on rollback — same contract as create.
#[tokio::test]
async fn tx_delete_fires_post_delete_after_commit_not_on_rollback() {
    let _g = lock().lock().await;
    let pool = boot().await;
    umbral::signals::clear_for_tests();

    let row = TxNote::objects()
        .create(TxNote {
            id: 0,
            body: "to-delete".into(),
        })
        .await
        .expect("seed row");

    let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_delete:tx_note", move |payload| {
        sink.lock().unwrap().push(payload.clone());
    });

    let row_id = row.id;
    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxNote::objects()
                .filter(tx_note::ID.eq(row_id))
                .on_tx(tx)
                .delete()
                .await?;
            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "on_tx().delete() must fire post_delete exactly once after commit; got {got:?}"
    );

    // Now prove rollback fires nothing, on a second row.
    let row2 = TxNote::objects()
        .create(TxNote {
            id: 0,
            body: "to-delete-2".into(),
        })
        .await
        .expect("seed row 2");
    seen.lock().unwrap().clear();

    let row2_id = row2.id;
    let result: Result<(), BoomError> = db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxNote::objects()
                .filter(tx_note::ID.eq(row2_id))
                .on_tx(tx)
                .delete()
                .await?;
            Err(BoomError)
        })
    })
    .await;
    assert!(result.is_err());

    let got2 = seen.lock().unwrap().clone();
    assert_eq!(
        got2.len(),
        0,
        "a rolled-back tx delete must fire NO post_delete; got {got2:?}"
    );
    let still_there = TxNote::objects()
        .filter(tx_note::ID.eq(row2_id))
        .count()
        .await
        .expect("count");
    assert_eq!(
        still_there, 1,
        "the row must still exist — delete rolled back"
    );
}

/// A tx `update_values()` must fire `bulk_post_save` after commit.
#[tokio::test]
async fn tx_update_values_fires_bulk_post_save_after_commit() {
    let _g = lock().lock().await;
    let pool = boot().await;
    umbral::signals::clear_for_tests();

    let row = TxNote::objects()
        .create(TxNote {
            id: 0,
            body: "before".into(),
        })
        .await
        .expect("seed row");

    let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("bulk_post_save:tx_note", move |payload| {
        sink.lock().unwrap().push(payload.clone());
    });

    let row_id = row.id;
    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let mut values = serde_json::Map::new();
            values.insert("body".into(), serde_json::json!("after"));
            TxNote::objects()
                .filter(tx_note::ID.eq(row_id))
                .on_tx(tx)
                .update_values(values)
                .await?;
            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "on_tx().update_values() must fire bulk_post_save once after commit; got {got:?}"
    );
    assert_eq!(got[0]["ids"], serde_json::json!([row_id]));
}

/// `#[umbral(signal_skip)]` fields must be redacted from BOTH the tx create
/// post_save payload and the tx delete post_delete payload — same
/// contract as the non-tx path (gaps6 #14's leak fix), just proven here for
/// the buffered after-commit path too.
#[tokio::test]
async fn tx_writes_redact_signal_skip_fields() {
    let _g = lock().lock().await;
    let pool = boot().await;
    umbral::signals::clear_for_tests();

    let saves = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = saves.clone();
    umbral::signals::subscribe("post_save:tx_secret", move |payload| {
        sink.lock().unwrap().push(payload.clone());
    });
    let deletes = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink2 = deletes.clone();
    umbral::signals::subscribe("post_delete:tx_secret", move |payload| {
        sink2.lock().unwrap().push(payload.clone());
    });

    let created = db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let row = TxSecret::objects()
                .on_tx(tx)
                .create(TxSecret {
                    id: 0,
                    name: "visible".into(),
                    secret: "topsecret".into(),
                })
                .await?;
            Ok::<_, BoomError>(row)
        })
    })
    .await
    .expect("transaction commits");

    let save_payloads = saves.lock().unwrap().clone();
    assert_eq!(save_payloads.len(), 1);
    let instance = &save_payloads[0]["instance"];
    assert_eq!(instance["name"], "visible");
    assert!(
        instance.get("secret").is_none(),
        "signal_skip field must be absent from tx create payload: {instance:?}"
    );
    assert!(
        !serde_json::to_string(&save_payloads[0])
            .unwrap()
            .contains("topsecret")
    );

    let created_id = created.id;
    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxSecret::objects()
                .filter(tx_secret::ID.eq(created_id))
                .on_tx(tx)
                .delete()
                .await?;
            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let delete_payloads = deletes.lock().unwrap().clone();
    assert_eq!(delete_payloads.len(), 1);
    let deleted_instance = &delete_payloads[0]["instance"];
    assert!(
        deleted_instance.get("secret").is_none(),
        "signal_skip field must be absent from tx delete payload: {deleted_instance:?}"
    );
    assert!(
        !serde_json::to_string(&delete_payloads[0])
            .unwrap()
            .contains("topsecret")
    );
}
