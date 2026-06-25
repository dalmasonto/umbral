//! Task 9 — `DatabaseRouter::allow_migrate` gates per-alias migration ops.
//!
//! A custom router that vetoes `allow_migrate` for a specific table must
//! prevent that table from being created during `migrate::run_in`. The
//! default router path is covered by the existing `multi_database` suite
//! (which already verifies every table lands on its assigned alias with no
//! custom router). This file focuses on the veto contract.
//!
//! Process-wide ambient state (settings, router, model registry) is
//! published exactly once via a `OnceCell`-guarded boot function. All tests
//! share that single boot.

use std::path::PathBuf;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::db::DatabaseRouter;
use umbral::migrate::ModelMeta;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Model whose table the custom router vetoes from migration.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mig_gated")]
pub struct MigGated {
    pub id: i64,
    pub name: String,
}

/// Bystander model — the router never vetoes this; it must always be created.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mig_bystander")]
pub struct MigBystander {
    pub id: i64,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Custom router
// ---------------------------------------------------------------------------

/// Vetoes migration of `mig_gated` on every alias; allows everything else.
struct GateRouter;

impl DatabaseRouter for GateRouter {
    fn allow_migrate(&self, _alias: &str, model: &ModelMeta) -> bool {
        model.table != "mig_gated"
    }
}

// ---------------------------------------------------------------------------
// Single shared boot
// ---------------------------------------------------------------------------

/// Shared state produced once: the migrations dir path + db file path.
struct BootState {
    db_path: PathBuf,
    _mig_path: PathBuf,
}

static BOOT: OnceCell<BootState> = OnceCell::const_new();

async fn boot() -> &'static BootState {
    BOOT.get_or_init(|| async {
        // File-backed SQLite so all pool connections share the same DB.
        let db_tmp = tempfile::tempdir().expect("tempdir for router_allow_migrate");
        let db_path = db_tmp.path().join("gated.db");
        std::mem::forget(db_tmp);

        let pool = make_pool(&db_path).await;
        let settings = umbral::Settings::from_env().expect("settings load");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .router(GateRouter)
            .model::<MigGated>()
            .model::<MigBystander>()
            .build()
            .expect("App::build must succeed even with a vetoing router");

        let mig_tmp = tempfile::tempdir().expect("migration tempdir");
        let mig_path = mig_tmp.path().to_path_buf();
        std::mem::forget(mig_tmp);

        umbral::migrate::make_in(&mig_path)
            .await
            .expect("makemigrations must write operations");

        umbral::migrate::run_in(&mig_path)
            .await
            .expect("run_in must succeed");

        BootState {
            db_path,
            _mig_path: mig_path,
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_pool(db_path: &std::path::Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("sqlite file-backed pool")
}

async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The vetoed table must NOT be created when the router's `allow_migrate`
/// returns `false` for it.
#[tokio::test(flavor = "multi_thread")]
async fn router_veto_prevents_table_creation() {
    let state = boot().await;
    let pool = make_pool(&state.db_path).await;

    assert!(
        !table_exists(&pool, "mig_gated").await,
        "mig_gated must NOT be created when the router vetoes allow_migrate"
    );
}

/// The bystander table (never vetoed) must be created even when a custom
/// router is installed — confirming the veto is model-specific, not global.
#[tokio::test(flavor = "multi_thread")]
async fn router_allows_unvetoed_table() {
    let state = boot().await;
    let pool = make_pool(&state.db_path).await;

    assert!(
        table_exists(&pool, "mig_bystander").await,
        "mig_bystander must be created — the router never vetoes it"
    );
}

/// Unit check: `DefaultRouter::allow_migrate` returns `true` for any
/// (alias, model) pair where the alias matches the model's assigned alias.
/// This is a pure function test — no `App::build`, no DB.
#[test]
fn default_router_allow_migrate_is_permissive_for_assigned_alias() {
    use umbral::db::{DatabaseRouter, DefaultRouter};
    use umbral::migrate::ModelMeta;

    let meta = ModelMeta {
        name: "MigGated".to_string(),
        table: "mig_gated".to_string(),
        database: None, // → resolves to "default"
        fields: vec![],
        display: String::new(),
        icon: "database".to_string(),
        singleton: false,
        unique_together: vec![],
        indexes: vec![],
        ordering: vec![],
        m2m_relations: vec![],
        soft_delete: false,
        app_label: "app".to_string(),
    };

    let router = DefaultRouter;
    // The default router must allow migration on the model's own alias.
    assert!(
        router.allow_migrate("default", &meta),
        "DefaultRouter must allow_migrate on the model's assigned alias"
    );
    // And must deny it on a different alias (table_alias check catches this
    // before the router is even consulted, but confirm the contract).
    assert!(
        !router.allow_migrate("analytics", &meta),
        "DefaultRouter must NOT allow_migrate on a non-assigned alias"
    );
}
