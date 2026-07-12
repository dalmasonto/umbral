//! gaps3 #43 — `#[umbral(help = "...")]` reaches the database as a column comment.
//!
//! Help text already flowed to the OpenAPI `description`, the admin form hint,
//! and (since gaps3 #38) the generated TypeScript TSDoc. The one audience that
//! never saw it was the person holding a `psql` prompt — the place people
//! actually go when they're asking "what is this column for?".
//!
//! Postgres has `COMMENT ON COLUMN`. SQLite has no comment facility at all, so
//! the same migration renders to zero statements there; that is an absence of a
//! *display* feature, not a schema divergence — the columns, types, constraints
//! and rows are identical on both backends.
//!
//! (MySQL, which the entry also asks about, is not a backend umbral ships. The
//! renderer panics on any name other than `sqlite` / `postgres`.)
//!
//! These drive the real `render_operation_for` and the real `diff`, so nothing
//! here is a mock of the migration engine.

use umbral::migrate::{Column, ModelMeta, OpSafety, Operation, Snapshot, classify_operation, diff};
use umbral::orm::SqlType;

fn col(name: &str, ty: SqlType, help: &str) -> Column {
    Column {
        name: name.to_string(),
        ty,
        help: help.to_string(),
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

fn create_table(fields: Vec<Column>) -> Operation {
    Operation::CreateTable {
        table: "note".to_string(),
        columns: fields,
        unique_together: Vec::new(),
        indexes: Vec::new(),
    }
}

/// Every statement the operation renders to, on one backend.
fn render(op: &Operation, backend: &str) -> Vec<String> {
    umbral::migrate::render_operation_for(op, backend)
}

fn comments(stmts: &[String]) -> Vec<&String> {
    stmts
        .iter()
        .filter(|s| s.starts_with("COMMENT ON"))
        .collect()
}

// ---------------------------------------------------------------------------
// CREATE TABLE
// ---------------------------------------------------------------------------

/// A column with help text gets a `COMMENT ON COLUMN` after the `CREATE TABLE`.
/// It has to be a separate statement — Postgres has no inline column-comment
/// syntax.
#[test]
fn create_table_emits_a_comment_per_documented_column() {
    let op = create_table(vec![
        col("id", SqlType::BigInt, ""),
        col("body", SqlType::Text, "The note's contents, as Markdown."),
        col(
            "views",
            SqlType::Integer,
            "Read count. Incremented on every GET.",
        ),
    ]);

    let stmts = render(&op, "postgres");
    let found = comments(&stmts);

    assert_eq!(
        found.len(),
        2,
        "one comment per documented column, none for `id`; got: {stmts:#?}",
    );
    assert!(
        stmts.contains(
            &r#"COMMENT ON COLUMN "note"."body" IS 'The note''s contents, as Markdown.'"#
                .to_string()
        ),
        "got: {stmts:#?}",
    );
    assert!(
        stmts.contains(
            &r#"COMMENT ON COLUMN "note"."views" IS 'Read count. Incremented on every GET.'"#
                .to_string()
        ),
        "got: {stmts:#?}",
    );

    // The comments come after the CREATE TABLE — commenting a column that does
    // not exist yet is an error.
    let create_at = stmts
        .iter()
        .position(|s| s.contains("CREATE TABLE"))
        .unwrap();
    let first_comment = stmts
        .iter()
        .position(|s| s.starts_with("COMMENT ON"))
        .unwrap();
    assert!(create_at < first_comment, "got: {stmts:#?}");
}

/// The apostrophe in "note's" above is the whole reason this test exists: help
/// text is prose, prose has apostrophes, and an unescaped one closes the SQL
/// string literal.
#[test]
fn a_quote_in_help_text_is_escaped() {
    let op = create_table(vec![col("body", SqlType::Text, "Don't 'quote' me")]);
    let stmts = render(&op, "postgres");

    assert!(
        stmts.contains(&r#"COMMENT ON COLUMN "note"."body" IS 'Don''t ''quote'' me'"#.to_string()),
        "single quotes must be doubled; got: {stmts:#?}",
    );
}

/// SQLite has no `COMMENT` statement. Rendering must simply omit them, not
/// produce SQL the driver will reject.
#[test]
fn sqlite_renders_no_comments() {
    let op = create_table(vec![col("body", SqlType::Text, "Some help.")]);
    let stmts = render(&op, "sqlite");

    assert!(
        comments(&stmts).is_empty(),
        "SQLite has no COMMENT statement; got: {stmts:#?}",
    );
    assert!(
        stmts.iter().any(|s| s.contains("CREATE TABLE")),
        "the table itself must still be created; got: {stmts:#?}",
    );
}

/// An undocumented column produces no comment at all — not `IS ''`, which would
/// read as "documented, with an empty description".
#[test]
fn an_undocumented_column_gets_no_comment() {
    let op = create_table(vec![col("id", SqlType::BigInt, "")]);
    assert!(comments(&render(&op, "postgres")).is_empty());
}

// ---------------------------------------------------------------------------
// ADD COLUMN
// ---------------------------------------------------------------------------

/// A column added later carries its help text too.
#[test]
fn add_column_emits_its_comment() {
    let op = Operation::AddColumn {
        table: "note".to_string(),
        column: col("slug", SqlType::Text, "URL-safe identifier."),
    };
    let stmts = render(&op, "postgres");

    assert!(
        stmts.contains(&r#"COMMENT ON COLUMN "note"."slug" IS 'URL-safe identifier.'"#.to_string()),
        "got: {stmts:#?}",
    );
    assert!(comments(&render(&op, "sqlite")).is_empty());
}

// ---------------------------------------------------------------------------
// The diff: editing help text is now a schema change.
// ---------------------------------------------------------------------------

/// Before this, `help` was excluded from the diff as "no DB effect". Now it has
/// one, so a copy edit must produce a migration — otherwise the comment in the
/// database silently rots against the comment in the model.
#[test]
fn editing_help_text_emits_a_set_column_comment() {
    let previous = snapshot(vec![col("body", SqlType::Text, "Old description.")]);
    let current = snapshot(vec![col("body", SqlType::Text, "New description.")]);

    let ops = diff(&previous, &current).expect("diff");

    assert_eq!(
        ops,
        vec![Operation::SetColumnComment {
            table: "note".to_string(),
            column: "body".to_string(),
            comment: "New description.".to_string(),
        }],
        "a help-text edit is exactly one comment op — no table rewrite",
    );

    let stmts = render(&ops[0], "postgres");
    assert_eq!(
        stmts,
        vec![r#"COMMENT ON COLUMN "note"."body" IS 'New description.'"#.to_string()],
    );
}

/// Removing help text clears the comment. `IS NULL` is Postgres's "no comment",
/// distinct from `IS ''`.
#[test]
fn removing_help_text_clears_the_comment_with_null() {
    let previous = snapshot(vec![col("body", SqlType::Text, "Was documented.")]);
    let current = snapshot(vec![col("body", SqlType::Text, "")]);

    let ops = diff(&previous, &current).expect("diff");
    assert_eq!(
        ops,
        vec![Operation::SetColumnComment {
            table: "note".to_string(),
            column: "body".to_string(),
            comment: String::new(),
        }],
    );

    assert_eq!(
        render(&ops[0], "postgres"),
        vec![r#"COMMENT ON COLUMN "note"."body" IS NULL"#.to_string()],
    );
    assert!(render(&ops[0], "sqlite").is_empty());
}

/// Unchanged help text produces no migration. Without this, every
/// `makemigrations` after the upgrade would re-emit a comment for every
/// documented column, forever.
#[test]
fn unchanged_help_text_produces_no_operation() {
    let fields = vec![col("body", SqlType::Text, "Stable description.")];
    let ops = diff(&snapshot(fields.clone()), &snapshot(fields)).expect("diff");
    assert!(ops.is_empty(), "no change means no migration; got {ops:#?}");
}

/// A help edit must not drag the column through a rewrite. On SQLite an
/// `AlterColumn` is a full table-recreation dance; on Postgres it rewrites the
/// column. A comment is neither.
#[test]
fn a_help_edit_never_emits_an_alter_column() {
    let previous = snapshot(vec![col("body", SqlType::Text, "Old.")]);
    let current = snapshot(vec![col("body", SqlType::Text, "New.")]);

    let ops = diff(&previous, &current).expect("diff");
    assert!(
        !ops.iter()
            .any(|o| matches!(o, Operation::AlterColumn { .. })),
        "got: {ops:#?}",
    );
}

/// `checkmigrations` gates deploys on operation safety. A comment touches no
/// row and no lock worth naming, so it must classify as SAFE — otherwise a
/// docstring edit would block a zero-downtime deploy.
#[test]
fn setting_a_comment_is_a_safe_operation() {
    let op = Operation::SetColumnComment {
        table: "note".to_string(),
        column: "body".to_string(),
        comment: "Anything.".to_string(),
    };
    assert!(
        matches!(classify_operation(&op), OpSafety::Safe),
        "a column comment is metadata; it must not gate a deploy",
    );
}
