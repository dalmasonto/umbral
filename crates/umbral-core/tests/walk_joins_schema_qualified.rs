//! ORM heavy-relations epic, Plan A Task 3 (SECURITY) — `walk_joins` must
//! schema-qualify EVERY joined table (root, every intermediate hop, and a
//! junction), not just the query's own root. A miss here crosses a tenant
//! boundary under the schema-per-tenant router.
//!
//! Mirrors `router_schema_qualified.rs`'s pattern (an installed
//! `DatabaseRouter::schema_for` returning `Some(schema)` makes every table
//! position dot-qualified) but drives it through `RelPath::from_path` +
//! `walk_joins`, over a genuine 2-hop forward-FK chain, so the qualifier is
//! proven for a JOIN target, not just a query's own FROM.
//!
//! Kept in its own process (own test binary): the schema router and the
//! model registry are both process-wide `OnceLock`s, so installing a
//! `SchemaRouter` here must not run in the same binary as the
//! `DefaultRouter` assertions in `walk_joins_sql.rs`.

use umbral::db::{DatabaseRouter, RouteContext, Schema};
use umbral::orm::{ForeignKey, Model};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_company")]
pub struct Company {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_user")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

/// Unconditionally scopes every request to schema `tenant1`, ignoring ctx.
struct SchemaRouter;
impl DatabaseRouter for SchemaRouter {
    fn schema_for(&self, _ctx: &RouteContext) -> Option<Schema> {
        Some(Schema::new("tenant1").expect("valid schema identifier"))
    }
}

async fn make_pool() -> sqlx::SqlitePool {
    umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn walk_joins_schema_qualifies_every_hop() {
    let pool = make_pool().await;

    umbral::App::builder()
        .settings(umbral::Settings::from_env().expect("settings load"))
        .database("default", pool)
        .router(SchemaRouter)
        .model::<Company>()
        .model::<User>()
        .model::<Post>()
        .build()
        .expect("App::build");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");

    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author__company").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();

    // Root (post) + hop 1 (user) + hop 2 (company) = 3 schema-qualified
    // table references, every one under "tenant1".
    assert_eq!(
        sql.matches("\"tenant1\"").count(),
        3,
        "root + 2 hops all schema-qualified under tenant1: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Post::TABLE)),
        "root not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", User::TABLE)),
        "hop 1 (user) not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Company::TABLE)),
        "hop 2 (company) not schema-qualified: {sql}"
    );
}
