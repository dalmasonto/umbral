//! ORM heavy-relations epic, Plan A Task 3 — `walk_joins`, the one walker
//! for every hop kind.
//!
//! Drives `walk_joins` through the public `RelPath::from_path` +
//! `to_sql_for_path` probe surface (never a hand-rolled SQL string): a
//! forward-FK chain resolves to the expected INNER JOIN count, and a single
//! hop keeps its pre-unification lightweight shape (PERFORMANCE — the
//! unification must not route one hop through the heavier multi-JOIN plan).
//!
//! The SECURITY proof (every joined table schema-qualified under an
//! installed tenant router) lives in its own process — `walk_joins_schema_qualified.rs`
//! — because the schema router and the model registry are both
//! process-wide `OnceLock`s: installing a `SchemaRouter` in the same test
//! binary as these `DefaultRouter` tests would leak across them depending on
//! test execution order.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "wjs_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "wjs_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "wjs_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Company>()
            .model::<User>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await;
}

/// A 2-hop forward FK chain (`Post.author -> User`, `User.company ->
/// Company`) emits exactly two INNER JOINs off the single flat SELECT.
#[tokio::test]
async fn walk_joins_emits_inner_join_for_forward_fk_chain() {
    boot().await;
    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author__company").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();
    assert!(sql.contains("INNER JOIN"), "sql: {sql}");
    assert_eq!(
        sql.matches("JOIN").count(),
        2,
        "two hops -> two joins: {sql}"
    );
}

/// PERFORMANCE: a single hop keeps its lightweight shape — the unification
/// must not route one hop through the heavy multi-table JOIN plan.
#[tokio::test]
async fn single_hop_stays_lightweight() {
    boot().await;
    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<User>(&path).unwrap();
    assert!(
        sql.matches(" JOIN ").count() <= 1,
        "single hop must stay lightweight: {sql}"
    );
}
