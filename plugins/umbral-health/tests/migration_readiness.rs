//! Kikosi #5 / gaps3 #38 — readiness gates on migrations, and the `/readyz`
//! alias.
//!
//! `HealthPlugin::require_migrations()` makes `/ready` (and `/readyz`) report a
//! `"migrations"` check and return 503 while the database is behind the
//! migrations this binary carries. This binary covers the wiring at the router
//! layer; the pure gate (`Pending` blocks, a DB-ahead rollback does not) is unit-
//! tested in `src/lib.rs`, and the underlying `drift_report` primitive in
//! `umbral-core`'s `tests/drift_report_probe.rs`.
//!
//! The health crate has no `migrations/` dir, so `drift_report()` sees zero
//! pending here — the deterministic "clean tree ⇒ ready" case.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral_health::HealthPlugin;

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults always load");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(HealthPlugin::default())
            .build()
            .expect("App::build should succeed");
    })
    .await;
}

async fn ready_json(plugin: HealthPlugin, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = plugin
        .routes()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

/// The k8s-convention `/readyz` alias resolves to the same readiness handler as
/// `/ready`.
#[tokio::test]
async fn readyz_is_an_alias_for_ready() {
    boot().await;
    let (status, json) = ready_json(HealthPlugin::default(), "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["checks"]["database"]["status"], "ok");
}

/// With the gate on and a clean schema (nothing pending), readiness includes a
/// passing `migrations` check and stays 200.
#[tokio::test]
async fn require_migrations_reports_ok_on_a_migrated_schema() {
    boot().await;
    let (status, json) = ready_json(HealthPlugin::default().require_migrations(), "/readyz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "clean schema is ready; body: {json}"
    );
    assert_eq!(
        json["checks"]["migrations"]["status"], "ok",
        "the migrations check must be present and passing; body: {json}",
    );
}

/// Without the opt-in, readiness is unchanged: no `migrations` check appears, so
/// existing `HealthPlugin` users see exactly the DB-only behaviour they had.
#[tokio::test]
async fn default_readiness_has_no_migrations_check() {
    boot().await;
    let (status, json) = ready_json(HealthPlugin::default(), "/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["checks"].get("migrations").is_none(),
        "migrations must not be probed unless require_migrations() opts in; body: {json}",
    );
}
