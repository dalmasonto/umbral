//! Permission-gated `.action_by(...)` coverage (gaps4 #80). Own binary,
//! same reason `actions_gated.rs` is split from `actions.rs`: the
//! resource's permission is plugin-wide.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{IsAuthenticated, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Community {
    id: i64,
    #[umbral(unique)]
    slug: String,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("action_by_gated.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let resource = ResourceConfig::new("community")
            .permission(IsAuthenticated)
            .action_by("slug", Method::GET, |_ctx| async move {
                Ok(json!({ "should": "not reach" }))
            });
        let rest = RestPlugin::default().resource(resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Community>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query("INSERT INTO community (id, slug, name) VALUES (1, 'acme-corp', 'Acme Corp')")
            .execute(&pool)
            .await
            .expect("seed community");

        app.into_router()
    })
    .await
}

async fn run(router: axum::Router, method: Method, uri: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(req).await.expect("oneshot").status()
}

/// An anonymous request to a `.action_by`-mounted route on a resource
/// gated by `IsAuthenticated` returns 401 — the SAME `Permission::check`
/// gate a plain `.action()` gets, proven here for the by-field form: the
/// row exists (seeded above) so a bug that skipped the gate would return
/// 200, not 401.
#[tokio::test]
async fn anonymous_by_field_lookup_is_rejected_when_gated() {
    let router = boot().await.clone();
    let status = run(router, Method::GET, "/api/community/acme-corp/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The PK-fallback path is gated identically — a permission denial can't
/// be bypassed by sending the PK instead of the slug.
#[tokio::test]
async fn anonymous_pk_fallback_is_also_rejected_when_gated() {
    let router = boot().await.clone();
    let status = run(router, Method::GET, "/api/community/1/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
