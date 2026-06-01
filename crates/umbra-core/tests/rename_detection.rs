//! Tests for the two-pass rename detection in `migrate::diff` (gap 30).
//!
//! `diff` is exposed as `pub` so tests can drive it directly with
//! hand-built snapshots without going through the process-wide registry.
//!
//! Pass layout:
//!
//! - **First pass (struct-name match):** same `Model::NAME`, different table
//!   → `RenameTable`, zero drop/create.
//! - **Second pass (column-shape match):** different struct names but
//!   bit-identical column shapes → `RenameTable` + warning.
//! - **Name-match wins over shape when both apply:** struct-name match
//!   outranks a coincidental shape match.
//! - **DDL:** `RenameTable` renders to `ALTER TABLE "from" RENAME TO "to"`
//!   on both backends.

use umbra_core::migrate::{Column, ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbra_core::orm::SqlType;

// =========================================================================
// Helpers
// =========================================================================

fn make_meta(name: &str, table: &str, cols: Vec<Column>) -> ModelMeta {
    ModelMeta {
        name: name.to_string(),
        table: table.to_string(),
        fields: cols,
        display: name.to_string(),
        icon: "database".to_string(),
    }
}

fn id_col() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        fk_target: None,
    }
}

fn title_col() -> Column {
    Column {
        name: "title".to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
    }
}

fn make_snapshot(models: Vec<ModelMeta>) -> Snapshot {
    let mut s = Snapshot { models };
    s.models.sort_by(|a, b| a.name.cmp(&b.name));
    s
}

// =========================================================================
// Test 1: First-pass rename — same Model::NAME, table changed
// =========================================================================

/// Snapshot has `Post (table = "post")`, current has `Post (table =
/// "blog_post")`. diff emits exactly one `RenameTable` and zero
/// CreateTable / DropTable operations.
#[test]
fn first_pass_rename_same_struct_name() {
    let prev = make_snapshot(vec![make_meta("Post", "post", vec![id_col(), title_col()])]);
    let curr = make_snapshot(vec![make_meta(
        "Post",
        "blog_post",
        vec![id_col(), title_col()],
    )]);

    let ops = diff(&prev, &curr).expect("diff should not error");

    assert_eq!(
        ops.len(),
        1,
        "expected exactly one operation (RenameTable), got: {ops:?}"
    );
    match &ops[0] {
        Operation::RenameTable { from, to } => {
            assert_eq!(from, "post");
            assert_eq!(to, "blog_post");
        }
        other => panic!("expected RenameTable, got {other:?}"),
    }
}

// =========================================================================
// Test 2: Second-pass rename — different struct names, identical columns
// =========================================================================

/// Snapshot has `Foo (table = "foo")`, current has `Bar (table = "bar")`
/// with identical column shapes. diff emits `RenameTable { from: "foo",
/// to: "bar" }`. The test cannot assert on the eprintln! warning directly;
/// it verifies the operation is produced.
#[test]
fn second_pass_rename_identical_column_shape() {
    let cols = vec![id_col(), title_col()];
    let prev = make_snapshot(vec![make_meta("Foo", "foo", cols.clone())]);
    let curr = make_snapshot(vec![make_meta("Bar", "bar", cols)]);

    let ops = diff(&prev, &curr).expect("diff should not error");

    assert_eq!(
        ops.len(),
        1,
        "expected one RenameTable operation from shape match, got: {ops:?}"
    );
    match &ops[0] {
        Operation::RenameTable { from, to } => {
            assert_eq!(from, "foo");
            assert_eq!(to, "bar");
        }
        other => panic!("expected RenameTable, got {other:?}"),
    }
}

// =========================================================================
// Test 3: Name-match wins when columns also changed
// =========================================================================

