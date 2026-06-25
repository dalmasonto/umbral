//! Task 8 — the cross-database FK guard (gaps2 #22) routes through
//! `DatabaseRouter::allow_relation`. A custom router can VETO an FK that
//! the default same-DB guard would allow, and with NO custom router the
//! default same-DB FK still builds (non-regression).
//!
//! Both models live on `default`, so the build-time alias check
//! (`alias_of(a) == alias_of(b)`) returns `true` for the pair. The custom
//! router overrides that to `false`, proving its veto reaches the guard.
//!
//! Each `App::build()` publishes process-wide ambient state via a
//! `OnceLock` that panics on a second `db::init`. Only the no-router
//! success build reaches Phase 3; the veto build fails in Phase 2.5b
//! (before any ambient publish), so the two tests never collide.

use umbral::db::DatabaseRouter;
use umbral::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "ra_parent")]
pub struct RaParent {
    pub id: i64,
    pub label: String,
}

/// Same-DB child with a real (db_constraint = true) FK to the parent. The
/// default guard ALLOWS this; a vetoing router must reject it.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "ra_child")]
pub struct RaChild {
    pub id: i64,
    #[umbral(no_reverse)]
    pub parent: umbral::orm::ForeignKey<RaParent>,
}

async fn mem_pool() -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool")
}

/// A router that refuses every relation. With it installed, even a legal
/// same-DB FK must be rejected by the Phase 2.5b guard.
struct VetoRouter;
impl DatabaseRouter for VetoRouter {
    fn allow_relation(&self, _a: &ModelMeta, _b: &ModelMeta) -> bool {
        false
    }
}

/// With a vetoing custom router, a normally-legal same-DB FK is rejected:
/// `build()` returns `CrossDatabaseForeignKey`. This errors in Phase 2.5b
/// before any ambient publish, so it is safe alongside the success build.
#[tokio::test(flavor = "multi_thread")]
async fn custom_router_veto_rejects_same_db_fk() {
    use umbral_core::app::BuildError;

    let mut settings = umbral::Settings::from_env().expect("settings load");
    settings.database_url = "sqlite::memory:".to_string();

    let result = umbral::App::builder()
        .settings(settings)
        .database("default", mem_pool().await)
        .router(VetoRouter)
        .model::<RaParent>()
        .model::<RaChild>()
        .build();

    match result {
        Err(BuildError::CrossDatabaseForeignKey {
            model,
            field,
            model_db,
            target_db,
        }) => {
            assert_eq!(model, "RaChild");
            assert_eq!(field, "parent");
            // Both resolve to "default" — the veto, not an alias mismatch,
            // is what failed the build.
            assert_eq!(model_db, "default");
            assert_eq!(target_db, "default");
        }
        Err(other) => {
            panic!("expected CrossDatabaseForeignKey from the router veto, got {other:?}")
        }
        Ok(_) => panic!("expected the router veto to fail the build, but it succeeded"),
    }
}

/// Non-regression: the SAME two models with NO custom router build
/// successfully — the default same-DB FK is allowed. This is the one
/// success build in this binary; it publishes the ambient registry.
#[tokio::test(flavor = "multi_thread")]
async fn no_router_allows_same_db_fk() {
    let mut settings = umbral::Settings::from_env().expect("settings load");
    settings.database_url = "sqlite::memory:".to_string();

    umbral::App::builder()
        .settings(settings)
        .database("default", mem_pool().await)
        .model::<RaParent>()
        .model::<RaChild>()
        .build()
        .expect("a same-DB FK with no custom router must build");
}
