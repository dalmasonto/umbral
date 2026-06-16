//! Option-C schema-per-tenant — the SQL-qualification seam. When the installed
//! `DatabaseRouter::schema_for(ctx)` returns `Some(schema)`, every ORM table
//! position renders schema-qualified (`"tenant_7"."sq_widget"`); when it
//! returns `None` (the `DefaultRouter` default) the table is bare. The bare
//! case is covered by the `db::router` unit test (a fresh process with no
//! installed router); this file proves the QUALIFIED output under an installed
//! schema router.
//!
//! No live database is required: `schema_qualified_table` is a pure SQL-builder
//! helper that consults the ambient router, so rendering a sea-query statement
//! is enough to observe the qualification.

use umbra::db::{DatabaseRouter, RouteContext, Schema};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sq_widget")]
pub struct SqWidget {
    pub id: i64,
    pub name: String,
}

/// Unconditionally scopes every request to schema `tenant_7`, ignoring ctx.
struct SchemaRouter;
impl DatabaseRouter for SchemaRouter {
    fn schema_for(&self, _ctx: &RouteContext) -> Option<Schema> {
        Some(Schema::new("tenant_7").expect("valid schema identifier"))
    }
}

async fn make_pool() -> sqlx::SqlitePool {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sq_widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_router_qualifies_table_references() {
    let pool = make_pool().await;

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings load"))
        .database("default", pool)
        .router(SchemaRouter)
        .model::<SqWidget>()
        .build()
        .unwrap();

    // The public SQL-builder seam every FROM/JOIN/INSERT table position routes
    // through. Under the installed `SchemaRouter` it must dot-qualify.
    let table_ref = umbra_core::db::router::schema_qualified_table("sq_widget");
    let sql = sea_query::Query::select()
        .column(sea_query::Asterisk)
        .from(table_ref)
        .to_string(sea_query::PostgresQueryBuilder);
    assert!(
        sql.contains("\"tenant_7\".\"sq_widget\""),
        "expected schema-qualified table, got: {sql}"
    );

    // A different table name is qualified with the same schema — the helper
    // qualifies whatever table it's handed, not a hard-coded one.
    let other = umbra_core::db::router::schema_qualified_table("sq_other");
    let other_sql = sea_query::Query::select()
        .column(sea_query::Asterisk)
        .from(other)
        .to_string(sea_query::PostgresQueryBuilder);
    assert!(
        other_sql.contains("\"tenant_7\".\"sq_other\""),
        "expected schema-qualified table, got: {other_sql}"
    );
}
