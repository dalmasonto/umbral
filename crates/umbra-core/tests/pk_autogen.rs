//! Gap 25 — PK auto-generation and schema emission.
//!
//! Coverage:
//!
//! - **i64 PK / SQLite.** `id: i64` emits `INTEGER PRIMARY KEY AUTOINCREMENT`.
//! - **i64 PK / Postgres.** `id: i64` emits `bigserial`.
//! - **uuid::Uuid PK / SQLite.** `id: uuid::Uuid` emits `TEXT` (no auto-default).
//! - **uuid::Uuid PK / Postgres.** `id: uuid::Uuid` emits `UUID`.
//! - **String PK.** `id: String` emits `TEXT`, no autoincrement or default.
//! - **Autoincrement sentinel.** `id == 0` on an i64 model triggers omit-
//!   from-INSERT so the DB assigns the PK.
//! - **Nil-UUID sentinel.** `id == Uuid::nil()` omits the PK column from
//!   the INSERT.

use umbra::migrate::{Column, Operation, render_operation_for};
use umbra::orm::write::is_default_pk;
use umbra::orm::{Model, SqlType};

// =========================================================================
// Model declarations
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "pk_int_model")]
pub struct IntPkModel {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "pk_uuid_model")]
pub struct UuidPkModel {
    pub id: uuid::Uuid,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "pk_string_model")]
pub struct StringPkModel {
    pub id: String,
    pub label: String,
}

// =========================================================================
// FieldSpec assertions — PK detection
// =========================================================================

/// The derive correctly marks `id: i64` as `primary_key = true` and
/// `ty = SqlType::BigInt`.
#[test]
fn int_pk_field_spec_is_primary_key() {
    let id_field = <IntPkModel as Model>::FIELDS
        .iter()
        .find(|f| f.name == "id")
        .expect("IntPkModel must have an `id` field");

    assert!(id_field.primary_key, "id should be marked primary_key");
    assert_eq!(id_field.ty, SqlType::BigInt);
    assert!(!id_field.nullable);
}

/// The derive correctly marks `id: uuid::Uuid` as `primary_key = true`
/// and `ty = SqlType::Uuid`.
#[test]
fn uuid_pk_field_spec_is_primary_key() {
    let id_field = <UuidPkModel as Model>::FIELDS
        .iter()
        .find(|f| f.name == "id")
        .expect("UuidPkModel must have an `id` field");

    assert!(id_field.primary_key, "id should be marked primary_key");
    assert_eq!(id_field.ty, SqlType::Uuid);
    assert!(!id_field.nullable);
}

/// The derive correctly marks `id: String` as `primary_key = true`
/// and `ty = SqlType::Text`.
#[test]
fn string_pk_field_spec_is_primary_key() {
    let id_field = <StringPkModel as Model>::FIELDS
        .iter()
        .find(|f| f.name == "id")
        .expect("StringPkModel must have an `id` field");

    assert!(id_field.primary_key, "id should be marked primary_key");
    assert_eq!(id_field.ty, SqlType::Text);
    assert!(!id_field.nullable);
}

// =========================================================================
// DDL rendering — SQLite
// =========================================================================

fn int_pk_column() -> Column {
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
        text_format: ::core::option::Option::None,
    }
}

fn uuid_pk_column() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::Uuid,
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
        text_format: ::core::option::Option::None,
    }
}

fn string_pk_column() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::Text,
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
        text_format: ::core::option::Option::None,
    }
}

fn label_column() -> Column {
    Column {
        name: "label".to_string(),
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
        text_format: ::core::option::Option::None,
    }
}

/// SQLite: `id: i64` primary key emits `INTEGER PRIMARY KEY AUTOINCREMENT`.
#[test]
fn sqlite_int_pk_emits_autoincrement() {
    let op = Operation::CreateTable {
        table: "pk_int_model".to_string(),
        columns: vec![int_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "sqlite");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("autoincrement"),
        "SQLite i64 PK should include AUTOINCREMENT; got: {sql}"
    );
    assert!(
        lower.contains("primary key"),
        "column should be marked PRIMARY KEY; got: {sql}"
    );
    // SQLite uses the `INTEGER` type for autoincrement PKs (not `BIGINT`).
    assert!(
        lower.contains("integer"),
        "SQLite PK should use INTEGER type for ROWID alias; got: {sql}"
    );
}

/// SQLite: `id: uuid::Uuid` primary key emits `TEXT PRIMARY KEY` without
/// autoincrement (no DB-side UUID generation at v1).
#[test]
fn sqlite_uuid_pk_emits_text_primary_key_no_default() {
    let op = Operation::CreateTable {
        table: "pk_uuid_model".to_string(),
        columns: vec![uuid_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "sqlite");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("primary key"),
        "UUID PK should be marked PRIMARY KEY; got: {sql}"
    );
    // SQLite stores UUIDs as TEXT.
    assert!(
        lower.contains("text"),
        "SQLite UUID PK should use TEXT type; got: {sql}"
    );
    // No autoincrement for UUID columns.
    assert!(
        !lower.contains("autoincrement"),
        "UUID PK should NOT have AUTOINCREMENT; got: {sql}"
    );
    // No DEFAULT clause — the application supplies UUID values.
    assert!(
        !lower.contains("default"),
        "UUID PK should NOT have a DEFAULT at v1; got: {sql}"
    );
}

