//! gaps4 #12 — a subscription is a read, and the SSE/WS transports must carry
//! the SAME per-request context (identity + private-column unlocks) the POST
//! query path does. Before the fix both transports injected only a default,
//! empty context: an authenticated caller's `private` unlock silently stayed
//! locked over a socket, and (worse) the loaders weren't keyed to the caller.
//!
//! This drives the ACTUAL `/graphql/sse` route — not `execute_stream` directly —
//! because the bug lived in the route wiring, not the resolver. Own binary:
//! `App::build` publishes settings/registry into process-wide `OnceLock`s.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sc_ticker")]
pub struct ScTicker {
    pub id: i64,
    pub symbol: String,
    /// Confidential, but an authenticated caller legitimately sees it. Over SSE
    /// this must obey the same unlock the POST path does.
    #[umbral(private)]
    pub cost: String,
}

/// Identity from any `x-user` header — presence is all this test needs.
struct HeaderUser;

#[async_trait::async_trait]
impl umbral::auth::Authentication for HeaderUser {
    async fn authenticate(
        &self,
        headers: &umbral::web::HeaderMap,
    ) -> Option<umbral::auth::Identity> {
        let id = headers.get("x-user")?.to_str().ok()?.to_string();
        Some(umbral::auth::Identity {
            user_id: id,
            is_staff: false,
            is_superuser: false,
            extras: Default::default(),
        })
    }
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sc.sqlite");
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

        let app = umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .model::<ScTicker>()
            .plugin(
                GraphqlPlugin::new()
                    .expose("sc_ticker")
                    .subscribable("sc_ticker")
                    // `cost` is private; any authenticated caller unlocks it.
                    .allow_private_if("sc_ticker", "cost", |id| id.is_some())
                    .authenticate(HeaderUser),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        sqlx::query("INSERT INTO sc_ticker (id, symbol, cost) VALUES (1, 'ACME', '42.00')")
            .execute(&p)
            .await
            .expect("seed");
        app.into_router()
    })
    .await
    .clone()
}

/// Open the SSE subscription as `user`, publish a change once the stream is
/// live, and return the first pushed `data:` event as JSON.
///
/// The resolver subscribes to the bus only when the stream is first polled —
/// which, over SSE, happens when the response body is read. So we start reading
/// and only THEN (after a beat) publish, exactly the open-then-write race a real
/// client has.
async fn first_sse_event(user: Option<&str>, query: &str) -> serde_json::Value {
    let body = serde_json::json!({ "query": query }).to_string();
    let mut req = Request::builder()
        .method("POST")
        .uri("/graphql/sse")
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-user", u);
    }
    let res = boot()
        .await
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the SSE route should answer 200"
    );

    let mut stream = res.into_body().into_data_stream();

    // Publish once the subscription has had a moment to attach to the bus.
    let publish = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        umbral_graphql::publish_for_tests("sc_ticker", "1", false);
    });

    // Accumulate body chunks until a full `data: {...}` SSE frame arrives.
    let read = async {
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.expect("body chunk");
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(line) = buf.lines().find(|l| l.starts_with("data:")) {
                let json = line.trim_start_matches("data:").trim();
                if !json.is_empty() {
                    return serde_json::from_str::<serde_json::Value>(json)
                        .expect("SSE data frame is JSON");
                }
            }
        }
        panic!("SSE stream ended before any data frame");
    };

    let out = tokio::time::timeout(std::time::Duration::from_secs(5), read)
        .await
        .expect("no SSE event within 5s");
    publish.await.expect("publish task");
    out
}

/// An authenticated caller unlocks the `private` column over SSE — proving the
/// transport now resolves identity and injects the caller's unlocks.
#[tokio::test]
async fn authenticated_sse_subscriber_sees_the_unlocked_private_column() {
    let out = first_sse_event(
        Some("7"),
        r#"subscription { sc_tickerChanged { symbol cost } }"#,
    )
    .await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["sc_tickerChanged"]["symbol"], "ACME");
    assert_eq!(
        out["data"]["sc_tickerChanged"]["cost"], "42.00",
        "an authenticated SSE caller must receive the unlocked private column: {out}"
    );
}

/// An anonymous caller over the same SSE route gets the private column redacted
/// to null — the default, empty context is the safe direction to be wrong in.
#[tokio::test]
async fn anonymous_sse_subscriber_gets_the_private_column_redacted() {
    let out = first_sse_event(None, r#"subscription { sc_tickerChanged { symbol cost } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["sc_tickerChanged"]["symbol"], "ACME");
    assert!(
        out["data"]["sc_tickerChanged"]["cost"].is_null(),
        "an anonymous SSE caller must NOT receive the private column: {out}"
    );
}
