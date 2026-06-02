//! Gap 14 — ForeignKey<T> field type.
//!
//! Coverage:
//!
//! - **Derive classification.** `ForeignKey<User>` on a model produces a
//!   `SqlType::ForeignKey` field spec with the correct `fk_target`.
//! - **Column constant.** The sibling module exposes a `ForeignKeyCol`
//!   constant, and `.eq(1)` produces a well-formed predicate.
//! - **DDL rendering — SQLite.** `render_operation_for` against `"sqlite"`
//!   emits `REFERENCES "user"("id")`.
//! - **DDL rendering — Postgres.** Same but against `"postgres"`.
//! - **Live SQLite round-trip.** Insert a User, insert a Post referencing
//!   it, call `post.author.resolve(&pool)` and get the User back.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbra::migrate::{Column, Operation, render_operation_for};
use umbra::orm::{ForeignKey, Model, SqlType};

// =========================================================================
// Model declarations
// =========================================================================

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "fk_user")]
pub struct User {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fk_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

// =========================================================================
// Derive classification
// =========================================================================

/// `ForeignKey<User>` on Post classifies as `SqlType::ForeignKey` and carries
/// the referenced table name in `fk_target`.
#[test]
fn derive_classifies_foreign_key_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> = <Post as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let author_field = by_name.get("author").expect("author field must exist");
    assert_eq!(
        author_field.ty,
        SqlType::ForeignKey,
        "ForeignKey<User> should classify as SqlType::ForeignKey"
    );
    assert_eq!(
        author_field.fk_target,
        Some("fk_user"),
        "fk_target should be User::TABLE"
    );
    assert!(
        !author_field.nullable,
        "non-Option ForeignKey is not nullable"
    );
    assert!(!author_field.primary_key, "FK field is not the primary key");
}

// =========================================================================
// Column constant
// =========================================================================

/// `post::AUTHOR.eq(1)` should compile and produce a predicate — that the
/// derive emitted a `ForeignKeyCol` constant for the `author` field.
///
/// The sibling module is named after the struct (`post`), not the table
/// (`fk_post`). The `#[derive(Model)]` macro uses `to_snake_case(struct_name)`.
#[test]
fn foreign_key_col_constant_eq_compiles() {
    // `post` is the sibling module emitted for the `Post` struct.
    use post::AUTHOR;
    let qs = Post::objects().filter(AUTHOR.eq(1));
    let sql = qs.to_sql();
    assert!(
        sql.contains("author"),
        "WHERE clause should reference `author`; got: {sql}"
    );
}

/// `post::AUTHOR.in_(&[1, 2])` compiles and includes the column name.
#[test]
fn foreign_key_col_constant_in_compiles() {
    use post::AUTHOR;
    let qs = Post::objects().filter(AUTHOR.in_(&[1i64, 2i64]));
    let sql = qs.to_sql();
    assert!(
        sql.contains("author"),
        "IN predicate should reference `author`; got: {sql}"
    );
}

// =========================================================================
// DDL rendering
// =========================================================================

/// SQLite `CreateTable` for a model with a ForeignKey column should include
/// `REFERENCES "fk_user"("id")` in the rendered DDL.
#[test]
fn create_table_emits_references_sqlite() {
    let op = Operation::CreateTable {
        table: "fk_post".to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
            Column {
                name: "title".to_string(),
                ty: SqlType::Text,
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
            Column {
                name: "author".to_string(),
                ty: SqlType::ForeignKey,
                primary_key: false,
                nullable: false,
                fk_target: Some("fk_user".to_string()),
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
        ],
    };

    let stmts = render_operation_for(&op, "sqlite");
    assert_eq!(stmts.len(), 1, "CreateTable should emit one statement");
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("references"),
        "SQLite DDL should contain REFERENCES; got: {sql}"
    );
    assert!(
        lower.contains("\"fk_user\""),
        "SQLite DDL should reference `fk_user`; got: {sql}"
    );
    assert!(
        lower.contains("(\"id\")"),
        "SQLite DDL should reference column `id`; got: {sql}"
    );
    assert!(
        lower.contains("bigint"),
        "FK column should be BIGINT; got: {sql}"
    );
}

