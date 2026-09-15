//! End-to-end proof that `strict_object_scope` turns the REST
//! `security.object_scope` warning into a boot-blocking error (IDOR design
//! spec, gaps5 #101 / tf#322).
//!
//! A single real `App::build()` — the OnceLocks it writes (settings / db /
//! backend) mean one build per test binary, so this file holds exactly one.
//! With `strict_object_scope = true` and a write-enabled, unscoped resource,
//! `build()` must fail with `BuildError::SystemCheckFailed` carrying the
//! `security.object_scope` finding at `Severity::Error`.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral::BuildError;
use umbral::check::Severity;
use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Widget {
    id: i64,
    name: String,
}

#[tokio::test]
async fn strict_mode_blocks_boot_on_unscoped_write_resource() {
    let mut settings = umbral::Settings::from_env().expect("figment defaults");
    settings.strict_object_scope = true;

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("rest_strict_object_scope.sqlite");
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

    let result = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Widget>()
        // A write-enabled resource with NO object scope and NO ack marker —
        // exactly the IDOR surface the check flags.
        .plugin(RestPlugin::default().resource(ResourceConfig::new("widget")))
        .build();

    match result {
        Err(BuildError::SystemCheckFailed { findings }) => {
            let hit = findings
                .iter()
                .find(|f| f.check_id == "security.object_scope");
            let hit = hit.expect(
                "build must fail with a security.object_scope finding under strict_object_scope",
            );
            assert_eq!(
                hit.severity,
                Severity::Error,
                "under strict_object_scope the finding is an Error"
            );
            assert!(
                hit.message.contains("widget"),
                "the finding must name the offending table; got {:?}",
                hit.message
            );
        }
        Err(other) => panic!("expected SystemCheckFailed, got a different BuildError: {other:?}"),
        Ok(_) => panic!("strict_object_scope must BLOCK boot on an unscoped write resource"),
    }
}
