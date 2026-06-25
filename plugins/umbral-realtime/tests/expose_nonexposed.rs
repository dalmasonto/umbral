//! Security proof (test 1): a model that was NEVER `expose`d (nor `on_model`'d)
//! is never broadcast. Default-deny: a subscriber on any group receives nothing
//! when an un-exposed model's `post_save` fires.

#![allow(dead_code)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use umbral_realtime::{DEFAULT_BUFFER, Expose, Realtime, RealtimePlugin};

// Exposed — present only so the plugin has SOME exposure wired; the test fires
// a DIFFERENT, un-exposed table.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nonexp_exposed")]
pub struct Exposed {
    pub id: i64,
    pub title: String,
}

// Never passed to `expose` / `on_model`.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nonexp_secret")]
pub struct SecretModel {
    pub id: i64,
    pub secret: String,
}

#[tokio::test]
async fn a_model_that_was_not_exposed_never_broadcasts() {
    umbral::signals::clear_for_tests();
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // Only `Exposed` is wired. `SecretModel` is deliberately absent.
        .plugin(RealtimePlugin::new().expose::<Exposed>(Expose::to_group("public:exposed")))
        .build()
        .expect("App::build");

    // A subscriber sits on a group; nothing routes the un-exposed model here.
    let mut groups = HashSet::new();
    groups.insert("public:exposed".to_string());
    groups.insert("public:secret".to_string());
    let (_id, mut rx) = Realtime::registry()
        .register(None, groups, DEFAULT_BUFFER)
        .await
        .expect("registration admitted");

    // Fire the un-exposed model's save exactly as the ORM write path would.
    umbral::signals::emit(
        "post_save:nonexp_secret",
        serde_json::json!({ "instance": { "id": 1, "secret": "leak" }, "created": true }),
    )
    .await;

    assert!(
        rx.try_recv().is_err(),
        "a non-exposed model must broadcast NOTHING"
    );

    // Sanity: the exposed model DOES broadcast — proving the silence above is
    // the default-deny, not a dead bridge.
    umbral::signals::emit(
        "post_save:nonexp_exposed",
        serde_json::json!({ "instance": { "id": 2, "title": "ok" }, "created": true }),
    )
    .await;
    assert!(
        rx.try_recv().is_ok(),
        "the exposed model still broadcasts (bridge is live)"
    );
}
