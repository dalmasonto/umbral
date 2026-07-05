//! Wave 2 — the same model that fails `file_field_check_missing_storage`
//! builds cleanly once a storage-providing plugin is registered. Proves
//! the `field.storage_backend` check reads the `provides_storage()`
//! capability flag, not the ambient (which is still unpublished at check
//! time because backends register in `on_ready`).
//!
//! Own binary: one `App::build` per process.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use serde::{Deserialize, Serialize};
use umbral::orm::ImageField;
use umbral::plugin::Plugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "with_storage_doc")]
pub struct Doc {
    pub id: i64,
    pub cover: ImageField,
}

/// Reports `provides_storage() == true`; does NOT register an ambient
/// backend, which is the point — the check must pass on the capability
/// flag alone, since `on_ready` (where a real backend registers) runs
/// after the check.
struct FakeStoragePlugin;

impl Plugin for FakeStoragePlugin {
    fn name(&self) -> &'static str {
        "fake_storage"
    }
    fn provides_storage(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn build_succeeds_with_storage_providing_plugin() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("with_storage.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let result = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Doc>()
        .plugin(FakeStoragePlugin)
        .build();

    assert!(
        result.is_ok(),
        "build should succeed when a plugin reports provides_storage(); got {:?}",
        result.err()
    );
}