/// Postgres `CreateTable` for a model with a ForeignKey column should
/// include `REFERENCES "fk_user"("id")` in the rendered DDL.
#[test]
fn create_table_emits_references_postgres() {
    let op = Operation::CreateTable {
        table: "fk_post".to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
            Column {
                name: "title".to_string(),
                ty: SqlType::Text,
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
            Column {
                name: "author".to_string(),
                ty: SqlType::ForeignKey,
                primary_key: false,
                nullable: false,
                fk_target: Some("fk_user".to_string()),
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
            },
        ],
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1, "CreateTable should emit one statement");
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("references"),
        "Postgres DDL should contain REFERENCES; got: {sql}"
    );
    assert!(
        lower.contains("\"fk_user\""),
        "Postgres DDL should reference `fk_user`; got: {sql}"
    );
    assert!(
        lower.contains("(\"id\")"),
        "Postgres DDL should reference column `id`; got: {sql}"
    );
    assert!(
        lower.contains("bigint"),
        "FK column should be BIGINT on Postgres; got: {sql}"
    );
}

// =========================================================================
// Live SQLite round-trip
// =========================================================================

async fn fresh_pool() -> SqlitePool {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite should always connect");

    // User table — no FK, plain PK.
    sqlx::query(
        "CREATE TABLE fk_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE fk_user");

    // Post table — FK to fk_user.
    sqlx::query(
        "CREATE TABLE fk_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author INTEGER NOT NULL REFERENCES fk_user(id)
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE fk_post");

    pool
}

/// Insert a User row and a Post row; verify that `post.author.resolve(&pool)`
/// returns the correct User.
#[tokio::test]
async fn resolve_returns_referenced_user() {
    let pool = fresh_pool().await;

    // Insert a user.
    let user: User = sqlx::query_as::<sqlx::Sqlite, User>(
        "INSERT INTO fk_user (name) VALUES ('Alice') RETURNING id, name",
    )
    .fetch_one(&pool)
    .await
    .expect("insert user");

    // Insert a post referencing the user.
    let post: Post = sqlx::query_as::<sqlx::Sqlite, Post>(
        "INSERT INTO fk_post (title, author) VALUES ('Hello', ?) RETURNING id, title, author",
    )
    .bind(user.id)
    .fetch_one(&pool)
    .await
    .expect("insert post");

    assert_eq!(
        post.author.id(),
        user.id,
        "post.author.id() should equal user.id"
    );

    // Resolve through the ForeignKey.
    let resolved = post
        .author
        .resolve(&pool)
        .await
        .expect("resolve should succeed");

    assert_eq!(resolved.id, user.id, "resolved.id should match");
    assert_eq!(resolved.name, "Alice", "resolved.name should match");
}

/// `ForeignKey::new(pk)` and `ForeignKey::id()` round-trip correctly.
/// The post-generic-FK API replaces the `From<i64>` shorthand with
/// the explicit `new(...)` constructor so non-i64 PK types (String,
/// UUID) work the same way.
#[test]
fn foreign_key_from_and_id_roundtrip() {
    let fk: ForeignKey<User> = ForeignKey::new(42i64);
    assert_eq!(fk.id(), 42);
}

/// `ForeignKey::set()` updates the stored value.
#[test]
fn foreign_key_set_updates_value() {
    let mut fk: ForeignKey<User> = ForeignKey::new(1i64);
    fk.set(99);
    assert_eq!(fk.id(), 99);
}

/// `ForeignKey<T>` serialises as a plain JSON integer for an
/// i64-keyed target. (String-keyed targets serialise as a JSON
/// string; UUID-keyed as a UUID-shaped string.)
#[test]
fn foreign_key_serialises_as_integer() {
    let fk: ForeignKey<User> = ForeignKey::new(7i64);
    let json = serde_json::to_string(&fk).unwrap();
    assert_eq!(json, "7", "ForeignKey should serialise as a bare integer");
}

/// `ForeignKey<T>` deserialises from a plain JSON integer.
#[test]
fn foreign_key_deserialises_from_integer() {
    let fk: ForeignKey<User> = serde_json::from_str("42").unwrap();
    assert_eq!(fk.id(), 42);
}
