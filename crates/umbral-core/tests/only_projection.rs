//! Gap #111 — `QuerySet::only(&["id", "name"])` and `Manager::only(...)`.
//!
//! `.only(...)` records a column projection that:
//!   - trims `to_sql()` / `to_sql_pg()` to just those columns,
//!   - errors on the typed terminals (`fetch` / `first` / `get`)
//!     with a message pointing the caller at `.values(...)`.
//!
//! The execution path for projected reads stays `.values(&[...])`
//! (returns `Vec<serde_json::Value>`); `.only()` is the
//! chainable + inspectable shape for the same set.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::DynQuerySet;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "op_brand")]
pub struct Brand {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub slug: String,
    pub website: Option<String>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Brand>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        for (name, slug) in &[("Acme", "acme"), ("UmbralGear", "umbralgear")] {
            sqlx::query("INSERT INTO op_brand (name, slug) VALUES (?, ?)")
                .bind(*name)
                .bind(*slug)
                .execute(&pool)
                .await
                .expect("seed");
        }
        // Tame the dead-code lint for the helper a fresh test would
        // call but this suite doesn't currently exercise.
        let _ = DynQuerySet::for_meta;
    })
    .await;
}

#[tokio::test]
async fn to_sql_without_only_emits_full_select() {
    boot().await;
    let sql = Brand::objects().filter(brand::ID.eq(1)).to_sql();
    // Every column on the model lands in the SELECT list.
    assert!(sql.contains("\"id\""), "sql: {sql}");
    assert!(sql.contains("\"name\""), "sql: {sql}");
    assert!(sql.contains("\"slug\""), "sql: {sql}");
    assert!(sql.contains("\"website\""), "sql: {sql}");
}

#[tokio::test]
async fn to_sql_with_only_trims_select_to_named_columns() {
    boot().await;
    let sql = Brand::objects()
        .filter(brand::ID.eq(1))
        .only(&["id", "name"])
        .to_sql();
    assert!(sql.contains("\"id\""), "sql: {sql}");
    assert!(sql.contains("\"name\""), "sql: {sql}");
    assert!(!sql.contains("\"slug\""), "slug must NOT be in: {sql}");
    assert!(
        !sql.contains("\"website\""),
        "website must NOT be in: {sql}"
    );
    // Predicate still rides along — `.only` is projection only.
    assert!(sql.contains("WHERE"), "sql: {sql}");
}

#[tokio::test]
async fn to_sql_pg_with_only_trims_select_to_named_columns() {
    boot().await;
    let sql = Brand::objects()
        .filter(brand::ID.eq(1))
        .only(&["id"])
        .to_sql_pg();
    assert!(sql.contains("\"id\""), "sql: {sql}");
    assert!(!sql.contains("\"name\""), "name must NOT be in: {sql}");
    assert!(
        sql.contains("$1"),
        "expected $1 placeholder in pg sql: {sql}"
    );
}

#[tokio::test]
async fn fetch_with_only_set_errors_clearly() {
    boot().await;
    let err = Brand::objects()
        .only(&["id", "name"])
        .fetch()
        .await
        .expect_err("fetch must reject when .only(...) is set");
    let msg = err.to_string();
    assert!(
        msg.contains(".only(...)") && msg.contains(".values"),
        "error must name `.only` and point at `.values`: got {msg}"
    );
    assert!(msg.contains("fetch"), "error must name the terminal: {msg}");
}

#[tokio::test]
async fn first_with_only_set_errors_clearly() {
    boot().await;
    let err = Brand::objects()
        .filter(brand::ID.eq(1))
        .only(&["id"])
        .first()
        .await
        .expect_err("first must reject when .only(...) is set");
    assert!(err.to_string().contains("first"));
}

#[tokio::test]
async fn get_with_only_set_errors_via_underlying_fetch_guard() {
    boot().await;
    use umbral::orm::GetError;
    let err = Brand::objects()
        .filter(brand::ID.eq(1))
        .only(&["id"])
        .get()
        .await
        .expect_err("get must reject when .only(...) is set");
    match err {
        GetError::Sqlx(e) => assert!(e.to_string().contains(".only(...)"), "{e}"),
        other => panic!("expected Sqlx-wrapped error, got {other:?}"),
    }
}

#[tokio::test]
async fn manager_only_forwards_to_queryset() {
    boot().await;
    let sql = Brand::objects().only(&["id", "slug"]).to_sql();
    assert!(sql.contains("\"id\""));
    assert!(sql.contains("\"slug\""));
    assert!(!sql.contains("\"name\""));
}

#[tokio::test]
async fn values_remains_the_execution_path_for_projected_reads() {
    boot().await;
    // .values(...) is independent of .only() — its own arg wins. This
    // is the canonical execution path for "select these columns" per
    // the gap #111 close-out: .only() inspects, .values() executes.
    let rows = Brand::objects()
        .values(&["id", "name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let obj = row.as_object().expect("object");
        assert_eq!(obj.len(), 2, "exactly the two projected columns");
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("name"));
    }
}
