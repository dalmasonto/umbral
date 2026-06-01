//! Coverage for Phase 2 of the Postgres rollout: backend-aware DDL
//! rendering. Verifies that `migrate::render_operation_for(op, "postgres")`
//! emits the right Postgres-dialect SQL without needing a live PG
//! server.
//!
//! The dispatch function is the public seam: `render_operation` (the
//! ambient version that reads `crate::backend::active()`) is the
//! production entry point, but `render_operation_for` lets us pin the
//! dialect explicitly. The two share the same per-backend helpers so
//! pinning the explicit form is the contract.
//!
//! No `App::build()` boot, no `OnceLock` writes, no live database —
//! these tests are pure functions over `Operation` values.

use umbra::migrate::{Column, Operation, render_operation_for};
use umbra::orm::SqlType;

/// One column descriptor — a `BigInt` primary key, the
/// `id: i64` shape every umbra model carries.
fn id_pk() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        fk_target: None,
    }
}

/// A non-nullable text column with the given name.
fn text_not_null(name: &str) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
    }
}

/// A nullable text column with the given name.
fn text_nullable(name: &str) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: true,
        fk_target: None,
    }
}

// --------------------------------------------------------------------- //
// CreateTable                                                            //
// --------------------------------------------------------------------- //

/// Postgres `CreateTable` with a BigInt PK should render `bigserial`
/// (sea-query lowers `BigInteger + auto_increment` to `bigserial` on
/// Postgres). The SQLite quirk (forcing `INTEGER` and attaching
/// `AUTOINCREMENT`) does NOT apply — Postgres has native identity
/// columns and respects the declared width.
#[test]
fn create_table_bigint_pk_renders_bigserial_on_postgres() {
    let op = Operation::CreateTable {
        table: "post".to_string(),
        columns: vec![id_pk(), text_not_null("title")],
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(
        stmts.len(),
        1,
        "CreateTable should render to one statement; got {stmts:?}"
    );
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    // The Postgres-shaped identity column for an i64 PK is bigserial.
    // The integer-PRIMARY-KEY-AUTOINCREMENT trick is SQLite-only.
    assert!(
        lower.contains("bigserial"),
        "expected `bigserial` for BigInt PK on postgres; got {sql}",
    );
    assert!(
        !lower.contains("autoincrement"),
        "AUTOINCREMENT is a SQLite-only quirk; got {sql}",
    );

    // Postgres-builder identifiers are double-quoted (vs SQLite's
    // backticks-or-double-quotes choice).
    assert!(
        sql.contains("\"post\""),
        "table identifier should be double-quoted on postgres; got {sql}",
    );
    assert!(
        sql.contains("\"id\""),
        "column identifier should be double-quoted on postgres; got {sql}",
    );
    assert!(
        sql.contains("\"title\""),
        "title column should be double-quoted on postgres; got {sql}",
    );
}

/// The SQLite path keeps its INTEGER-PRIMARY-KEY-AUTOINCREMENT quirk
/// even for a BigInt PK. Pinning the contrast so the Postgres change
/// doesn't quietly regress SQLite behaviour.
#[test]
fn create_table_bigint_pk_renders_integer_autoincrement_on_sqlite() {
    let op = Operation::CreateTable {
        table: "post".to_string(),
        columns: vec![id_pk(), text_not_null("title")],
    };

    let stmts = render_operation_for(&op, "sqlite");
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();

    assert!(
        lower.contains("autoincrement"),
        "SQLite path keeps the INTEGER PK + AUTOINCREMENT quirk; got {sql}",
    );
    assert!(
        !lower.contains("bigserial"),
        "BIGSERIAL is Postgres-only; got {sql}",
    );
}

// --------------------------------------------------------------------- //
// AlterColumn                                                            //
// --------------------------------------------------------------------- //

/// Flipping a column to nullable on Postgres should emit one native
/// `ALTER TABLE ... ALTER COLUMN ... DROP NOT NULL`, NOT the SQLite
/// four-step table-recreation dance.
#[test]
fn alter_column_to_nullable_uses_native_alter_on_postgres() {
    let op = Operation::AlterColumn {
        table: "post".to_string(),
        column: "title".to_string(),
        new_columns: vec![id_pk(), text_nullable("title")],
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(
        stmts.len(),
        1,
        "native ALTER is one statement, not the four-step SQLite dance; got {stmts:?}",
    );
    let sql = &stmts[0];
    let upper = sql.to_ascii_uppercase();
    assert!(
        upper.contains("ALTER TABLE"),
        "expected ALTER TABLE on postgres; got {sql}",
    );
    assert!(
        upper.contains("DROP NOT NULL"),
        "nullable=true should emit DROP NOT NULL; got {sql}",
    );
    assert!(
        sql.contains("\"post\""),
        "table identifier double-quoted; got {sql}",
    );
    assert!(
        sql.contains("\"title\""),
        "column identifier double-quoted; got {sql}",
    );
}

/// Flipping a column to non-nullable on Postgres emits `SET NOT NULL`.
#[test]
fn alter_column_to_not_null_uses_set_not_null_on_postgres() {
    let op = Operation::AlterColumn {
        table: "post".to_string(),
        column: "title".to_string(),
        new_columns: vec![id_pk(), text_not_null("title")],
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    let upper = sql.to_ascii_uppercase();
    assert!(
        upper.contains("SET NOT NULL"),
        "nullable=false should emit SET NOT NULL; got {sql}",
    );
    assert!(
        !upper.contains("DROP NOT NULL"),
        "should not emit DROP NOT NULL when flipping to non-null; got {sql}",
    );
}

/// The SQLite path keeps the four-step table-recreation dance for
/// nullable flips. Pinning the contrast so the Postgres change
/// doesn't quietly regress SQLite behaviour.
#[test]
fn alter_column_keeps_recreation_dance_on_sqlite() {
    let op = Operation::AlterColumn {
        table: "post".to_string(),
        column: "title".to_string(),
        new_columns: vec![id_pk(), text_nullable("title")],
    };

    let stmts = render_operation_for(&op, "sqlite");
    assert_eq!(
        stmts.len(),
        4,
        "SQLite dance is CREATE + INSERT...SELECT + DROP + RENAME; got {stmts:?}",
    );
    let upper_join = stmts.join("\n").to_ascii_uppercase();
    assert!(upper_join.contains("CREATE TABLE"));
    assert!(upper_join.contains("INSERT INTO"));
    assert!(upper_join.contains("DROP TABLE"));
    assert!(upper_join.contains("RENAME"));
}

// --------------------------------------------------------------------- //
// DropTable / AddColumn / DropColumn                                     //
// --------------------------------------------------------------------- //

/// DropTable renders identical-shape SQL on both backends; the only
/// observable difference is identifier quoting. The Postgres builder
/// uses double quotes consistently.
#[test]
fn drop_table_on_postgres_double_quotes_identifier() {
    let op = Operation::DropTable {
        table: "post".to_string(),
    };

    let stmts = render_operation_for(&op, "postgres");
    let sql = &stmts[0];
    let upper = sql.to_ascii_uppercase();
    assert!(upper.contains("DROP TABLE"));
    assert!(sql.contains("\"post\""), "got {sql}");
}

/// AddColumn on Postgres emits ALTER TABLE ADD COLUMN with the
/// Postgres native type mapping (`text` for SqlType::Text).
#[test]
fn add_column_on_postgres_uses_pg_type_mapping() {
    let op = Operation::AddColumn {
        table: "post".to_string(),
        column: text_not_null("body"),
    };

    let stmts = render_operation_for(&op, "postgres");
    let sql = &stmts[0];
    let upper = sql.to_ascii_uppercase();
    assert!(upper.contains("ALTER TABLE"));
    assert!(upper.contains("ADD COLUMN"));
    // The Postgres mapping for SqlType::Text is `text`.
    assert!(
        sql.to_ascii_lowercase().contains("text"),
        "Postgres Text should render as `text`; got {sql}",
    );
    assert!(sql.contains("\"body\""), "got {sql}");
}

/// DropColumn on Postgres emits ALTER TABLE DROP COLUMN with a
/// double-quoted identifier.
#[test]
fn drop_column_on_postgres_double_quotes_identifier() {
    let op = Operation::DropColumn {
        table: "post".to_string(),
        column: "body".to_string(),
    };

    let stmts = render_operation_for(&op, "postgres");
    let sql = &stmts[0];
    let upper = sql.to_ascii_uppercase();
    assert!(upper.contains("ALTER TABLE"));
    assert!(upper.contains("DROP COLUMN"));
    assert!(sql.contains("\"body\""), "got {sql}");
}

// --------------------------------------------------------------------- //
// Dispatch guards                                                        //
// --------------------------------------------------------------------- //

/// `render_operation_for` panics on an unknown backend name with a
/// clear hint about the two shipped dialects. Phase 2 explicitly only
/// covers sqlite + postgres.
#[test]
#[should_panic(expected = "no DDL renderer for backend `mysql`")]
fn render_operation_for_unknown_backend_panics() {
    let op = Operation::DropTable {
        table: "x".to_string(),
    };
    let _ = render_operation_for(&op, "mysql");
}
