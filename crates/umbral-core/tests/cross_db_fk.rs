//! gaps2 #22 — cross-database foreign-key integrity guard +
//! `#[umbral(db_constraint = false)]` opt-out.
//!
//! umbral routes models to different databases (named pools). A foreign
//! key whose target model lives on a *different* database cannot be a
//! real DB constraint — constraints can't span databases. Two halves of
//! one mechanism:
//!
//! **(a) A boot-time guard** (`App::build()` → `BuildError`) that fails
//! loudly when an FK spans two databases.
//!
//! **(b) The opt-out** `#[umbral(db_constraint = false)]` keeps the FK as
//! a logical relation (joins / `select_related` / app-level FK checks
//! keep working via `fk_target`) but emits NO physical `FOREIGN KEY`
//! DDL. The guard forbids cross-DB FKs unless the field opts out.
//!
//! Because the ambient pool registry is a process-wide `OnceLock` that
//! panics on a second `db::init`, only ONE *successful* `App::build()`
//! may run per test binary. The cross-DB guard test fails in the build's
//! alias-validation phase (before any ambient state is published), so it
//! never collides with the single success build. The DDL-emission tests
//! drive `render_operation_for` directly and touch no global state.

use sqlx::SqlitePool;
use umbral::migrate::{Column, Operation, render_operation_for};
use umbral::orm::{FkAction, Model, SqlType};

// ---------------------------------------------------------------------------
// Models for the boot-guard tests.
//
// `default`-routed parent; an `analytics`-routed child whose FK points
// back at the parent → cross-database FK. One child opts out via
// `db_constraint = false`, one does not.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "xdb_parent")]
pub struct XdbParent {
    pub id: i64,
    pub label: String,
}

/// Child on `analytics` with an FK to a `default`-routed parent and NO
/// opt-out → `App::build()` must reject this.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "xdb_bad_child", database = "analytics")]
pub struct XdbBadChild {
    pub id: i64,
    #[umbral(no_reverse)]
    pub parent: umbral::orm::ForeignKey<XdbParent>,
}

/// Same cross-DB shape, but the FK opts out of the physical constraint
/// → `App::build()` must accept it.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "xdb_ok_child", database = "analytics")]
pub struct XdbOkChild {
    pub id: i64,
    #[umbral(no_reverse, db_constraint = false)]
    pub parent: umbral::orm::ForeignKey<XdbParent>,
}

async fn mem_pool() -> SqlitePool {
    SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool")
}

/// (a) A cross-DB FK with NO opt-out → `App::build()` returns the
/// `CrossDatabaseForeignKey` `BuildError`. This errors in the alias-
/// validation phase, before any ambient state is published, so it is
/// safe to run alongside the single success build in this binary.
#[tokio::test(flavor = "multi_thread")]
async fn cross_db_fk_without_optout_fails_build() {
    use umbral_core::app::BuildError;

    let mut settings = umbral::Settings::from_env().expect("settings load");
    settings.database_url = "sqlite::memory:".to_string();

    let result = umbral::App::builder()
        .settings(settings)
        .database("default", mem_pool().await)
        .database("analytics", mem_pool().await)
        .model::<XdbParent>()
        .model::<XdbBadChild>()
        .build();

    match result {
        Err(BuildError::CrossDatabaseForeignKey {
            model,
            field,
            model_db,
            target_db,
        }) => {
            assert_eq!(model, "XdbBadChild");
            assert_eq!(field, "parent");
            assert_eq!(model_db, "analytics");
            assert_eq!(target_db, "default");
        }
        Err(other) => panic!("expected CrossDatabaseForeignKey, got {other:?}"),
        Ok(_) => panic!("expected CrossDatabaseForeignKey BuildError, build succeeded"),
    }
}

/// (b) The same cross-DB FK with `#[umbral(db_constraint = false)]` →
/// `App::build()` succeeds. This is the ONE success build in this
/// binary; it publishes the ambient registry.
#[tokio::test(flavor = "multi_thread")]
async fn cross_db_fk_with_optout_builds() {
    let mut settings = umbral::Settings::from_env().expect("settings load");
    settings.database_url = "sqlite::memory:".to_string();

    umbral::App::builder()
        .settings(settings)
        .database("default", mem_pool().await)
        .database("analytics", mem_pool().await)
        .model::<XdbParent>()
        .model::<XdbOkChild>()
        .build()
        .expect("db_constraint = false should let a cross-DB FK build");
}

