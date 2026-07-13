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

use umbral_core::migrate::{
    Column, M2MRelation, ModelMeta, Operation, Snapshot, diff, render_operation_for,
};
use umbral_core::orm::SqlType;

// =========================================================================
// Helpers
// =========================================================================

fn make_meta(name: &str, table: &str, cols: Vec<Column>) -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: name.to_string(),
        table: table.to_string(),
        fields: cols,
        display: name.to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        audited: false,
        app_label: "app".to_string(),
    }
}

fn id_col() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    }
}

fn title_col() -> Column {
    Column {
        name: "title".to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
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

/// audit_2 H23 — `Foo (table = "foo")` dropped, `Bar (table = "bar")` created
/// with an identical column shape is genuinely ambiguous (rename vs. two
/// unrelated models). With no `UMBRAL_MIGRATIONS_ASSUME_RENAMES` intent set,
/// `diff` must FAIL CLOSED with `AmbiguousRename` rather than silently emit a
/// `RenameTable` that hands `foo`'s rows to `bar`. The env-driven `assume` /
/// `independent` resolutions are covered in `rename_intent_env.rs` (own binary,
/// since the env is process-global).
///
/// This test must not run with the env set; it asserts the unset default.
#[test]
fn second_pass_shape_match_is_ambiguous_and_errors_by_default() {
    // Guard against a leaked env value from a parallel binary — skip rather
    // than give a false failure (the dedicated binary owns the env cases).
    if std::env::var("UMBRAL_MIGRATIONS_ASSUME_RENAMES").is_ok() {
        return;
    }
    let cols = vec![id_col(), title_col()];
    let prev = make_snapshot(vec![make_meta("Foo", "foo", cols.clone())]);
    let curr = make_snapshot(vec![make_meta("Bar", "bar", cols)]);

    match diff(&prev, &curr) {
        Err(umbral_core::migrate::MigrateError::AmbiguousRename {
            from_table,
            to_table,
        }) => {
            assert_eq!(from_table, "foo");
            assert_eq!(to_table, "bar");
        }
        other => panic!("expected AmbiguousRename error, got {other:?}"),
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
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
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
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
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

// =========================================================================
// Test 7: Renaming a parent model RENAMES its M2M junction (gaps.md #93)
// =========================================================================

/// A model with an `M2M<Tag>` field is renamed (`article` → `post`). The
/// junction, whose name is `<parent_table>_<field>`, must be RENAMED
/// (`article_tags` → `post_tags`) — not dropped and recreated, which would
/// destroy every relationship row. The junction's columns are generic
/// (`parent_id`/`child_id`) and its FK to the parent is auto-updated by the
/// parent's own rename, so a plain table rename is sufficient.
#[test]
fn parent_rename_renames_its_m2m_junction_not_drop_create() {
    let tag = make_meta("Tag", "tag", vec![id_col()]);

    let mut article_prev = make_meta("Article", "article", vec![id_col(), title_col()]);
    article_prev.m2m_relations = vec![M2MRelation {
        field_name: "tags".to_string(),
        target_table: "tag".to_string(),
        target_name: "Tag".to_string(),
    }];
    // Same struct NAME, table renamed article → post (first-pass rename).
    let mut article_curr = article_prev.clone();
    article_curr.table = "post".to_string();

    let prev = make_snapshot(vec![tag.clone(), article_prev]);
    let curr = make_snapshot(vec![tag, article_curr]);

    let ops = diff(&prev, &curr).expect("diff should not error");

    assert!(
        ops.iter()
            .any(|op| matches!(op, Operation::RenameTable { from, to }
            if from == "article" && to == "post")),
        "the parent table must rename article → post; ops: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Operation::RenameTable { from, to }
            if from == "article_tags" && to == "post_tags")),
        "the junction must be RENAMED article_tags → post_tags, not drop+create; ops: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Operation::DropM2MTable { .. })),
        "the junction must NOT be dropped (would destroy the relationship rows); ops: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Operation::CreateM2MTable { .. })),
        "the junction must NOT be recreated; ops: {ops:?}"
    );
}

/// Guard: a genuinely NEW M2M field (no parent rename) still emits
/// CreateM2MTable — the rename detection must not swallow a real create.
#[test]
fn new_m2m_without_parent_rename_still_creates_junction() {
    let tag = make_meta("Tag", "tag", vec![id_col()]);
    let article_prev = make_meta("Article", "article", vec![id_col(), title_col()]);
    let mut article_curr = article_prev.clone();
    article_curr.m2m_relations = vec![M2MRelation {
        field_name: "tags".to_string(),
        target_table: "tag".to_string(),
        target_name: "Tag".to_string(),
    }];

    let prev = make_snapshot(vec![tag.clone(), article_prev]);
    let curr = make_snapshot(vec![tag, article_curr]);

    let ops = diff(&prev, &curr).expect("diff should not error");
    assert!(
        ops.iter().any(
            |op| matches!(op, Operation::CreateM2MTable { junction_table, .. }
            if junction_table == "article_tags")
        ),
        "a new M2M field must create its junction; ops: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Operation::RenameTable { .. })),
        "no table was renamed; ops: {ops:?}"
    );
}

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
