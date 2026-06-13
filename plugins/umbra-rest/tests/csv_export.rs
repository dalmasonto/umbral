//! `?format=csv` list export (feature #61). A `GET /api/<table>/?format=csv`
//! downloads the full filtered set as CSV.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbra_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Widget {
    id: i64,
    name: String,
    qty: i32,
}

async fn boot() -> axum::Router {
    let settings = umbra::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("csv.sqlite");
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

    let app = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Widget>()
        .plugin(RestPlugin::default())
        .build()
        .expect("App::build");

    let pool = umbra::db::pool();
    sqlx::query("CREATE TABLE widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, qty INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .expect("create widget");
    for (name, qty) in [("Anvil", 3), ("Rope, sturdy", 10)] {
        sqlx::query("INSERT INTO widget (name, qty) VALUES (?, ?)")
            .bind(name)
            .bind(qty)
            .execute(&pool)
            .await
            .expect("seed widget");
    }

    app.into_router()
}

#[tokio::test]
async fn list_exports_csv() {
    let router = boot().await;

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/widget/?format=csv")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.contains("text/csv"),
        "CSV content-type, got {ctype:?}"
    );
    let disp = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        disp.contains("widget.csv"),
        "filename in disposition: {disp:?}"
    );

    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    let mut lines = body.lines();
    assert_eq!(
        lines.next(),
        Some("id,name,qty"),
        "header row follows model field order"
    );
    // Both rows present; the csv writer quotes the value with a comma.
    assert!(body.contains("Anvil"), "first row: {body}");
    assert!(
        body.contains("\"Rope, sturdy\""),
        "a comma-containing value is quoted: {body}"
    );
}