/// The macro plumbs `db_constraint` onto the generated `FieldSpec`:
/// `true` by default, `false` when the opt-out is set.
#[test]
fn db_constraint_lands_on_field_spec() {
    let bad_fk = <XdbBadChild as Model>::FIELDS
        .iter()
        .find(|f| f.name == "parent")
        .expect("parent field");
    assert!(
        bad_fk.db_constraint,
        "default FK keeps db_constraint = true"
    );

    let ok_fk = <XdbOkChild as Model>::FIELDS
        .iter()
        .find(|f| f.name == "parent")
        .expect("parent field");
    assert!(
        !ok_fk.db_constraint,
        "#[umbral(db_constraint = false)] sets db_constraint = false"
    );
}

// ---------------------------------------------------------------------------
// DDL-emission tests. Drive `render_operation_for` directly so they need
// no `App::build()` and touch no global state.
// ---------------------------------------------------------------------------

/// Build a minimal `CreateTable` op: an `id` PK plus one FK column whose
/// `db_constraint` flag is the parameter.
fn create_table_with_fk(table: &str, fk_target: &str, db_constraint: bool) -> Operation {
    Operation::CreateTable {
        table: table.to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
                fk_target: None,
                db_constraint: true,
                noform: false,
                privileged: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: FkAction::NoAction,
                on_update: FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: None,
                slug_from: None,
            },
            Column {
                name: "parent".to_string(),
                ty: SqlType::ForeignKey,
                primary_key: false,
                nullable: false,
                fk_target: Some(fk_target.to_string()),
                db_constraint,
                noform: false,
                privileged: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: FkAction::NoAction,
                on_update: FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: None,
                slug_from: None,
            },
        ],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    }
}

/// Pick the `CREATE TABLE` statement out of a rendered op's statements.
/// FK auto-indexing means a `CreateTable` op renders as multiple
/// statements (the table itself plus a `CREATE INDEX` per FK column);
/// the DDL assertions here target the table definition specifically.
fn create_table_stmt<'a>(stmts: &'a [String], backend: &str) -> &'a String {
    stmts
        .iter()
        .find(|s| s.to_ascii_uppercase().contains("CREATE TABLE"))
        .unwrap_or_else(|| panic!("{backend}: expected a CREATE TABLE statement; got {stmts:?}"))
}

/// A same-DB FK with `db_constraint = false` builds and the emitted DDL
/// carries NO `FOREIGN KEY` / `REFERENCES` clause for that column — only
/// the bare column survives.
#[test]
fn db_constraint_false_omits_references_in_ddl() {
    let op = create_table_with_fk("dcf_child", "dcf_parent", false);

    for backend in ["sqlite", "postgres"] {
        let stmts = render_operation_for(&op, backend);
        // A CreateTable for an FK column now also emits a CREATE INDEX on
        // that column (FK auto-indexing), so inspect the CREATE TABLE
        // statement specifically rather than assuming a single statement.
        let sql = create_table_stmt(&stmts, backend);
        assert!(
            !sql.to_ascii_uppercase().contains("REFERENCES"),
            "{backend}: db_constraint = false must omit REFERENCES; got {sql}"
        );
        assert!(
            !sql.to_ascii_uppercase().contains("FOREIGN KEY"),
            "{backend}: db_constraint = false must omit FOREIGN KEY; got {sql}"
        );
        // The logical column itself still exists.
        assert!(
            sql.contains("parent"),
            "{backend}: the FK column must still be emitted; got {sql}"
        );
    }
}

/// A normal same-DB FK (default `db_constraint = true`) still emits the
/// physical `REFERENCES` constraint — regression guard for the opt-out.
#[test]
fn default_fk_still_emits_references_in_ddl() {
    let op = create_table_with_fk("dft_child", "dft_parent", true);

    for backend in ["sqlite", "postgres"] {
        let stmts = render_operation_for(&op, backend);
        // FK auto-indexing adds a CREATE INDEX statement alongside the
        // CreateTable; assert on the CREATE TABLE statement itself.
        let sql = create_table_stmt(&stmts, backend);
        assert!(
            sql.to_ascii_uppercase().contains("REFERENCES"),
            "{backend}: a default FK must still emit REFERENCES; got {sql}"
        );
        assert!(
            sql.contains("dft_parent"),
            "{backend}: REFERENCES must name the target table; got {sql}"
        );
    }
}
