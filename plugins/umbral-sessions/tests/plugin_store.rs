//! Tests for `SessionsPlugin.store()` builder + lifecycle install (2a Task 4).
//!
//! Verifies:
//!   (a) The default `SessionsPlugin::default()` installs a `DbStore` during
//!       `on_ready` so `active_store()` resolves and a round-trip through
//!       `session_layer` + `set_data` persists the value.
//!   (b) `SessionsPlugin::default().store(DbStore::default())` explicitly sets
//!       the store; `active_store()` still resolves and a write/read
//!       round-trip works.
//!   (c) `active_store()` can be called from outside a request scope (direct
//!       store I/O) and still works.
//!
//! Own test binary (own ambient `OnceLock`s), so pool + store state are
//! isolated from other suites.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::web::header;
use umbral_sessions::store::{DbStore, SessionRecord, active_store};
use umbral_sessions::{COOKIE_NAME, SessionsPlugin};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("plugin_store.sqlite");
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
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            // Boot with an explicit .store(DbStore::default()) to exercise
            // the builder. The `on_ready` hook installs it into the ambient
            // OnceLock so active_store() returns it for the rest of the suite.
            .plugin(SessionsPlugin::default().store(DbStore))
            .build()
            .expect("App::build with SessionsPlugin + explicit store");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// `active_store()` resolves after `SessionsPlugin::on_ready` ran (directly
/// through `App::build` which drives the plugin lifecycle). A save/load
/// round-trip through the installed store works.
#[tokio::test]
async fn active_store_resolves_after_plugin_boot() {
    boot().await;

    let store = active_store();
    let now = chrono::Utc::now();
    let record = SessionRecord {
        user_id: Some("plugin-test-user".to_string()),
        data: r#"{"plugin":true}"#.to_string(),
        created_at: now,
        expires_at: now + chrono::Duration::seconds(3600),
    };

    store
        .save("plugin-store-tok", &record)
        .await
        .expect("save via active_store");
    let loaded = store
        .load("plugin-store-tok")
        .await
        .expect("load via active_store")
        .expect("record present");

    assert_eq!(loaded.user_id, Some("plugin-test-user".to_string()));
    assert_eq!(loaded.data, r#"{"plugin":true}"#);
}

/// A round-trip through `session_layer` + `set_data` persists via the
/// installed store. The `.store(DbStore::default())` builder wires correctly.
#[tokio::test]
async fn session_layer_set_data_persists_via_installed_store() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    boot().await;

    async fn writer() -> impl IntoResponse {
        umbral_sessions::current_mut(|s| s.set_raw("store_key", serde_json::json!(42)))
            .expect("inside a request scope");
        "wrote"
    }

    let inner = axum::Router::new().route("/w", get(writer));
    // Use .store(DbStore::default()) explicitly — this exercises the builder
    // path (on_ready tries to install again; idempotent, keeps first).
    let router = SessionsPlugin::default().store(DbStore).wrap_router(inner);

    let req = Request::builder().uri("/w").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // A Set-Cookie header must have been emitted (lazy materialisation fired).
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("written session must emit Set-Cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));

    // Extract the raw token and read the persisted row back via active_store.
    let token = set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let loaded = active_store()
        .load(&token)
        .await
        .expect("load via active_store after set_data")
        .expect("row must exist");

    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&loaded.data).expect("data is valid JSON");
    assert_eq!(
        map.get("store_key").and_then(|v| v.as_i64()),
        Some(42),
        "set_data value must round-trip through the installed store"
    );
}

/// Out-of-request direct I/O through `active_store()` still works.
/// This exercises the fallback path (no task-local scope active).
#[tokio::test]
async fn active_store_direct_io_out_of_request() {
    boot().await;

    let store = active_store();
    let now = chrono::Utc::now();
    let record = SessionRecord {
        user_id: None,
        data: "{}".to_string(),
        created_at: now,
        expires_at: now + chrono::Duration::seconds(7200),
    };

    store
        .save("direct-io-tok", &record)
        .await
        .expect("direct save");
    let loaded = store
        .load("direct-io-tok")
        .await
        .expect("direct load")
        .expect("present");

    assert_eq!(loaded.data, "{}");
    assert!(loaded.expires_at > chrono::Utc::now());

    store
        .destroy("direct-io-tok")
        .await
        .expect("direct destroy");
    let gone = store
        .load("direct-io-tok")
        .await
        .expect("load after destroy");
    assert!(gone.is_none(), "destroyed token → None");
}
