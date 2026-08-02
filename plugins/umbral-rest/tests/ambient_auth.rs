//! gaps4 #42: `AppBuilder::authentication(...)` — the app-wide default
//! authentication backend that REST (and GraphQL / realtime) inherit when
//! no per-plugin `.authenticate(...)` was configured.
//!
//! Before this, the same `ChainAuthentication` block had to be pasted into
//! each plugin, and forgetting one copy silently made that surface
//! anonymous — every request denied by gates that could never open.
//!
//! This boots a REST plugin with NO `.authenticate(...)` of its own and
//! drives an `IsAuthenticated`-gated resource: the app-wide backend must
//! be the one identifying the caller. Own test binary (one `App::build`
//! per process).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use serde::{Deserialize, Serialize};
use umbral_rest::{FnAuthentication, Identity, IsAuthenticated, ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "amb_note")]
pub struct Note {
    pub id: i64,
    pub title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ambient_auth.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            // ONE app-wide line — note the REST plugin below has NO
            // .authenticate(...) of its own.
            .authentication(FnAuthentication::new(
                |headers: umbral::web::HeaderMap| async move {
                    let user_id: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
                    Some(Identity::user(user_id))
                },
            ))
            .model::<Note>()
            .plugin(
                RestPlugin::default()
                    .resource(ResourceConfig::new("amb_note").permission(IsAuthenticated)),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        sqlx::query("INSERT INTO amb_note (title) VALUES ('hello')")
            .execute(&umbral::db::pool())
            .await
            .expect("seed");

        app.into_router()
    })
    .await
}

async fn get(path: &str, user: Option<i64>) -> StatusCode {
    let app = boot().await.clone();
    let mut req = Request::builder().uri(path).method("GET");
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    app.oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("request")
        .status()
}

/// The headline: the REST plugin inherits the app-wide backend, so the
/// gated resource opens for an identified caller and stays shut otherwise.
#[tokio::test]
async fn rest_inherits_the_app_wide_authentication() {
    assert_eq!(
        get("/api/amb_note/", Some(7)).await,
        StatusCode::OK,
        "the app-wide backend identifies the caller — no per-plugin .authenticate needed"
    );
    let denied = get("/api/amb_note/", None).await;
    assert!(
        denied == StatusCode::UNAUTHORIZED || denied == StatusCode::FORBIDDEN,
        "anonymous stays denied by IsAuthenticated; got {denied}"
    );
}

/// The ambient accessor is populated for OTHER plugins (GraphQL, realtime)
/// to inherit through the same seam.
#[tokio::test]
async fn the_default_backend_is_published_ambiently() {
    boot().await;
    assert!(
        umbral::auth::default_authentication().is_some(),
        "AppBuilder::authentication publishes the ambient default"
    );
}
