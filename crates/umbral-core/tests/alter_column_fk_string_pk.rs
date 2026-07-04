//! Audit core-migrate #1 — an `AlterColumn` that re-adds a foreign key
//! against a *String-PK* target must render `REFERENCES
//! target("<pk>")`, not the hardcoded `REFERENCES target("id")`.
//!
//! Before the fix, `render_alter_column_postgres` hardcoded the
//! referenced column to `"id"`. Changing `on_delete`/`on_update` on an
//! FK whose target PK is a String (e.g. `Permission.codename`) then
//! drops+re-adds `REFERENCES "permission"("id")`, which Postgres
//! rejects ("column id ... does not exist") — the deploy aborts. The
//! CreateTable path already resolves the real PK via `fk_target_pk`;
//! this test pins the re-add to the same behaviour.
//!
//! `fk_target_pk` reads the model REGISTRY, so this test must boot an
//! `App` (which seeds the registry) — hence a dedicated test binary so
//! the process-global `OnceLock` is written exactly once.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use umbral::migrate::{Column, Operation, render_operation_for};
use umbral::orm::{FkAction, ForeignKey, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "af_permission")]
pub struct Permission {
    #[umbral(primary_key)]
    pub codename: String,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "af_grant")]
pub struct Grant {
    pub id: i64,
    pub permission: ForeignKey<Permission>,
}

async fn boot() {
    let settings = umbral::Settings::from_env().expect("settings");
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Permission>()
        .model::<Grant>()
        .build()
        .expect("App::build");
}

/// The FK column, as it exists on the `af_grant` table. `fk_target`
/// points at the String-PK `af_permission` table.
fn fk_col(on_delete: FkAction) -> Column {
    Column {
        name: "permission".to_string(),
        ty: SqlType::ForeignKey,
        primary_key: false,
        nullable: false,
        fk_target: Some("af_permission".to_string()),
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete,
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
    }
}

#[tokio::test]
async fn alter_fk_readd_resolves_string_pk_target_column() {
    boot().await;

    // on_delete flips NoAction -> Cascade: the FK constraint is dropped
    // and re-added carrying the new clause.
    let prev = fk_col(FkAction::NoAction);
    let next = fk_col(FkAction::Cascade);
    let op = Operation::AlterColumn {
        table: "af_grant".to_string(),
        column: "permission".to_string(),
        new_columns: vec![next],
        prev_columns: Some(vec![prev]),
    };

    let stmts = render_operation_for(&op, "postgres");
    let joined = stmts.join("\n");

    assert!(
        joined.contains("REFERENCES \"af_permission\"(\"codename\")"),
        "FK re-add must reference the target's real PK column `codename`; got: {joined}",
    );
    assert!(
        !joined.contains("REFERENCES \"af_permission\"(\"id\")"),
        "FK re-add must NOT hardcode `id` for a String-PK target; got: {joined}",
    );
    assert!(
        joined.contains("ON DELETE CASCADE"),
        "FK re-add should carry the new ON DELETE clause; got: {joined}",
    );
}
