//! End-to-end coverage for the test helpers themselves. Builds tiny
//! routers and pools, walks them through the TestClient, asserts the
//! helpers do what the docs claim.

use axum::Router;
use axum::body::Body;
use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http::header::{COOKIE, HeaderValue, SET_COOKIE};
use serde::{Deserialize, Serialize};
use umbral_testing::{TempPool, TestClient};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Note {
    id: i64,
    body: String,
}

fn router() -> Router {
    Router::new()
        .route("/ping", get(|| async { "pong" }))
        .route(
            "/notes",
            get(|| async {
                Json(vec![
                    Note {
                        id: 1,
                        body: "hello".into(),
                    },
                    Note {
                        id: 2,
                        body: "world".into(),
                    },
                ])
            }),
        )
        .route("/echo", post(|Json(n): Json<Note>| async move { Json(n) }))
        .route(
            "/needs-auth",
            get(|headers: http::HeaderMap| async move {
                if headers.get("authorization") == Some(&HeaderValue::from_static("Bearer tok")) {
                    "ok".into_response()
                } else {
                    (StatusCode::UNAUTHORIZED, "denied").into_response()
                }
            }),
        )
        .route(
            "/set-cookie",
            get(|| async {
                let mut resp = Response::new(Body::from("set"));
                resp.headers_mut()
                    .insert(SET_COOKIE, HeaderValue::from_static("sid=abc123; Path=/"));
                resp
            }),
        )
        .route(
            "/who",
            get(|headers: http::HeaderMap| async move {
                let cookie = headers
                    .get(COOKIE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("(none)");
                format!("cookie:{cookie}")
            }),
        )
}

#[tokio::test]
async fn get_round_trip_returns_status_and_body() {
    let c = TestClient::new(router());
    let r = c.get("/ping").await;
    r.assert_status_ok().assert_body_contains("pong");
    assert_eq!(r.body_text(), "pong");
}

#[tokio::test]
async fn body_json_parses_the_response() {
    let c = TestClient::new(router());
    let r = c.get("/notes").await;
    r.assert_status_ok();
    let notes: Vec<Note> = r.body_json();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].body, "hello");
}

#[tokio::test]
async fn post_json_round_trip() {
    let c = TestClient::new(router());
    let payload = Note {
        id: 42,
        body: "round-trip".into(),
    };
    let r = c.post_json("/echo", &payload).await;
    r.assert_status_ok();
    let back: Note = r.body_json();
    assert_eq!(back, payload);
}

#[tokio::test]
async fn default_header_rides_on_subsequent_requests() {
    let c = TestClient::new(router());
    let unauth = c.get("/needs-auth").await;
    unauth.assert_status(StatusCode::UNAUTHORIZED);

    c.set_default_header(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer tok"),
    );
    let auth = c.get("/needs-auth").await;
    auth.assert_status_ok().assert_body_contains("ok");
}

#[tokio::test]
async fn cookie_jar_carries_cookies_across_requests() {
    let c = TestClient::new(router());
    let first = c.get("/set-cookie").await;
    first.assert_status_ok();
    assert_eq!(c.cookie("sid").as_deref(), Some("abc123"));

    let echoed = c.get("/who").await;
    echoed.assert_status_ok().assert_body_contains("sid=abc123");
}

#[tokio::test]
async fn temp_pool_builds_a_usable_sqlite_pool() {
    let p = TempPool::new().await;
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .execute(p.handle())
        .await
        .unwrap();
    sqlx::query("INSERT INTO t (v) VALUES (?)")
        .bind("hello")
        .execute(p.handle())
        .await
        .unwrap();
    let (got,): (String,) = sqlx::query_as("SELECT v FROM t WHERE id = 1")
        .fetch_one(p.handle())
        .await
        .unwrap();
    assert_eq!(got, "hello");
}

#[tokio::test]
async fn temp_pool_clone_handle_shares_state() {
    let p = TempPool::new().await;
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .execute(p.handle())
        .await
        .unwrap();
    let cloned = p.clone_handle();
    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM t")
        .fetch_one(&cloned)
        .await
        .unwrap();
    assert_eq!(row.0, 0);
}

#[tokio::test]
#[should_panic(expected = "expected status 200 OK, got 401 Unauthorized")]
async fn assert_status_ok_panics_with_a_useful_message() {
    let c = TestClient::new(router());
    let r = c.get("/needs-auth").await;
    r.assert_status_ok();
}

#[tokio::test]
async fn assert_header_matches_an_expected_value() {
    let c = TestClient::new(router());
    let r = c.get("/set-cookie").await;
    r.assert_header("set-cookie", "sid=abc123; Path=/");
}
