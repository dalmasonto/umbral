//! Cursor pagination.
//!
//! The headline test is `a_cursor_survives_a_write_that_offset_would_not`, which paginates
//! across a concurrent write and asserts nothing is skipped or repeated. Everything else here
//! exists to stop that one from passing for the wrong reason.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::orm::Model;
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "pg_item")]
pub struct PgItem {
    pub id: i64,
    pub name: String,
    /// Deliberately NOT unique across rows — several items share a rank. Sorting by this
    /// alone would let tied rows straddle a page boundary, which is what the pk tie-break in
    /// the keyset predicate exists to prevent.
    pub rank: i64,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pg.sqlite");
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
            .model::<PgItem>()
            .plugin(GraphqlPlugin::new().expose("pg_item").mutable("pg_item"))
            .build()
            .expect("App::build");

        let p = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE pg_item (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             rank INTEGER NOT NULL)",
        )
        .execute(&p)
        .await
        .expect("ddl");
        // Ten rows, five distinct ranks — every rank is a tie.
        for i in 1..=10 {
            sqlx::query("INSERT INTO pg_item (name, rank) VALUES (?, ?)")
                .bind(format!("item-{i:02}"))
                .bind((i + 1) / 2)
                .execute(&p)
                .await
                .expect("seed");
        }
        app.into_router()
    })
    .await
    .clone()
}

