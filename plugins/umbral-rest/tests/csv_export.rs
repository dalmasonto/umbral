//! `?format=csv` list export (feature #61). A `GET /api/<table>/?format=csv`
//! downloads the full filtered set as CSV, capped at `MAX_LIST_ROWS` (1000).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct Widget {
    id: i64,
    name: String,
    qty: i32,
}

// One App per test-binary process: the global OnceLock inside App::build
// panics on a second call, so all tests in this file share a single boot
// seeded with 1 001 rows (one more than MAX_LIST_ROWS = 1 000).
static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("csv.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE widget \
             (id INTEGER PRIMARY KEY AUTOINCREMENT, \
              name TEXT NOT NULL, \
              qty  INTEGER NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create widget");

        // Seed the two hand-checked named rows first (IDs 1, 2) so they
        // always fall within MAX_LIST_ROWS and the header/quoting assertions
        // in list_exports_csv remain meaningful.
        sqlx::query("INSERT INTO widget (name, qty) VALUES (?, ?)")
            .bind("Anvil")
            .bind(3_i32)
            .execute(&pool)
            .await
            .expect("seed Anvil");
        sqlx::query("INSERT INTO widget (name, qty) VALUES (?, ?)")
            .bind("Rope, sturdy")
            .bind(10_i32)
            .execute(&pool)
            .await
            .expect("seed Rope");

        // Seed 1 001 more rows — bringing the total to 1 003 (one more than
        // MAX_LIST_ROWS = 1 000) — so the cap test proves the ceiling is
        // enforced, not just that the table happens to be smaller.
        for i in 1..=1001_i32 {
            sqlx::query("INSERT INTO widget (name, qty) VALUES (?, ?)")
                .bind(format!("item-{i}"))
                .bind(i)
                .execute(&pool)
                .await
                .expect("seed widget");
        }

        app.into_router()
    })
    .await
}

// ─── helpers ─────────────────────────────────────────────────────────────────

async fn csv_get(uri: &str) -> (StatusCode, String) {
    let router = boot().await.clone();
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, body)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_exports_csv() {
    let (status, body) = csv_get("/api/widget/?format=csv").await;

    assert_eq!(status, StatusCode::OK);

    // content-type and disposition are checked by driving the router directly
    // so we re-run a raw request here for header assertions.
    let router = boot().await.clone();
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/widget/?format=csv")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let disp = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    assert!(
        ctype.contains("text/csv"),
        "CSV content-type, got {ctype:?}"
    );
    assert!(
        disp.contains("widget.csv"),
        "filename in disposition: {disp:?}"
    );

    let mut lines = body.lines();
    assert_eq!(
        lines.next(),
        Some("id,name,qty"),
        "header row follows model field order"
    );
    // Both hand-seeded named rows are present if they fall within the cap.
    assert!(body.contains("Anvil"), "Anvil row present: {body}");
    assert!(
        body.contains("\"Rope, sturdy\""),
        "a comma-containing value is quoted: {body}"
    );
}

/// gaps2 #72 — `?format=csv` with no `page` param must be capped at
/// `MAX_LIST_ROWS` (1 000). The table has 1 003 rows; the response must
/// contain exactly 1 000 data rows (the header row is row 0).
#[tokio::test]
async fn csv_export_capped_at_max_list_rows() {
    let (status, body) = csv_get("/api/widget/?format=csv").await;
    assert_eq!(status, StatusCode::OK, "response must be 200: {body}");

    // Count data rows: total lines minus the header.
    let total_lines = body.lines().count();
    let data_rows = total_lines.saturating_sub(1); // subtract header

    assert_eq!(
        data_rows, 1000,
        "CSV export must be capped at MAX_LIST_ROWS (1 000); \
         got {data_rows} data rows in {total_lines} total lines"
    );
}
