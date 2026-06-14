//! Coverage for the `field.choices_default` system check (gaps2 #32):
//! a `choices` field whose `#[umbra(default = "...")]` isn't one of the
//! enum's stored DB values ships broken DDL. The check walks the model
//! registry at boot and turns that into a `BuildError::SystemCheckFailed`
//! with a did-you-mean when the bad default looks like a Rust enum path.
//!
//! Each `tests/*.rs` file is its own binary, so this file owns its own
//! copy of the process-wide registry / settings / db OnceLocks. Within a
//! single binary only one `App::builder().build()` can pass phase 3 (it
//! publishes those OnceLocks), so the bad-default case (which fails at
//! phase 4 *after* phase 3 already published the registry) is the lone
//! `build()` here. The valid-default case lives in its own binary,
//! `system_check_choices_default_valid.rs`.
//!
//! See `crates/umbra-core/src/check.rs::field_choices_default`.

use std::collections::HashMap;

use umbra::check::{CheckLocation, Severity};
use umbra::{App, BuildError, Environment, Settings};

/// A choices enum with `rename_all = "lowercase"`, so the stored DB
/// values are `["happy", "sad", "neutral"]` — and the Rust path
/// `Mood::Sad` lowers its tail to the valid literal `"sad"`.
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

/// The bug shape from gaps2 #32: the default is the Rust enum *path*
/// (`Mood::Sad`) instead of the stored DB literal (`sad`). That value
/// lands verbatim in DDL and produces an unusable schema, so the boot
/// check must reject it.
#[derive(
    Debug, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "scd_bad_default")]
pub struct BadDefault {
    pub id: i64,
    pub body: String,
    #[umbra(choices, default = "Mood::Sad")]
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

/// A choices field with a path-shaped default that isn't a member of the
/// choices must fail `App::build()` at phase 4 with a
/// `field.choices_default` Error finding that names the model + field,
/// and whose hint carries the did-you-mean for the stored DB literal.
#[tokio::test]
async fn choices_path_default_fails_build_with_did_you_mean() {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = App::builder()
        .settings(make_settings())
        .database("default", pool)
        .model::<BadDefault>()
        .build();

    let err = result
        .err()
        .expect("a choices field with a non-member default should fail the build");

    let findings = match err {
        BuildError::SystemCheckFailed { findings } => findings,
        other => panic!("expected BuildError::SystemCheckFailed, got {other:?}"),
    };

    let hit = findings
        .iter()
        .find(|f| f.check_id == "field.choices_default" && f.severity == Severity::Error)
        .unwrap_or_else(|| {
            panic!("expected a field.choices_default Error finding; got {findings:#?}")
        });

    // The finding must point at the offending model + field.
    match &hit.location {
        CheckLocation::Field {
            plugin,
            model,
            field,
        } => {
            assert_eq!(
                *plugin, "app",
                "model registered via .model::<T>() is the `app` plugin"
            );
            assert_eq!(*model, "BadDefault", "finding should name the model");
            assert_eq!(*field, "mood", "finding should name the field");
        }
        other => panic!("expected a Field location, got {other:?}"),
    }

    // The message should name the bad default and list the valid choices.
    assert!(
        hit.message.contains("Mood::Sad"),
        "message should quote the bad default; got {:?}",
        hit.message,
    );

    // `Mood::Sad` lowers its tail to `sad`, which IS a valid choice, so
    // the hint should suggest the stored DB literal.
    let hint = hit
        .hint
        .as_deref()
        .expect("a path-shaped default should carry a did-you-mean hint");
    assert!(
        hint.contains("Did you mean the DB literal `sad`"),
        "hint should suggest the lowercased stored literal; got {hint:?}",
    );
}
