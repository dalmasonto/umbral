//! Wave 2 — the `field.storage_backend` boot check FAILS when a model
//! declares an `ImageField` / `FileField` but no plugin provides a
//! Storage backend.
//!
//! Own binary: one `App::build` per process (settings OnceLock).

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use serde::{Deserialize, Serialize};
use umbra::orm::ImageField;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "no_storage_doc")]
pub struct Doc {
    pub id: i64,
    pub cover: ImageField,
}

#[tokio::test]
async fn build_fails_when_image_field_has_no_storage_backend() {
    let settings = umbra::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("no_storage.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let result = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Doc>()
        .build();

    let err = result
        .err()
        .expect("build must fail: ImageField with no Storage backend");

    let findings = match err {
        umbra::BuildError::SystemCheckFailed { findings } => findings,
        other => panic!("expected SystemCheckFailed, got {other:?}"),
    };

    let hit = findings
        .iter()
        .find(|f| f.check_id == "field.storage_backend");
    let hit = hit.expect("a field.storage_backend finding should be present");
    assert_eq!(hit.severity, umbra_core::check::Severity::Error);
    // The message names the model + field and the fix.
    assert!(
        hit.message.contains("cover") && hit.message.contains("no Storage backend"),
        "message should name the field and the problem; got: {}",
        hit.message
    );
    assert!(
        hit.hint
            .as_deref()
            .unwrap_or_default()
            .contains("StoragePlugin"),
        "hint should mention the fix (StoragePlugin / set_storage)"
    );
}
