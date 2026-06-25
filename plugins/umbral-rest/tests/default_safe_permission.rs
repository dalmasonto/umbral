//! WEB-1 regression: `RestPlugin`'s default permission is `ReadOnly`,
//! not `AllowAny`. A resource mounted with no explicit `.permission(...)`
//! and no `.default_permission(...)` must serve anonymous reads but
//! reject anonymous writes (POST/PUT/PATCH/DELETE) with 403 — so adding
//! `RestPlugin::default()` to get a read API no longer silently exposes
//! anonymous full CRUD on every model.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::RestPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Widget {
    id: i64,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

/// Boots with a plain `RestPlugin::default()` — the whole point is to
/// exercise the *default* permission, so we deliberately do NOT call
/// `.default_permission(...)` or `.permission(...)`.
async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_default_safe.sqlite");
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
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build with RestPlugin");

        let pool = umbral::db::pool();
        let meta = umbral::migrate::ModelMeta::for_::<Widget>();
        let op = umbral::migrate::Operation::CreateTable {
            table: "widget".to_string(),
            columns: meta.fields.clone(),
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };
        for stmt in umbral::migrate::render_operation_for(&op, "sqlite") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .expect("create widget");
        }
        sqlx::query("INSERT INTO widget (name) VALUES ('seed')")
            .execute(&pool)
            .await
            .expect("seed widget");

        app.into_router()
    })
    .await
}

async fn status_of(req: Request<Body>) -> StatusCode {
    let resp = boot().await.clone().oneshot(req).await.expect("oneshot");
    resp.status()
}

#[tokio::test]
async fn default_permission_allows_reads() {
    let list = Request::builder()
        .method("GET")
        .uri("/api/widget/")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        status_of(list).await,
        StatusCode::OK,
        "anonymous list must be allowed under the ReadOnly default"
    );

    let detail = Request::builder()
        .method("GET")
        .uri("/api/widget/1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        status_of(detail).await,
        StatusCode::OK,
        "anonymous retrieve must be allowed under the ReadOnly default"
    );
}

#[tokio::test]
async fn default_permission_blocks_writes() {
    let create = Request::builder()
        .method("POST")
        .uri("/api/widget/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"injected"}"#))
        .unwrap();
    assert_eq!(
        status_of(create).await,
        StatusCode::FORBIDDEN,
        "anonymous create must be 403 under the ReadOnly default (WEB-1)"
    );

    let update = Request::builder()
        .method("PATCH")
        .uri("/api/widget/1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"tampered"}"#))
        .unwrap();
    assert_eq!(
        status_of(update).await,
        StatusCode::FORBIDDEN,
        "anonymous update must be 403 under the ReadOnly default (WEB-1)"
    );

    let delete = Request::builder()
        .method("DELETE")
        .uri("/api/widget/1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        status_of(delete).await,
        StatusCode::FORBIDDEN,
        "anonymous delete must be 403 under the ReadOnly default (WEB-1)"
    );
}
