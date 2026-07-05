//! Multi-DB routing: per-model `#[umbral(database = "alias")]` + per-DB
//! migration tracking.
//!
//! This test boots an `App` with two registered SQLite pools and two
//! models — one routed to `default`, one to `analytics` via the new
//! struct-level attribute. After `migrate::run_in`, each pool should
//! have ONLY the table that routes to it, and each pool's
//! `umbral_migrations` tracking table should record exactly the
//! migrations whose operations ran against that pool.
//!
//! Covers the v1 contract for gap #53:
//!
//! 1. The model attribute → `Model::DATABASE` → `ModelMeta::database`
//!    chain plumbs the alias all the way to `init_model_aliases`.
//! 2. `db::registered_aliases()` returns the alphabetical list.
//! 3. `migrate::table_alias(table)` resolves a table name to its
//!    owning pool alias, falling back to "default".
//! 4. `migrate::run_in` walks every alias, filters operations to those
//!    targeting tables routed to that pool, and records tracking rows
//!    only on pools that actually ran SQL.

use std::path::PathBuf;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::Model;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, umbral::orm::Model)]
#[umbral(table = "primary_article")]
pub struct PrimaryArticle {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, umbral::orm::Model)]
#[umbral(table = "analytics_event", database = "analytics")]
pub struct AnalyticsEvent {
    pub id: i64,
    pub event_name: String,
}

static BOOT: OnceCell<PathBuf> = OnceCell::const_new();

async fn boot_app_with_two_dbs() -> &'static PathBuf {
    BOOT.get_or_init(|| async {
        // Use file-backed sqlite (in tempdirs) instead of `:memory:`
        // so multi-connection pools share the same DB. axum/sqlx's
        // in-memory pool gives each connection its own private DB,
        // which would make "table exists" assertions on a later
        // connection fail.
        let default_tmp = tempfile::tempdir().expect("default tempdir");
        let analytics_tmp = tempfile::tempdir().expect("analytics tempdir");
        let default_path = default_tmp.path().join("default.db");
        let analytics_path = analytics_tmp.path().join("analytics.db");
        std::mem::forget(default_tmp);
        std::mem::forget(analytics_tmp);

        let default_pool = make_pool(&default_path).await;
        let analytics_pool = make_pool(&analytics_path).await;

        let settings = umbral::Settings::from_env().expect("settings load");

        umbral::App::builder()
            .settings(settings)
            .database("default", default_pool)
            .database("analytics", analytics_pool)
            .model::<PrimaryArticle>()
            .model::<AnalyticsEvent>()
            .build()
            .expect("App::build should accept two pools and two models");

        let mig_dir = tempfile::tempdir().expect("migration tempdir");
        let mig_path = mig_dir.path().to_path_buf();
        std::mem::forget(mig_dir);

        umbral::migrate::make_in(&mig_path)
            .await
            .expect("makemigrations should write per-model CreateTable ops");
        umbral::migrate::run_in(&mig_path)
            .await
            .expect("run_in should apply per-alias");

        mig_path
    })
    .await
}

async fn make_pool(db_path: &std::path::Path) -> SqlitePool {
    let options = SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
        .filename(db_path)
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("sqlite file-backed pool")
}

#[tokio::test(flavor = "multi_thread")]
async fn model_database_attribute_lands_in_runtime_meta() {
    // The macro should have emitted `Model::DATABASE` and the trait
    // should expose it. Verify both poles of the attribute pipeline:
    // the default (None) and the override.
    assert_eq!(PrimaryArticle::DATABASE, None);
    assert_eq!(AnalyticsEvent::DATABASE, Some("analytics"));
}

#[tokio::test(flavor = "multi_thread")]
async fn table_alias_resolves_per_model_attribute() {
    let _ = boot_app_with_two_dbs().await;

    // `primary_article` has no override → "default".
    assert_eq!(umbral::migrate::table_alias("primary_article"), "default");
    // `analytics_event` was tagged with `database = "analytics"`.
    assert_eq!(umbral::migrate::table_alias("analytics_event"), "analytics");
    // Unknown tables fall through to "default" so framework book-
    // keeping (`umbral_migrations` itself, orphan schema) lands on
    // the main pool.
    assert_eq!(umbral::migrate::table_alias("not_a_table"), "default");
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_aliases_lists_both_pools_in_alphabetical_order() {
    let _ = boot_app_with_two_dbs().await;
    assert_eq!(
        umbral::db::registered_aliases(),
        vec!["analytics".to_string(), "default".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_pool_has_only_its_routed_table() {
    let _ = boot_app_with_two_dbs().await;

    // Default pool should have `primary_article` but NOT
    // `analytics_event`.
    let default_pool = umbral::db::pool_for("default");
    let primary_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
    )
    .bind("primary_article")
    .fetch_one(&default_pool)
    .await
    .unwrap();
    assert!(primary_exists, "primary_article must exist on default pool");

    let analytic_on_default: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
    )
    .bind("analytics_event")
    .fetch_one(&default_pool)
    .await
    .unwrap();
    assert!(
        !analytic_on_default,
        "analytics_event must NOT exist on default pool"
    );

    // Analytics pool should have the inverse.
    let analytics_pool = umbral::db::pool_for("analytics");
    let analytic_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
    )
    .bind("analytics_event")
    .fetch_one(&analytics_pool)
    .await
    .unwrap();
    assert!(
        analytic_exists,
        "analytics_event must exist on analytics pool"
    );

    let primary_on_analytics: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
    )
    .bind("primary_article")
    .fetch_one(&analytics_pool)
    .await
    .unwrap();
    assert!(
        !primary_on_analytics,
        "primary_article must NOT exist on analytics pool"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_pool_has_its_own_umbral_migrations_tracking_table() {
    let _ = boot_app_with_two_dbs().await;

    // Both pools should carry the tracking table; only the file
    // whose ops actually landed in that pool should be recorded
    // there. Since `makemigrations` emits one file per plugin (the
    // implicit "app") with both CreateTable ops bundled, both pools
    // record the same file id — that's correct: the file's
    // effects did land partially in each pool.
    for alias in ["default", "analytics"] {
        let pool = umbral::db::pool_for(alias);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            count >= 1,
            "expected at least one tracking row on `{alias}`, got {count}"
        );
    }
}
