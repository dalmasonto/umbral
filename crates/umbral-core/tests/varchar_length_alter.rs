//! `max_length` is schema-meaningful on Postgres — a `Text` column with a
//! bound renders as `character varying(n)` — but both the differ and the
//! `AlterColumn` renderer treated it as UI-only. Raising a model's
//! `max_length` therefore never emitted an ALTER, models drifted from DDL,
//! and every FRESH database enforced the stale bound. Found live loading
//! TaskFlow's dev dump into a new Postgres: four columns (message bodies,
//! prompt answers, activity metadata) rejected rows the app had been writing
//! for weeks.
//!
//! Policy pinned here:
//!   * WIDENING (n → bigger n, or n → unbounded) is a lossless in-place
//!     alter: the differ emits `AlterColumn`, the Postgres renderer emits
//!     `ALTER TABLE .. TYPE character varying(bigger)`.
//!   * NARROWING (n → smaller n, or unbounded → any bound) can abort on
//!     existing rows mid-migration, so the differ refuses with `UnsafeAlter`
//!     and demands a hand-written data-preserving migration.
//!   * SQLite needs no renderer change: its `AlterColumn` table-recreation
//!     dance rebuilds from `new_columns`, and varchar bounds are advisory
//!     there anyway.
//!
//! These drive the real `diff` and `render_operation_for` — nothing mocks the
//! migration engine.

use umbral::migrate::{Column, ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbral::orm::SqlType;

fn text_col(name: &str, max_length: u32) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        max_length,
        ..Column::default()
    }
}

fn model(fields: Vec<Column>) -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: "Note".to_string(),
        table: "note".to_string(),
        app_label: "app".to_string(),
        fields,
        ..ModelMeta::default()
    }
}

fn snapshot(fields: Vec<Column>) -> Snapshot {
    Snapshot {
        models: vec![model(fields)],
    }
}

fn alter_op(prev_len: u32, new_len: u32) -> Operation {
    Operation::AlterColumn {
        table: "note".to_string(),
        column: "body".to_string(),
        new_columns: vec![text_col("body", new_len)],
        prev_columns: Some(vec![text_col("body", prev_len)]),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The differ
// ---------------------------------------------------------------------------

/// Raising a bound is lossless; the differ must emit the AlterColumn it
/// silently swallowed before.
#[test]
fn raising_max_length_emits_an_alter_column() {
    let previous = snapshot(vec![text_col("body", 8000)]);
    let current = snapshot(vec![text_col("body", 40000)]);

    let ops = diff(&previous, &current).expect("diff");
    assert!(
        ops.iter()
            .any(|o| matches!(o, Operation::AlterColumn { column, .. } if column == "body")),
        "widening 8000 -> 40000 must emit AlterColumn; got: {ops:#?}",
    );
}

/// Dropping the bound entirely (varchar(n) -> text) only ever gains room.
#[test]
fn unbounding_emits_an_alter_column() {
    let previous = snapshot(vec![text_col("body", 8000)]);
    let current = snapshot(vec![text_col("body", 0)]);

    let ops = diff(&previous, &current).expect("diff");
    assert!(
        ops.iter()
            .any(|o| matches!(o, Operation::AlterColumn { column, .. } if column == "body")),
        "unbounding varchar(8000) -> text must emit AlterColumn; got: {ops:#?}",
    );
}

/// Narrowing can abort the migration on existing rows — refuse it.
#[test]
fn narrowing_max_length_is_an_unsafe_alter() {
    let previous = snapshot(vec![text_col("body", 8000)]);
    let current = snapshot(vec![text_col("body", 200)]);

    let err = diff(&previous, &current).expect_err("narrowing must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("max_length") || msg.contains("narrow"),
        "the refusal should name the length change; got: {msg}",
    );
}

/// unbounded -> bounded is also a narrowing (any oversized row aborts it).
#[test]
fn bounding_unbounded_text_is_an_unsafe_alter() {
    let previous = snapshot(vec![text_col("body", 0)]);
    let current = snapshot(vec![text_col("body", 8000)]);

    diff(&previous, &current).expect_err("text -> varchar(8000) must refuse");
}

/// An unchanged bound must NOT start emitting alters (no churn).
#[test]
fn equal_max_length_emits_nothing() {
    let previous = snapshot(vec![text_col("body", 8000)]);
    let current = snapshot(vec![text_col("body", 8000)]);

    let ops = diff(&previous, &current).expect("diff");
    assert!(ops.is_empty(), "no schema change, no ops; got: {ops:#?}",);
}

// ---------------------------------------------------------------------------
// The Postgres renderer
// ---------------------------------------------------------------------------

/// A widening AlterColumn must render the in-place TYPE change.
#[test]
fn widening_renders_a_postgres_type_change() {
    let stmts = render_operation_for(&alter_op(8000, 40000), "postgres");
    assert!(
        stmts.iter().any(|s| s
            .contains(r#"ALTER TABLE "note" ALTER COLUMN "body" TYPE character varying(40000)"#)),
        "expected a varchar(40000) TYPE alter; got: {stmts:#?}",
    );
}

/// Widening to unbounded renders `TYPE text`.
#[test]
fn unbounding_renders_postgres_text() {
    let stmts = render_operation_for(&alter_op(8000, 0), "postgres");
    assert!(
        stmts
            .iter()
            .any(|s| s.contains(r#"ALTER COLUMN "body" TYPE text"#)),
        "expected a TYPE text alter; got: {stmts:#?}",
    );
}

/// A same-length AlterColumn (e.g. a nullable flip) must not grow a TYPE
/// statement it never had.
#[test]
fn same_length_alter_renders_no_type_change() {
    let mut new_col = text_col("body", 8000);
    new_col.nullable = true;
    let op = Operation::AlterColumn {
        table: "note".to_string(),
        column: "body".to_string(),
        new_columns: vec![new_col],
        prev_columns: Some(vec![text_col("body", 8000)]),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let stmts = render_operation_for(&op, "postgres");
    assert!(
        !stmts.iter().any(|s| s.contains(" TYPE ")),
        "no length change, no TYPE statement; got: {stmts:#?}",
    );
}
