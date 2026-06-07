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
        slug_from: ::core::option::Option::None,
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
        slug_from: ::core::option::Option::None,
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
        slug_from: ::core::option::Option::None,
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
        unique_together: Vec::new(),
        indexes: Vec::new(),
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
        unique_together: Vec::new(),
        indexes: Vec::new(),
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
        prev_columns: None,
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
        prev_columns: None,
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
        prev_columns: None,
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
// Safe-cast support (gap #64)                                            //
// --------------------------------------------------------------------- //

/// Helper: build a non-nullable column of a given type.
fn col(name: &str, ty: SqlType) -> Column {
    Column {
        name: name.to_string(),
        ty,
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
        slug_from: ::core::option::Option::None,
    }
}

/// BigInt → Text on Postgres emits an `ALTER COLUMN ... TYPE TEXT
/// USING <col>::text`. This is the canonical case from gap #64
/// (Session.user_id flipping to polymorphic Text storage).
#[test]
fn safe_cast_bigint_to_text_emits_using_on_postgres() {
    let op = Operation::AlterColumn {
        table: "session".to_string(),
        column: "user_id".to_string(),
        new_columns: vec![id_pk(), col("user_id", SqlType::Text)],
        prev_columns: Some(vec![id_pk(), col("user_id", SqlType::BigInt)]),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(
        stmts.len(),
        1,
        "type-only change should emit one ALTER (no nullable flip); got {stmts:?}",
    );
    let sql = &stmts[0];
    assert!(sql.contains("TYPE text"), "expected TYPE text; got {sql}");
    assert!(
        sql.contains("USING \"user_id\"::text"),
        "expected USING <col>::text cast; got {sql}",
    );
    assert!(
        sql.contains("\"session\""),
        "table identifier double-quoted; got {sql}",
    );
}

/// Integer widening also flows through the safe-cast path.
#[test]
fn safe_cast_smallint_to_integer_emits_using_on_postgres() {
    let op = Operation::AlterColumn {
        table: "thing".to_string(),
        column: "count".to_string(),
        new_columns: vec![id_pk(), col("count", SqlType::Integer)],
        prev_columns: Some(vec![id_pk(), col("count", SqlType::SmallInt)]),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    assert!(
        sql.contains("TYPE integer"),
        "expected TYPE integer; got {sql}",
    );
    assert!(
        sql.contains("USING \"count\"::integer"),
        "expected USING cast; got {sql}",
    );
}

/// A combined type + nullable flip emits BOTH statements in order
/// (TYPE first so the NOT NULL flip evaluates against the new type).
#[test]
fn safe_cast_with_nullable_flip_emits_two_statements_in_order() {
    let mut prev = col("user_id", SqlType::BigInt);
    prev.nullable = false;
    let mut next = col("user_id", SqlType::Text);
    next.nullable = true;

    let op = Operation::AlterColumn {
        table: "session".to_string(),
        column: "user_id".to_string(),
        new_columns: vec![id_pk(), next],
        prev_columns: Some(vec![id_pk(), prev]),
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 2, "expected TYPE + nullable; got {stmts:?}");
    assert!(stmts[0].contains("TYPE text"), "TYPE first; got {stmts:?}");
    assert!(
        stmts[1].contains("DROP NOT NULL"),
        "nullable change second; got {stmts:?}",
    );
}

/// Without a previous snapshot the renderer falls back to the legacy
/// nullable-only path (the migration was produced before the safe-cast
/// machinery shipped, so we can't tell what changed and emit just the
/// nullable flip).
#[test]
fn missing_prev_columns_keeps_legacy_nullable_only_behaviour() {
    let op = Operation::AlterColumn {
        table: "post".to_string(),
        column: "title".to_string(),
        new_columns: vec![id_pk(), text_nullable("title")],
        prev_columns: None,
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    assert!(stmts[0].contains("DROP NOT NULL"));
    assert!(
        !stmts[0].contains("TYPE"),
        "no prev means no TYPE inference; got {stmts:?}",
    );
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

// --------------------------------------------------------------------- //
// IMP-3: `#[umbra(min = N)]` / `#[umbra(max = N)]` CHECK constraints      //
// --------------------------------------------------------------------- //

/// An integer column with min/max bounds renders a CHECK clause that
/// quotes the column name and combines both bounds with AND.
#[test]
fn create_table_int_with_min_max_emits_check_on_postgres() {
    let mut age = Column {
        name: "age".to_string(),
        ty: SqlType::Integer,
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
        min: Some(0),
        max: Some(150),
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    };
    let op = Operation::CreateTable {
        table: "person".to_string(),
        columns: vec![id_pk(), age.clone()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let stmts = render_operation_for(&op, "postgres");
    let sql = &stmts[0];
    assert!(
        sql.contains("CHECK (\"age\" >= 0 AND \"age\" <= 150)"),
        "expected combined min+max CHECK; got {sql}",
    );

    // SQLite emits the same CHECK; both dialects accept the syntax.
    let op2 = Operation::CreateTable {
        table: "person".to_string(),
        columns: vec![id_pk(), age.clone()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let sqlite_sql = &render_operation_for(&op2, "sqlite")[0];
    assert!(
        sqlite_sql.contains("CHECK (\"age\" >= 0 AND \"age\" <= 150)"),
        "expected the same CHECK on SQLite; got {sqlite_sql}",
    );

    // Min-only is just `>=`.
    age.max = None;
    let op3 = Operation::CreateTable {
        table: "person".to_string(),
        columns: vec![id_pk(), age.clone()],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let pg_min_only = &render_operation_for(&op3, "postgres")[0];
    assert!(
        pg_min_only.contains("CHECK (\"age\" >= 0)") && !pg_min_only.contains("<="),
        "min-only should drop the upper bound; got {pg_min_only}",
    );
}

// --------------------------------------------------------------------- //
// BUG-6/7: `unique_together` + `indexes` struct-level attributes          //
// --------------------------------------------------------------------- //

/// A `CreateTable` carrying a `unique_together` group emits an inline
/// `UNIQUE (col, col)` clause on both backends. Two groups → two
/// clauses.
#[test]
fn create_table_emits_unique_together_clauses() {
    let op = Operation::CreateTable {
        table: "post".to_string(),
        columns: vec![id_pk(), text_not_null("tenant_id"), text_not_null("slug")],
        unique_together: vec![vec!["tenant_id".to_string(), "slug".to_string()]],
        indexes: Vec::new(),
    };
    let sql_pg = &render_operation_for(&op, "postgres")[0];
    let sql_lite = &render_operation_for(
        &Operation::CreateTable {
            table: "post".to_string(),
            columns: vec![id_pk(), text_not_null("tenant_id"), text_not_null("slug")],
            unique_together: vec![vec!["tenant_id".to_string(), "slug".to_string()]],
            indexes: Vec::new(),
        },
        "sqlite",
    )[0]
    .clone();
    assert!(
        sql_pg.to_ascii_uppercase().contains("UNIQUE")
            && sql_pg.contains("\"tenant_id\"")
            && sql_pg.contains("\"slug\""),
        "expected composite UNIQUE on postgres; got {sql_pg}",
    );
    assert!(
        sql_lite.to_ascii_uppercase().contains("UNIQUE")
            && sql_lite.contains("tenant_id")
            && sql_lite.contains("slug"),
        "expected composite UNIQUE on sqlite; got {sql_lite}",
    );
}

/// A `CreateTable` with an `indexes` group emits a follow-up
/// `CREATE INDEX IF NOT EXISTS` statement after the table, with a
/// deterministic `idx_<table>_<col1>_<col2>` name.
#[test]
fn create_table_emits_multi_column_index_after_table() {
    let op = Operation::CreateTable {
        table: "post".to_string(),
        columns: vec![
            id_pk(),
            text_not_null("tenant_id"),
            text_not_null("created_at"),
        ],
        unique_together: Vec::new(),
        indexes: vec![vec!["tenant_id".to_string(), "created_at".to_string()]],
    };
    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(
        stmts.len(),
        2,
        "expected CREATE TABLE + CREATE INDEX = 2 stmts; got {stmts:?}",
    );
    let idx = &stmts[1];
    assert!(
        idx.contains("CREATE INDEX IF NOT EXISTS")
            && idx.contains("\"idx_post_tenant_id_created_at\"")
            && idx.contains("\"tenant_id\"")
            && idx.contains("\"created_at\""),
        "expected multi-col index DDL; got {idx}",
    );
}

/// Non-numeric column types skip the CHECK even when bounds are set.
/// Min/max on a TEXT column is nonsensical (lexicographic comparison)
/// so the renderer treats them as a no-op rather than a footgun.
#[test]
fn min_max_skipped_for_non_numeric_columns() {
    let mut title = text_not_null("title");
    title.min = Some(1);
    title.max = Some(100);
    let op = Operation::CreateTable {
        table: "post".to_string(),
        columns: vec![id_pk(), title],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let sql = &render_operation_for(&op, "postgres")[0];
    assert!(
        !sql.contains("CHECK"),
        "min/max on TEXT must not emit a CHECK clause; got {sql}",
    );
}
