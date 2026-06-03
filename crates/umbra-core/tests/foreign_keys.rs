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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
            },
        ],
        unique_together: Vec::new(),
        indexes: Vec::new(),
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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
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
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
            },
        ],
        unique_together: Vec::new(),
        indexes: Vec::new(),
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

// =========================================================================
// Gap #69: self-referential foreign keys.
//
// Django uses a string sentinel (`models.ForeignKey('self', ...)`)
// because the model class isn't bound yet when the field is declared.
// Rust solves the same problem differently: `ForeignKey<T>` stores
// `T::PrimaryKey` and `Option<Box<T>>` (boxed so the type stays
// finite-size), and `<T as Model>::TABLE` resolves at the same
// expansion step that emits `impl Model for Self`. Net result: writing
// `ForeignKey<MyType>` *inside* `MyType` Just Works — no `Self`
// keyword needed, no string sentinel, no macro special-case.
// =========================================================================

/// A category tree: each row optionally points at its parent, with
/// `ON DELETE CASCADE` so deleting a root prunes the subtree. The
/// `parent_id` column's `fk_target` resolves to the *same table* the
/// model lives in.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fk_category")]
pub struct Category {
    pub id: i64,
    pub name: String,
    #[umbra(on_delete = "cascade")]
    pub parent_id: Option<ForeignKey<Category>>,
}

/// Static metadata: the self-FK column carries `fk_target =
/// Some("fk_category")` — the model's own table name. The derive
/// emits `<Category as Model>::TABLE` for the target lookup, which
/// resolves to the const that the same derive expansion is producing
/// alongside the field metadata. The const resolution happens before
/// the const's value is needed, which is fine because const-eval
/// runs after the expansion is complete.
#[test]
fn self_referential_fk_target_resolves_to_own_table() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <Category as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();
    let parent = by_name.get("parent_id").expect("parent_id field present");
    assert_eq!(parent.ty, SqlType::ForeignKey);
    assert_eq!(parent.fk_target, Some("fk_category"));
    assert!(parent.nullable, "Option<ForeignKey<T>> should be nullable");
    assert!(
        matches!(parent.on_delete, umbra::orm::FkAction::Cascade),
        "ON DELETE CASCADE should round-trip from the attribute"
    );
}

/// The DDL renders `REFERENCES "fk_category"("id") ON DELETE CASCADE`
/// for the parent column — the table is self-referencing within a
/// single CREATE TABLE statement. Both SQLite and Postgres accept
/// that shape because the column constraint is evaluated against
/// the table being created in the same statement.
#[test]
fn self_referential_fk_renders_inline_references() {
    let meta = umbra::migrate::ModelMeta::for_::<Category>();
    let op = Operation::CreateTable {
        table: Category::TABLE.to_string(),
        columns: meta.fields.clone(),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    for backend in ["sqlite", "postgres"] {
        let sql = render_operation_for(&op, backend).join("\n");
        assert!(
            sql.contains("REFERENCES \"fk_category\"(\"id\")"),
            "{backend}: expected self-reference in REFERENCES tail; got: {sql}"
        );
        assert!(
            sql.to_uppercase().contains("ON DELETE CASCADE"),
            "{backend}: expected ON DELETE CASCADE; got: {sql}"
        );
    }
}

/// End-to-end against a live SQLite engine: create the table, insert
/// a root + a child, assert the FK link, then delete the root and
/// confirm the cascade pruned the child. The same `Operation::
/// CreateTable` the migration engine emits is the only DDL run —
/// nothing is hand-stitched.
#[tokio::test]
async fn self_referential_fk_round_trips_through_sqlite_with_cascade() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON;")
        .execute(&pool)
        .await
        .unwrap();
    let meta = umbra::migrate::ModelMeta::for_::<Category>();
    let op = Operation::CreateTable {
        table: Category::TABLE.to_string(),
        columns: meta.fields.clone(),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    for stmt in render_operation_for(&op, "sqlite") {
        sqlx::query(&stmt).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO fk_category (id, name, parent_id) VALUES (1, 'root', NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO fk_category (id, name, parent_id) VALUES (2, 'child', 1)")
        .execute(&pool)
        .await
        .unwrap();
    let (children,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM fk_category WHERE parent_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(children, 1, "child should reference the root");

    sqlx::query("DELETE FROM fk_category WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let (remaining,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM fk_category")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "CASCADE should have pruned the child after the root was deleted"
    );
}

// =========================================================================
// BUG-15 from bugs/tests/testBugs.md: a OneToOne relationship is
// expressible today as `#[umbra(unique)] ForeignKey<T>`. The
// emitted DDL combines UNIQUE + REFERENCES; the second INSERT
// pointing at the same target row fails the UNIQUE constraint.
// No dedicated `OneToOne<T>` type is needed at v1; this test
// pins the pattern so future macro refactors don't silently
// drop the combination.
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fk_profile")]
pub struct Profile {
    pub id: i64,
    /// One profile per user. The `#[umbra(unique)]` flag is what
    /// makes this a 1:1 rather than a 1:N relationship.
    #[umbra(unique, on_delete = "cascade")]
    pub user_id: ForeignKey<User>,
    pub bio: String,
}

#[test]
fn one_to_one_pattern_emits_unique_and_references() {
    let meta = umbra::migrate::ModelMeta::for_::<Profile>();
    let user_id = meta
        .fields
        .iter()
        .find(|c| c.name == "user_id")
        .expect("user_id present");
    assert_eq!(user_id.ty, SqlType::ForeignKey);
    assert_eq!(user_id.fk_target.as_deref(), Some("fk_user"));
    assert!(
        user_id.unique,
        "the #[umbra(unique)] flag is what makes this a 1:1",
    );

    for backend in ["sqlite", "postgres"] {
        let op = Operation::CreateTable {
            table: Profile::TABLE.to_string(),
            columns: meta.fields.clone(),
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };
        let sql = render_operation_for(&op, backend).join("\n");
        assert!(
            sql.to_uppercase().contains("UNIQUE"),
            "{backend}: should emit UNIQUE for the 1:1 column; got: {sql}",
        );
        assert!(
            sql.contains("REFERENCES \"fk_user\"(\"id\")"),
            "{backend}: should still emit the FK REFERENCES clause; got: {sql}",
        );
    }
}

#[tokio::test]
async fn one_to_one_rejects_second_reference_to_same_target() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON;")
        .execute(&pool)
        .await
        .unwrap();
    // Create the user side by hand for the test (test fixture only).
    sqlx::query("CREATE TABLE fk_user (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    let meta = umbra::migrate::ModelMeta::for_::<Profile>();
    let op = Operation::CreateTable {
        table: Profile::TABLE.to_string(),
        columns: meta.fields.clone(),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    for stmt in render_operation_for(&op, "sqlite") {
        sqlx::query(&stmt).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO fk_user (id, name) VALUES (1, 'alice')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO fk_profile (id, user_id, bio) VALUES (1, 1, 'first')")
        .execute(&pool)
        .await
        .unwrap();
    let dupe = sqlx::query("INSERT INTO fk_profile (id, user_id, bio) VALUES (2, 1, 'second')")
        .execute(&pool)
        .await;
    assert!(
        dupe.is_err(),
        "a second profile pointing at the same user must fail the UNIQUE constraint; succeeded instead",
    );
    let msg = format!("{:?}", dupe.unwrap_err()).to_lowercase();
    assert!(
        msg.contains("unique"),
        "the error should mention the UNIQUE violation; got: {msg}",
    );
}
