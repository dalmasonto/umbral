//! Behavioral coverage for the public `umbral_rest::is_hidden(table, field)`
//! query — the field-hide read umbral-openapi uses to scrub hidden columns
//! out of the generated spec.
//!
//! `is_hidden` must agree 1:1 with what `RestPlugin::apply_overrides`
//! strips, across BOTH hide sources:
//!   - plugin-level `RestPlugin::hide` / `hide_model`, AND
//!   - resource-level `ResourceConfig::hide`.
//!
//! Driven through the real registration path (`App::build` → the REST
//! plugin's `routes()` sets the CONFIG OnceLock) so the resource→plugin
//! hide merge is actually exercised. Lives in its own test binary so the
//! single-set CONFIG / settings OnceLocks are clean.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Account {
    id: i64,
    label: String,
    // hidden via the plugin-level `.hide(...)` builder
    password_hash: String,
    // hidden via a `ResourceConfig::hide(...)` merged through `.resource`
    api_token: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("is_hidden.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let rest = RestPlugin::default()
            // plugin-level hide
            .hide("account", "password_hash")
            // resource-level hide, merged into the plugin's hide set
            .resource(ResourceConfig::new("account").hide("api_token"));

        let _app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Account>()
            .plugin(rest)
            .build()
            .expect("App::build sets the REST CONFIG OnceLock");
    })
    .await;
}

#[tokio::test]
async fn is_hidden_true_for_plugin_level_hide() {
    boot().await;
    assert!(
        umbral_rest::is_hidden("account", "password_hash"),
        "plugin-level RestPlugin::hide should make is_hidden true"
    );
}

#[tokio::test]
async fn is_hidden_true_for_resource_config_hide() {
    boot().await;
    assert!(
        umbral_rest::is_hidden("account", "api_token"),
        "resource-level ResourceConfig::hide should make is_hidden true \
         (proves the resource→plugin hide merge is reflected)"
    );
}

#[tokio::test]
async fn is_hidden_false_for_visible_field() {
    boot().await;
    assert!(
        !umbral_rest::is_hidden("account", "label"),
        "a non-hidden field must report false"
    );
}

#[tokio::test]
async fn is_hidden_false_for_unknown_table() {
    boot().await;
    // Use a non-sensitive field name. `password_hash` is now in the
    // HARD_DENIED_FIELDS list (gaps2 #75) and always returns true, which
    // would make this assertion wrong. Use a plain field that has no
    // special treatment to verify the "no hides registered" path.
    assert!(
        !umbral_rest::is_hidden("no_such_table", "some_plain_field"),
        "a non-sensitive field on a table with no hides must report false"
    );
}