async fn gql(query: &str) -> serde_json::Value {
    let body = serde_json::json!({ "query": query }).to_string();
    let res = boot()
        .await
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Walk a connection to exhaustion, returning every name in order.
async fn page_through(first: usize, order_by: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let args = match &after {
            Some(c) => format!(r#"first: {first}, orderBy: "{order_by}", after: "{c}""#),
            None => format!(r#"first: {first}, orderBy: "{order_by}""#),
        };
        let out = gql(&format!(
            "{{ pg_itemsConnection({args}) {{ edges {{ node {{ name }} }} pageInfo {{ hasNextPage endCursor }} }} }}"
        ))
        .await;
        assert!(out.get("errors").is_none(), "{out}");
        let conn = &out["data"]["pg_itemsConnection"];
        for e in conn["edges"].as_array().unwrap() {
            names.push(e["node"]["name"].as_str().unwrap().to_string());
        }
        if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap() {
            break;
        }
        after = Some(
            conn["pageInfo"]["endCursor"]
                .as_str()
                .expect("hasNextPage means there IS an endCursor")
                .to_string(),
        );
        assert!(names.len() < 100, "paging did not terminate");
    }
    names
}

/// Paging to the end visits every row exactly once, and in order.
#[tokio::test]
async fn paging_visits_every_row_exactly_once() {
    let _g = lock().lock().await;
    let names = page_through(3, "id").await;
    assert_eq!(names.len(), 10, "{names:?}");

    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 10, "a row was repeated: {names:?}");
    assert_eq!(names[0], "item-01");
    assert_eq!(names[9], "item-10");
}

/// **The reason cursors exist.** Page 1, then delete a row from behind the boundary, then
/// page 2.
///
/// With `OFFSET 3`, removing an earlier row shifts every later row up by one — so page 2
/// starts at what is NOW row 4, which is `item-05`. `item-04` is never served to anyone.
/// Nothing errors. The client quietly receives a list with a hole in it.
///
/// A cursor is a *position in the ordering*, not a count, so a write behind it cannot move
/// it. Page 2 still starts at `item-04`.
#[tokio::test]
async fn a_cursor_survives_a_write_that_offset_would_not() {
    let _g = lock().lock().await;

    // Page 1: the three lowest ids.
    let out = gql(
        r#"{ pg_itemsConnection(first: 3, orderBy: "id") { edges { node { name } } pageInfo { endCursor } } }"#,
    )
    .await;
    let conn = &out["data"]["pg_itemsConnection"];
    let page1: Vec<String> = conn["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(page1, ["item-01", "item-02", "item-03"]);
    let cursor = conn["pageInfo"]["endCursor"].as_str().unwrap().to_string();

    // Somebody writes. Ordering by id, a NEW row sorts last and cannot disturb an earlier
    // page — so the write that actually breaks OFFSET is one that removes a row from BEHIND
    // the boundary, shifting every later row up by one.
    let out = gql(r#"mutation { deletePgItem(id: "1") }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(
        out["data"]["deletePgItem"], true,
        "the delete must actually happen, or this test proves nothing"
    );

    // Page 2, resumed from the cursor. item-04 must be next: the delete happened BEFORE the
    // boundary, so with OFFSET 3 the database would now hand back item-05 and item-04 would
    // be lost forever.
    let out = gql(&format!(
        r#"{{ pg_itemsConnection(first: 3, orderBy: "id", after: "{cursor}") {{ edges {{ node {{ name }} }} }} }}"#
    ))
    .await;
    assert!(out.get("errors").is_none(), "{out}");
    let page2: Vec<String> = out["data"]["pg_itemsConnection"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        page2,
        ["item-04", "item-05", "item-06"],
        "the cursor must resume where it left off, regardless of what was deleted behind it"
    );

    // put it back for the other tests
    let p = umbral::db::pool();
    sqlx::query("INSERT OR IGNORE INTO pg_item (id, name, rank) VALUES (1, 'item-01', 1)")
        .execute(&p)
        .await
        .expect("restore");
}

/// Ties must not straddle the page boundary.
///
/// `rank` repeats (two rows per rank). Sorting by `rank` alone, the row at the boundary is
/// ambiguous — the database may return either of the tied rows, and paging can serve one
/// twice and skip the other. The keyset predicate breaks the tie on the primary key, so the
/// ordering is total and the boundary is exact.
#[tokio::test]
async fn tied_sort_values_do_not_straddle_the_page_boundary() {
    let _g = lock().lock().await;
    // page size 1 across 10 rows with 5 duplicate rank values — every page boundary lands on
    // or next to a tie.
    let names = page_through(1, "rank").await;
    assert_eq!(names.len(), 10, "{names:?}");

    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        10,
        "a tied row was served twice or skipped: {names:?}"
    );
}

/// A cursor is a position in ONE ordering. Replaying it under a different `orderBy` would
/// silently return the wrong window, so it is refused rather than guessed at.
#[tokio::test]
async fn a_cursor_from_another_ordering_is_refused() {
    let _g = lock().lock().await;
    let out =
        gql(r#"{ pg_itemsConnection(first: 2, orderBy: "id") { pageInfo { endCursor } } }"#).await;
    let cursor = out["data"]["pg_itemsConnection"]["pageInfo"]["endCursor"]
        .as_str()
        .unwrap()
        .to_string();

    let out = gql(&format!(
        r#"{{ pg_itemsConnection(first: 2, orderBy: "rank", after: "{cursor}") {{ edges {{ node {{ name }} }} }} }}"#
    ))
    .await;
    assert!(
        out["errors"].to_string().contains("different `orderBy`"),
        "a cursor from another ordering must be an error, not a guess: {out}"
    );
}

/// Garbage in the cursor is an error, not a panic and not a silent first page.
#[tokio::test]
async fn a_malformed_cursor_is_rejected() {
    let _g = lock().lock().await;
    let out = gql(
        r#"{ pg_itemsConnection(first: 2, after: "not-a-cursor") { edges { node { name } } } }"#,
    )
    .await;
    assert!(
        out["errors"].to_string().contains("malformed cursor"),
        "{out}"
    );
}

/// You cannot order by a column that is not in the schema. Beyond the obvious, a cursor built
/// on a hidden column would carry its values back out inside the cursor — and base64 is not
/// encryption.
#[tokio::test]
async fn ordering_by_an_unknown_column_is_refused() {
    let _g = lock().lock().await;
    let out = gql(
        r#"{ pg_itemsConnection(first: 2, orderBy: "nonexistent") { edges { node { name } } } }"#,
    )
    .await;
    assert!(
        out["errors"].to_string().contains("cannot order by"),
        "{out}"
    );
}
