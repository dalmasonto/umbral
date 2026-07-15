//! Subscriptions.
//!
//! Driven through the schema's `execute_stream` — the same code path both the WebSocket and
//! the SSE transport run on, so what these tests prove is true of both.
//!
//! The one that matters is `a_pushed_row_is_redacted_like_any_other_read`. The ORM's signal
//! payload is a serde dump of the model and knows nothing about `private` / `secret`, so the
//! easy implementation — forward the payload — leaks every protected column over the socket.
//! A subscription is a read. The socket is a transport, not an exemption.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::{DynQuerySet, Model};
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "sb_ticker")]
pub struct SbTicker {
    pub id: i64,
    pub symbol: String,
    pub price: String,
    /// The subscriber must never see this, even though the signal payload contains it.
    #[umbral(secret)]
    pub internal_book: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sb.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .model::<SbTicker>()
            .plugin(
                GraphqlPlugin::new()
                    .expose("sb_ticker")
                    .mutable("sb_ticker")
                    .subscribable("sb_ticker"),
            )
            .build()
            .expect("build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        sqlx::query(
            "INSERT INTO sb_ticker (id, symbol, price, internal_book) VALUES \
             (1, 'ACME', '10.00', 'do-not-leak')",
        )
        .execute(&p)
        .await
        .expect("seed");
    })
    .await;
}

fn schema() -> async_graphql::dynamic::Schema {
    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "sb_ticker")
        .expect("registered");
    umbral_graphql::build_schema_for_tests(&[umbral_graphql::Exposed {
        meta,
        access: None,
        hidden: Vec::new(),
        writable: None,
        private_unlocks: Vec::new(),
        subscribable: true,
        owner_field: None,
    }])
    .expect("schema")
}

/// Subscribe, cause a change, and take the first pushed value.
///
/// The stream must be POLLED before the event fires: the resolver — and therefore the
/// `bus().subscribe()` inside it — does not run until then, so an event published beforehand
/// has nobody listening for it. That is not a test artefact; it is the same race a real client
/// has between opening a socket and the first write landing, and it is why a client that
/// needs to miss nothing must read its initial state *after* subscribing, not before.
async fn first_event(query: &str, cause: impl FnOnce()) -> serde_json::Value {
    let schema = schema();
    let mut stream = schema.execute_stream(async_graphql::Request::new(query));

    // Drive the stream on its own task so it is actually subscribed...
    let handle = tokio::spawn(async move { stream.next().await });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // ...and only then cause the change.
    cause();

    let res = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("subscription never pushed anything")
        .expect("subscriber task panicked")
        .expect("stream ended without an event");
    serde_json::to_value(res).expect("response as json")
}

/// **The one that matters.** The pushed row goes through the same redaction as any read.
///
/// The signal payload contains `internal_book` — it is a serde dump of the model. If this
/// test ever fails, the implementation is forwarding that payload instead of re-reading the
/// row, and every `private` / `secret` column in the app is going out over the socket.
#[tokio::test]
async fn a_pushed_row_is_redacted_like_any_other_read() {
    let _g = lock().lock().await;
    boot().await;

    let out = first_event(
        r#"subscription { sb_tickerChanged { symbol price } }"#,
        || umbral_graphql::publish_for_tests("sb_ticker", "1", false),
    )
    .await;

    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["sb_tickerChanged"]["symbol"], "ACME");

    let whole = out.to_string();
    assert!(
        !whole.contains("do-not-leak") && !whole.contains("internal_book"),
        "a secret column reached a subscriber — the signal payload is being forwarded \
         instead of the row being re-read: {whole}"
    );
}

/// A real write — not a synthetic publish — reaches a subscriber.
///
/// This is what wires the whole thing together: the ORM fires `post_save`, the plugin
/// republishes the primary key, and the subscriber gets the row. It also means a write from
/// REST, the admin, or a background task reaches subscribers, since they all fire the same
/// signal.
#[tokio::test]
async fn an_ordinary_write_reaches_a_subscriber() {
    let _g = lock().lock().await;
    boot().await;
    umbral_graphql::wire_signals_for_tests("sb_ticker");

    let schema = schema();
    let mut stream = schema.execute_stream(async_graphql::Request::new(
        r#"subscription { sb_tickerChanged { symbol price } }"#,
    ));
    let handle = tokio::spawn(async move { stream.next().await });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // An UPDATE through the ORM — the same call REST or the admin would make.
    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "sb_ticker")
        .unwrap();
    let mut body = serde_json::Map::new();
    body.insert("price".into(), serde_json::json!("11.50"));
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "1")
        .update_json(&body)
        .await
        .expect("update");

    let res = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("an ORM write must reach subscribers")
        .expect("subscriber task panicked")
        .expect("stream ended");
    let out = serde_json::to_value(res).unwrap();
    assert_eq!(out["data"]["sb_tickerChanged"]["price"], "11.50", "{out}");
}

/// An `id` argument narrows the stream to one row. Without it, every subscriber to a busy
/// table wakes for every write in it.
#[tokio::test]
async fn the_id_argument_filters_the_stream() {
    let _g = lock().lock().await;
    boot().await;

    let schema = schema();
    let mut stream = schema.execute_stream(async_graphql::Request::new(
        r#"subscription { sb_tickerChanged(id: "1") { symbol } }"#,
    ));
    tokio::task::yield_now().await;

    // A change to a DIFFERENT row must not wake this subscriber...
    umbral_graphql::publish_for_tests("sb_ticker", "999", false);
    let got = tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
    assert!(got.is_err(), "a different row must not push to this stream");

    // ...but a change to the subscribed row must.
    umbral_graphql::publish_for_tests("sb_ticker", "1", false);
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("the subscribed row must push")
        .expect("stream ended");
    let out = serde_json::to_value(res).unwrap();
    assert_eq!(out["data"]["sb_tickerChanged"]["symbol"], "ACME");
}

/// A deleted row cannot be re-read, so the delete stream yields the ID.
///
/// Echoing the last-known row from the signal payload would be both a leak (see above) and a
/// lie — a "row" that no longer exists.
#[tokio::test]
async fn deletes_push_an_id_not_a_row() {
    let _g = lock().lock().await;
    boot().await;

    let out = first_event(r#"subscription { sb_tickerDeleted }"#, || {
        umbral_graphql::publish_for_tests("sb_ticker", "42", true)
    })
    .await;

    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["sb_tickerDeleted"], "42");
}

/// A model that did not opt in has no subscription field at all.
#[tokio::test]
async fn a_model_that_did_not_opt_in_is_not_subscribable() {
    let _g = lock().lock().await;
    boot().await;

    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "sb_ticker")
        .unwrap();
    // Same model, `subscribable: false`.
    let schema = umbral_graphql::build_schema_for_tests(&[umbral_graphql::Exposed {
        meta,
        access: None,
        hidden: Vec::new(),
        writable: None,
        private_unlocks: Vec::new(),
        subscribable: false,
        owner_field: None,
    }])
    .expect("schema");

    let sdl = schema.sdl();
    assert!(
        !sdl.contains("type Subscription"),
        "no model opted in, so there must be no Subscription root at all:\n{sdl}"
    );
}
