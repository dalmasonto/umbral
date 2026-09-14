//! ORM heavy-relations epic, Plan A Task 3 — `walk_joins`, the one walker
//! for every hop kind.
//!
//! Drives `walk_joins` through the public `RelPath::from_path` +
//! `to_sql_for_path` probe surface (never a hand-rolled SQL string): a
//! forward-FK chain resolves to the expected INNER JOIN count, and a single
//! hop keeps its pre-unification lightweight shape (PERFORMANCE — the
//! unification must not route one hop through the heavier multi-JOIN plan).
//! Per the project's testing rule, the SQL-shape assertions never stand
//! alone: every test also EXECUTES the built statement against real seeded
//! rows and reads the object graph back.
//!
//! `RelPath::from_path` always roots at `PathBase::TableRoot` (no single
//! object), so every test here doubles as an execution proof for that new
//! base's distinct shape: a bare `FROM <table> JOIN ...` with no
//! `WHERE`/`LIMIT`, which enumerates every root row the JOIN chain reaches
//! rather than anchoring on one PK. `table_root_path_enumerates_every_root_row`
//! makes that enumeration property the primary assertion (row count equals
//! the number of seeded root rows, not 1), since it had never been executed
//! against real data before this suite.
//!
//! All three tests share ONE seeded dataset via the process-wide `boot()`
//! (like every other test file in this suite) — seeding happens exactly
//! once, inside the `OnceCell`, so concurrently-running `#[tokio::test]`
//! functions never race each other over shared rows.
//!
//! The SECURITY proof (every joined table schema-qualified under an
//! installed tenant router) lives in its own process — `walk_joins_schema_qualified.rs`
//! — because the schema router and the model registry are both
//! process-wide `OnceLock`s: installing a `SchemaRouter` in the same test
//! binary as these `DefaultRouter` tests would leak across them depending on
//! test execution order. Schema-per-tenant isn't round-trippable on SQLite
//! (SQLite has no schemas), so that file stays SQL-string-only — matching
//! `router_schema_qualified.rs`'s precedent.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
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

/// Seeded once, shared read-only by every test in this file (the usual
/// pattern across this suite): Acme(1) employs ada(1); Globex(2) employs
/// grace(2). ada authored two posts, grace authored one — three `Post` rows
/// total, spread across two distinct companies, so a query that enumerates
/// every root row (the `TableRoot` shape) is distinguishable from one that
/// accidentally narrows to a single row or a single company.
async fn boot() -> SqlitePool {
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

        sqlx::query("INSERT INTO wjs_company (name) VALUES ('Acme')")
            .execute(&pool)
            .await
            .expect("seed company 1");
        sqlx::query("INSERT INTO wjs_company (name) VALUES ('Globex')")
            .execute(&pool)
            .await
            .expect("seed company 2");
        sqlx::query("INSERT INTO wjs_user (name, company) VALUES ('ada', 1)")
            .execute(&pool)
            .await
            .expect("seed user 1");
        sqlx::query("INSERT INTO wjs_user (name, company) VALUES ('grace', 2)")
            .execute(&pool)
            .await
            .expect("seed user 2");
        sqlx::query("INSERT INTO wjs_post (title, author) VALUES ('Hello', 1)")
            .execute(&pool)
            .await
            .expect("seed post 1");
        sqlx::query("INSERT INTO wjs_post (title, author) VALUES ('World', 1)")
            .execute(&pool)
            .await
            .expect("seed post 2");
        sqlx::query("INSERT INTO wjs_post (title, author) VALUES ('Extra', 2)")
            .execute(&pool)
            .await
            .expect("seed post 3");

        pool
    })
    .await
    .clone()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort_unstable();
    v
}

