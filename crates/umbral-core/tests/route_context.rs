//! Task 6 — `App::builder().route_context(resolver)` must establish the
//! request-scoped `RouteContext` so that the downstream HANDLER (and every
//! `.await` inside it, including ORM calls) runs inside
//! `route_context::scope(ctx, ...)`.
//!
//! The critical correctness property proved here: a handler reads the tenant
//! the resolver set from the `X-Tenant` header via the ambient
//! `umbral::db::route_context()` accessor — NOT from anything the test threads
//! through manually. If the task-local didn't survive into the handler future,
//! the handler would see the default (no tenant) and the assertion would fail.

use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral::db::{RouteContext, TenantKey};
use umbral::routes::Routes;

/// The handler reads the AMBIENT routing context (set by the resolver layer,
/// not by anything passed in) and echoes the tenant string in the body.
async fn echo_tenant() -> Response {
    let ctx = umbral::db::route_context();
    let body = match ctx.tenant() {
        Some(t) => t.as_str().to_string(),
        None => "none".to_string(),
    };
    (StatusCode::OK, body).into_response()
}

async fn build() -> axum::Router {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .route_context(|req: &Request| {
            match req.headers().get("x-tenant").and_then(|v| v.to_str().ok()) {
                Some(t) => RouteContext::new().with_tenant(TenantKey::new(t)),
                None => RouteContext::new(),
            }
        })
        .routes(Routes::new().get("/whoami", echo_tenant))
        .build()
        .expect("App::build")
        .into_router()
}

async fn body_for(router: &axum::Router, header: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().uri("/whoami");
    if let Some(h) = header {
        builder = builder.header("x-tenant", h);
    }
    let resp = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn handler_sees_tenant_set_by_resolver() {
    let router = build().await;

    // With an X-Tenant header, the resolver builds a tenant context and the
    // handler — deep in the stack — reads it back ambiently.
    let (status, body) = body_for(&router, Some("acme")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "acme",
        "handler must observe the tenant the resolver set"
    );

    // With NO header, the resolver yields a default context: the handler sees
    // no tenant (no silent inheritance from the previous request).
    let (status, body) = body_for(&router, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "none",
        "handler must see the default context when no tenant is resolved"
    );
}