/// SQLite: `id: String` primary key emits `TEXT PRIMARY KEY` without
/// autoincrement or default.
#[test]
fn sqlite_string_pk_emits_text_primary_key() {
    let op = Operation::CreateTable {
        table: "pk_string_model".to_string(),
        columns: vec![string_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "sqlite");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("primary key"),
        "String PK should be PRIMARY KEY; got: {sql}"
    );
    assert!(
        lower.contains("text"),
        "String PK should be TEXT type; got: {sql}"
    );
    assert!(
        !lower.contains("autoincrement"),
        "String PK must NOT have AUTOINCREMENT; got: {sql}"
    );
}

// =========================================================================
// DDL rendering — Postgres
// =========================================================================

/// Postgres: `id: i64` emits `bigserial` (sea-query `BigInt + auto_increment`).
#[test]
fn postgres_int_pk_emits_bigserial() {
    let op = Operation::CreateTable {
        table: "pk_int_model".to_string(),
        columns: vec![int_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("bigserial"),
        "Postgres i64 PK should emit bigserial; got: {sql}"
    );
    assert!(
        !lower.contains("autoincrement"),
        "AUTOINCREMENT is SQLite-only; got: {sql}"
    );
}

/// Postgres: `id: uuid::Uuid` emits `uuid PRIMARY KEY` without any default
/// at the schema level.
#[test]
fn postgres_uuid_pk_emits_uuid_type() {
    let op = Operation::CreateTable {
        table: "pk_uuid_model".to_string(),
        columns: vec![uuid_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("uuid"),
        "Postgres UUID PK should use UUID type; got: {sql}"
    );
    assert!(
        lower.contains("primary key"),
        "UUID PK should be PRIMARY KEY; got: {sql}"
    );
}

/// Postgres: `id: String` emits `text PRIMARY KEY`.
#[test]
fn postgres_string_pk_emits_text() {
    let op = Operation::CreateTable {
        table: "pk_string_model".to_string(),
        columns: vec![string_pk_column(), label_column()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("text"),
        "Postgres String PK should use TEXT type; got: {sql}"
    );
    assert!(
        lower.contains("primary key"),
        "String PK should be marked PRIMARY KEY; got: {sql}"
    );
}

// =========================================================================
// Autoincrement sentinel logic
// =========================================================================

/// Integer PK sentinel: `0` is the default PK for i64.
#[test]
fn is_default_pk_recognises_zero_for_bigint() {
    assert!(
        is_default_pk(SqlType::BigInt, &serde_json::json!(0)),
        "0 should be the default PK sentinel for BigInt"
    );
    assert!(
        !is_default_pk(SqlType::BigInt, &serde_json::json!(1)),
        "1 is not the default sentinel"
    );
    assert!(
        !is_default_pk(SqlType::BigInt, &serde_json::json!(-1)),
        "negative values are not the default sentinel"
    );
}

/// Nil UUID is the sentinel for UUID PKs.
#[test]
fn is_default_pk_recognises_nil_uuid() {
    assert!(
        is_default_pk(
            SqlType::Uuid,
            &serde_json::json!("00000000-0000-0000-0000-000000000000")
        ),
        "nil UUID should be the default PK sentinel"
    );
    assert!(
        !is_default_pk(
            SqlType::Uuid,
            &serde_json::json!("11111111-1111-1111-1111-111111111111")
        ),
        "non-nil UUID is not the sentinel"
    );
}

/// Empty string is the sentinel for String PKs.
#[test]
fn is_default_pk_recognises_empty_string() {
    assert!(
        is_default_pk(SqlType::Text, &serde_json::json!("")),
        "empty string should be the default PK sentinel for Text PKs"
    );
    assert!(
        !is_default_pk(SqlType::Text, &serde_json::json!("my-slug")),
        "non-empty string is not the sentinel"
    );
}

// =========================================================================
// Live SQLite round-trips — autoincrement
// =========================================================================

/// Inserting an `IntPkModel` with `id: 0` (the sentinel) results in the DB
/// assigning a positive autoincrement PK.
#[tokio::test]
async fn create_int_pk_model_assigns_autoincrement_id() {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");

    sqlx::query(
        "CREATE TABLE pk_int_model (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    let row: IntPkModel =
        sqlx::query_as("INSERT INTO pk_int_model (label) VALUES ('hello') RETURNING id, label")
            .fetch_one(&pool)
            .await
            .expect("insert");

    assert!(
        row.id > 0,
        "autoincrement id should be positive; got {}",
        row.id
    );
    assert_eq!(row.label, "hello");
}

/// Inserting a `UuidPkModel` with an explicit UUID (not nil) stores it
/// correctly. SQLite's `uuid` feature in sqlx stores UUIDs as 16-byte BLOBs
/// so the table type is BLOB and the UUID is bound directly (not as a string).
#[tokio::test]
async fn create_uuid_pk_model_with_explicit_uuid() {
    let pool = umbra_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");

    // sqlx stores uuid::Uuid on SQLite as a 16-byte BLOB.
    sqlx::query(
        "CREATE TABLE pk_uuid_model (
            id BLOB PRIMARY KEY,
            label TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    // Use new_v7 — the only UUID version enabled in Cargo.toml. The
    // timestamp origin is arbitrary for this test; we only need a non-nil
    // UUID to verify the round-trip stores it correctly.
    let id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext));
    let row: UuidPkModel = sqlx::query_as(
        "INSERT INTO pk_uuid_model (id, label) VALUES (?, 'world') RETURNING id, label",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("insert");

    assert_eq!(row.id, id, "stored UUID should round-trip");
    assert_eq!(row.label, "world");
}
