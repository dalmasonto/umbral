//! gaps3 #35 — `#[umbral(case_insensitive)]` makes a column case-insensitive at
//! the DATABASE level while preserving the original casing in storage. On SQLite
//! that's `COLLATE NOCASE`. Behavioural end-to-end: the migration engine emits
//! the DDL, then real rows prove the three properties developers expect —
//! case-insensitive UNIQUE, case-insensitive lookup, and case-preserving storage.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ci_handle")]
pub struct Handle {
    pub id: i64,
    /// Case-insensitive unique: `Dalmas` and `dalmas` are the same handle, but
    /// the stored value keeps whatever case was first written.
    #[umbral(case_insensitive, unique)]
    pub name: String,
}

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let dir = std::env::temp_dir();
    let path = dir.join(format!("umbral_ci_field_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Handle>()
        .build()
        .expect("App::build");

    // Create the table THROUGH the migration engine so the COLLATE NOCASE the
    // engine renders is actually what the DB gets (a raw CREATE TABLE would
    // bypass the very code under test).
    let mig = tempfile::tempdir().expect("migration dir");
    let mig_path = mig.path().to_path_buf();
    std::mem::forget(mig);
    umbral::migrate::make_in(&mig_path).await.expect("make");
    umbral::migrate::run_in(&mig_path).await.expect("run");
}

#[tokio::test]
async fn case_insensitive_unique_lookup_and_preserved_casing() {
    boot().await;

    // Store with mixed case.
    Handle::objects()
        .create(Handle {
            id: 0,
            name: "Dalmas".to_string(),
        })
        .await
        .expect("first insert");

    // 1. Case-insensitive UNIQUE: a differently-cased duplicate collides.
    let dup = Handle::objects()
        .create(Handle {
            id: 0,
            name: "dalmas".to_string(),
        })
        .await;
    assert!(
        matches!(
            dup,
            Err(umbral::orm::write::WriteError::UniqueViolation { .. })
        ),
        "`dalmas` must collide with the stored `Dalmas` on a case-insensitive UNIQUE; got {dup:?}"
    );

    // 2. Case-insensitive lookup: querying with any casing finds the row.
    for probe in ["DALMAS", "dalmas", "DaLmAs"] {
        let found = Handle::objects()
            .filter(handle::NAME.eq(probe))
            .first()
            .await
            .expect("query")
            .unwrap_or_else(|| panic!("case-insensitive lookup for `{probe}` must find the row"));
        // 3. Case is PRESERVED — the stored value is the original `Dalmas`.
        assert_eq!(
            found.name, "Dalmas",
            "the stored casing must be preserved, not folded"
        );
    }
}
