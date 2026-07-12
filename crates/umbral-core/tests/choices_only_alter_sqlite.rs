//! gaps3 #24 — adding/removing a `Choices` variant (a choices-only column delta)
//! must NOT trigger the SQLite table-recreation dance. Choices are enforced in
//! Rust on SQLite (`build_column_def_sqlite` emits no CHECK), so a choices-only
//! change is invisible to the schema — the rebuild is pure churn. Postgres DOES
//! store a `CHECK (col IN (...))`, so it must still swap that constraint.

#![allow(dead_code)]

use umbral::migrate::{Column, Operation, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn text_col(name: &str, choices: &[&str]) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: choices.iter().map(|s| s.to_string()).collect(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
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
        text_format: None,
        slug_from: None,
    }
}

#[test]
fn choices_only_alter_is_a_noop_on_sqlite_but_swaps_the_pg_check() {
    // Same Text column; the ONLY difference is an added "waived" choice.
    let prev = vec![text_col("status", &["pending", "paid"])];
    let new = vec![text_col("status", &["pending", "paid", "waived"])];
    let op = Operation::AlterColumn {
        table: "payment".to_string(),
        column: "status".to_string(),
        new_columns: new,
        prev_columns: Some(prev),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    // SQLite: no CHECK in the DDL, so a choices-only alter is invisible to the
    // schema — no table rebuild.
    assert!(
        render_operation_for(&op, "sqlite").is_empty(),
        "a choices-only alter must NOT rebuild the table on SQLite; got: {:?}",
        render_operation_for(&op, "sqlite")
    );

    // Postgres: choices ARE a CHECK constraint, so the alter must swap it.
    assert!(
        !render_operation_for(&op, "postgres").is_empty(),
        "Postgres must still swap the CHECK constraint on a choices change"
    );
}