/// Snapshot has `Foo (table = "foo")` with `[id, title]`, current has
/// `Foo (table = "foo_v2")` with `[id, title, body]` (different columns).
/// The struct-name match wins: emit `RenameTable` plus the column-level ops
/// for the added column.
#[test]
fn name_match_wins_over_shape_match_when_columns_differ() {
    let body_col = Column {
        name: "body".to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: true,
        fk_target: None,
    };

    let prev = make_snapshot(vec![make_meta("Foo", "foo", vec![id_col(), title_col()])]);
    let curr = make_snapshot(vec![make_meta(
        "Foo",
        "foo_v2",
        vec![id_col(), title_col(), body_col.clone()],
    )]);

    let ops = diff(&prev, &curr).expect("diff should not error");

    // Must contain exactly one RenameTable and one AddColumn.
    let rename_ops: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Operation::RenameTable { .. }))
        .collect();
    let add_ops: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Operation::AddColumn { .. }))
        .collect();
    let create_ops: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Operation::CreateTable { .. }))
        .collect();
    let drop_ops: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Operation::DropTable { .. }))
        .collect();

    assert_eq!(
        rename_ops.len(),
        1,
        "expected one RenameTable; ops: {ops:?}"
    );
    assert_eq!(
        add_ops.len(),
        1,
        "expected one AddColumn for 'body'; ops: {ops:?}"
    );
    assert_eq!(create_ops.len(), 0, "expected no CreateTable; ops: {ops:?}");
    assert_eq!(drop_ops.len(), 0, "expected no DropTable; ops: {ops:?}");

    match rename_ops[0] {
        Operation::RenameTable { from, to } => {
            assert_eq!(from, "foo");
            assert_eq!(to, "foo_v2");
        }
        _ => unreachable!(),
    }
}

// =========================================================================
// Test 4: No rename when shapes differ and names differ
// =========================================================================

/// Snapshot has `Foo (table = "foo")` with `[id, title]`, current has
/// `Bar (table = "bar")` with `[id, description]` (different column names).
/// No rename heuristic fires → plain DropTable + CreateTable.
#[test]
fn no_rename_when_shapes_differ() {
    let desc_col = Column {
        name: "description".to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
    };

    let prev = make_snapshot(vec![make_meta("Foo", "foo", vec![id_col(), title_col()])]);
    let curr = make_snapshot(vec![make_meta("Bar", "bar", vec![id_col(), desc_col])]);

    let ops = diff(&prev, &curr).expect("diff should not error");

    let rename_ops: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Operation::RenameTable { .. }))
        .collect();
    assert_eq!(
        rename_ops.len(),
        0,
        "differing column shapes should not produce a rename; ops: {ops:?}"
    );

    let create_count = ops
        .iter()
        .filter(|op| matches!(op, Operation::CreateTable { .. }))
        .count();
    let drop_count = ops
        .iter()
        .filter(|op| matches!(op, Operation::DropTable { .. }))
        .count();
    assert_eq!(create_count, 1);
    assert_eq!(drop_count, 1);
}

// =========================================================================
// Test 5: DDL rendering for RenameTable on SQLite
// =========================================================================

#[test]
fn rename_table_ddl_sqlite() {
    let op = Operation::RenameTable {
        from: "post".to_string(),
        to: "blog_post".to_string(),
    };
    let sqls = render_operation_for(&op, "sqlite");
    assert_eq!(
        sqls.len(),
        1,
        "RenameTable should render to one SQL statement"
    );
    let sql = &sqls[0];
    // sea-query renders: ALTER TABLE "post" RENAME TO "blog_post"
    assert!(
        sql.contains("RENAME TO"),
        "SQLite RenameTable DDL should contain RENAME TO; got: {sql}"
    );
    assert!(
        sql.contains("\"post\"") || sql.contains("post"),
        "DDL should reference the original table name; got: {sql}"
    );
    assert!(
        sql.contains("\"blog_post\"") || sql.contains("blog_post"),
        "DDL should reference the new table name; got: {sql}"
    );
}

// =========================================================================
// Test 6: DDL rendering for RenameTable on Postgres
// =========================================================================

#[test]
fn rename_table_ddl_postgres() {
    let op = Operation::RenameTable {
        from: "post".to_string(),
        to: "blog_post".to_string(),
    };
    let sqls = render_operation_for(&op, "postgres");
    assert_eq!(
        sqls.len(),
        1,
        "RenameTable should render to one SQL statement"
    );
    let sql = &sqls[0];
    assert!(
        sql.contains("RENAME TO"),
        "Postgres RenameTable DDL should contain RENAME TO; got: {sql}"
    );
    assert!(
        sql.contains("\"post\"") || sql.contains("post"),
        "DDL should reference the original table name; got: {sql}"
    );
    assert!(
        sql.contains("\"blog_post\"") || sql.contains("blog_post"),
        "DDL should reference the new table name; got: {sql}"
    );
}