/// A 2-hop forward FK chain (`Post.author -> User`, `User.company ->
/// Company`) emits exactly two INNER JOINs off the single flat SELECT, AND
/// — executed for real — reads the actual leaf `Company` rows back: one per
/// seeded `Post`, since the `TableRoot` base enumerates every root row.
#[tokio::test]
async fn walk_joins_emits_inner_join_for_forward_fk_chain() {
    let pool = boot().await;
    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author__company").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();

    // SQL-shape assertions (never the sole assertion — see the round-trip
    // below).
    assert!(sql.contains("INNER JOIN"), "sql: {sql}");
    assert_eq!(
        sql.matches("JOIN").count(),
        2,
        "two hops -> two joins: {sql}"
    );

    // Round-trip: execute the exact statement `walk_joins` built and read
    // the real leaf rows back.
    let rows: Vec<Company> = sqlx::query_as::<sqlx::Sqlite, Company>(&sql)
        .fetch_all(&pool)
        .await
        .expect("execute the 2-hop chain");
    assert_eq!(
        rows.len(),
        3,
        "one Company row per seeded Post (2 @ Acme via ada, 1 @ Globex via grace): {rows:?}"
    );
    let names = sorted(rows.iter().map(|c| c.name.clone()).collect());
    assert_eq!(names, vec!["Acme", "Acme", "Globex"]);
}

/// PERFORMANCE: a single hop keeps its lightweight shape — the unification
/// must not route one hop through the heavy multi-table JOIN plan. Executed
/// for real: reads the actual leaf `User` rows back (one per seeded `Post`).
#[tokio::test]
async fn single_hop_stays_lightweight() {
    let pool = boot().await;
    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<User>(&path).unwrap();
    assert!(
        sql.matches(" JOIN ").count() <= 1,
        "single hop must stay lightweight: {sql}"
    );

    let rows: Vec<User> = sqlx::query_as::<sqlx::Sqlite, User>(&sql)
        .fetch_all(&pool)
        .await
        .expect("execute the single-hop path");
    assert_eq!(
        rows.len(),
        3,
        "one User row per seeded Post (2 x ada, 1 x grace): {rows:?}"
    );
    let names = sorted(rows.iter().map(|u| u.name.clone()).collect());
    assert_eq!(names, vec!["ada", "ada", "grace"]);
}

/// The `PathBase::TableRoot` base has a shape distinct from the
/// object-rooted `SinglePk` base: no `WHERE root.pk = ?`, no `LIMIT 1` — it
/// enumerates every row the JOIN chain reaches from the WHOLE root table.
/// `RelPath::from_path` always produces this base, but until this test it
/// had never been executed against real data. The seeded dataset spans TWO
/// companies via two different authors, so the row count can only be right
/// (3, not 1) if the query is genuinely unfiltered and unlimited — a stray
/// `WHERE`/`LIMIT` bug would silently drop rows here.
#[tokio::test]
async fn table_root_path_enumerates_every_root_row() {
    let pool = boot().await;
    use umbral::orm::relation::{PathBase, RelPath};

    let path = RelPath::from_path::<Post>("author__company").unwrap();
    match path.base {
        PathBase::TableRoot { table } => assert_eq!(table, "wjs_post"),
        _ => panic!("RelPath::from_path must root at PathBase::TableRoot"),
    }
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();
    assert!(
        !sql.to_uppercase().contains("WHERE"),
        "a TableRoot-based path has no single row to anchor on: {sql}"
    );
    assert!(
        !sql.to_uppercase().contains("LIMIT"),
        "a TableRoot-based path enumerates every root row, not one: {sql}"
    );

    let rows: Vec<Company> = sqlx::query_as::<sqlx::Sqlite, Company>(&sql)
        .fetch_all(&pool)
        .await
        .expect("execute the TableRoot-based path");
    assert_eq!(
        rows.len(),
        3,
        "3 seeded posts -> 3 rows (2 @ Acme, 1 @ Globex) — proves the \
         no-WHERE/no-LIMIT enumeration shape actually runs: {rows:?}"
    );
    let names = sorted(rows.iter().map(|c| c.name.clone()).collect());
    assert_eq!(names, vec!["Acme", "Acme", "Globex"]);
}
