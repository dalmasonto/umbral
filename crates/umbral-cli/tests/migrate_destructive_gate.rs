//! audit_2 core-migrate #6 — `migrate` must REFUSE to apply a pending migration
//! that drops a table/column (destroys rows) unless the operator passes
//! `--allow-destructive`. Guards against one missing `.model::<T>()` silently
//! dropping a production table via the routine `makemigrations && migrate` loop.
//!
//! Sole test in its binary: it sets the process cwd (so the default `migrations`
//! dir resolves to a fixture tree) and builds one ambient App.

use umbral::migrate::{APP_PLUGIN_NAME, MigrationFile, Operation, Snapshot};
use umbral::{App, Settings};

#[tokio::test]
async fn migrate_refuses_a_pending_drop_table_without_allow_destructive() {
    // A fixture migrations tree with ONE pending migration that drops a table.
    let tmp = tempfile::tempdir().expect("tempdir");
    let app_mig = tmp.path().join("migrations").join(APP_PLUGIN_NAME);
    std::fs::create_dir_all(&app_mig).expect("mkdir migrations/app");
    let file = MigrationFile {
        id: "0001_drop_legacy".to_string(),
        plugin: APP_PLUGIN_NAME.to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::DropTable {
            table: "legacy".to_string(),
        }],
        snapshot_after: Snapshot::default(),
        replaces: Vec::new(),
    };
    std::fs::write(
        app_mig.join("0001_drop_legacy.json"),
        umbral::_serde_json::to_string_pretty(&file).expect("serialize"),
    )
    .expect("write fixture");

    // `migrate` reads the default `MIGRATIONS_DIR` ("migrations") relative to the
    // cwd — point it at the fixture. (Only test in this binary, so no races.)
    let original_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(tmp.path()).expect("chdir into fixture");

    let settings = Settings::from_env().expect("figment defaults");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .build()
        .expect("App::build");

    // `umbral migrate` — no --allow-destructive. The gate must refuse BEFORE any
    // SQL runs, so nothing is dropped and the command errors.
    let argv: Vec<std::ffi::OsString> = vec!["umbral".into(), "migrate".into()];
    let result = umbral_cli::dispatch_with_argv(app, argv).await;

    // Restore cwd before asserting so a panic can't leave the process elsewhere.
    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    let err = result.expect_err("migrate must REFUSE the destructive DropTable without the flag");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("destructive"),
        "the error must name the destructive gate; got: {msg}"
    );

    // The migration was NOT recorded as applied (nothing ran).
    let applied: i64 = umbral::_sqlx::query_scalar(
        "SELECT COUNT(*) FROM umbral_migrations WHERE name = '0001_drop_legacy'",
    )
    .fetch_one(&umbral::db::pool())
    .await
    .unwrap_or(0);
    assert_eq!(
        applied, 0,
        "the destructive migration must NOT have been applied"
    );
}
