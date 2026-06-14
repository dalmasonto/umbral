//! The pass side of the `field.choices_default` system check (gaps2
//! #32): a choices field whose `#[umbra(default = "...")]` IS one of the
//! stored DB literals must build cleanly — the check fires only on a
//! non-member default.
//!
//! Separate binary from `system_check_choices_default.rs` because a
//! single test binary can only let one `App::builder().build()` pass
//! phase 3 (it publishes the process-wide registry / settings / db
//! OnceLocks).
//!
//! See `crates/umbra-core/src/check.rs::field_choices_default`.

use std::collections::HashMap;

use umbra::{App, Environment, Settings};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    umbra::orm::Choices,
    serde::Serialize,
    serde::Deserialize,
)]
#[choices(rename_all = "lowercase")]
pub enum Mood {
    #[default]
    Happy,
    Sad,
    Neutral,
}

/// The correct shape: the default is the stored DB literal (`sad`), a
/// real member of the choices set. The boot check must stay silent.
#[derive(
    Debug, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "scd_good_default")]
pub struct GoodDefault {
    pub id: i64,
    pub body: String,
    #[umbra(choices, default = "sad")]
    pub mood: Mood,
}

fn make_settings() -> Settings {
    Settings {
        database_url: "sqlite::memory:".to_string(),
        databases: HashMap::new(),
        secret_key: "real-secret-not-the-default".to_string(),
        environment: Environment::Dev,
        allowed_hosts: vec!["localhost".to_string(), "127.0.0.1".to_string()],
        log_level: "info".to_string(),
        db_max_connections: 10,
        db_acquire_timeout_secs: 30,
        bind_addr: "127.0.0.1:8000".to_string(),
        time_zone: None,
        static_url: "/static/".to_string(),
        static_root: "staticfiles/".to_string(),
        extra: HashMap::new(),
    }
}

/// A choices default that's a real member of the choices set must not
/// trip `field.choices_default`; `App::build()` succeeds.
#[tokio::test]
async fn valid_choices_default_builds_cleanly() {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = App::builder()
        .settings(make_settings())
        .database("default", pool)
        .model::<GoodDefault>()
        .build();

    assert!(
        result.is_ok(),
        "a choices field defaulting to a real member should build cleanly; got {:?}",
        result.err(),
    );
}
