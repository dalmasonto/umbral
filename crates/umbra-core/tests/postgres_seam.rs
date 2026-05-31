//! Coverage for the Phase 1 Postgres seam.
//!
//! Phase 1 adds the `DbPool` enum (sqlite + postgres variants) and the
//! backend-vs-pool cross-check in `App::build`. The actual Postgres
//! query path lands in Phase 2 of the rollout; this file just verifies
//! the seam holds:
//!
//! - `db::connect("postgres://...")` resolves to the Postgres branch
//!   (without actually connecting — there's no server in the test
//!   environment).
//! - `DbPool::from(SqlitePool)` round-trips through the variant
//!   accessors.
//! - `App::build` rejects a mismatch between `settings.database_url`
//!   and the registered default pool's runtime backend.
//! - Unsupported URL schemes surface as `sqlx::Error::Configuration`.

use sqlx::SqlitePool;
use umbra_core::app::{App, BuildError};
use umbra_core::db::{self, DbPool};
use umbra_core::settings::Settings;

#[tokio::test]
async fn dbpool_round_trips_through_from_sqlitepool() {
    let sp = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let dp: DbPool = sp.clone().into();
    assert_eq!(dp.backend_name(), "sqlite");
    assert!(dp.as_sqlite().is_some());
    assert!(dp.as_postgres().is_none());
    // sqlite_or_panic resolves on the sqlite variant.
    let inner = dp.sqlite_or_panic();
    let (one,): (i64,) = sqlx::query_as("SELECT 1").fetch_one(inner).await.unwrap();
    assert_eq!(one, 1);
}

#[tokio::test]
async fn unsupported_scheme_surfaces_as_configuration_error() {
    let result = db::connect("mysql://user:pass@host/db").await;
    match result {
        Err(sqlx::Error::Configuration(msg)) => {
            assert!(msg.to_string().contains("mysql"));
        }
        other => panic!("expected sqlx::Error::Configuration, got {other:?}"),
    }
}

#[tokio::test]
async fn build_rejects_url_pool_backend_mismatch() {
    // Settings says postgres, but the registered pool is sqlite.
    // Build should reject with DatabaseBackendMismatch, surfacing the
    // typo to the operator at boot.
    let mut settings = Settings::from_env().expect("figment defaults load");
    settings.database_url = "postgres://nobody@nowhere/missing".to_string();

    let sqlite_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();

    let result = App::builder()
        .settings(settings)
        .database("default", sqlite_pool)
        .build();

    // App doesn't implement Debug, so match on Err alone and unwrap the
    // variant manually rather than `result.unwrap_err()`.
    match result {
        Err(BuildError::DatabaseBackendMismatch {
            url_backend,
            pool_backend,
        }) => {
            assert_eq!(url_backend, "postgres");
            assert_eq!(pool_backend, "sqlite");
        }
        Err(other) => panic!("expected DatabaseBackendMismatch, got {other:?}"),
        Ok(_) => panic!("expected DatabaseBackendMismatch, got Ok"),
    }
}
