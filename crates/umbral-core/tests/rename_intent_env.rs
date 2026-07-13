//! audit_2 H23 — the operator resolves an ambiguous column-shape rename via
//! `UMBRAL_MIGRATIONS_ASSUME_RENAMES`: `assume` → `RenameTable` (move the rows),
//! `independent` → drop + create (unrelated models). The unset default (error)
//! is covered in `rename_detection.rs`.
//!
//! Own test binary + a mutex because the env var is process-global; each case
//! sets it, runs `diff` synchronously, asserts, and clears it under the lock.

#![allow(dead_code)]

use std::sync::{Mutex, OnceLock};

use umbral_core::migrate::{Column, ModelMeta, Operation, Snapshot, diff};
use umbral_core::orm::{FkAction, SqlType};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn col(name: &str, ty: SqlType, primary_key: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
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

fn meta(name: &str, table: &str) -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: name.to_string(),
        table: table.to_string(),
        fields: vec![
            col("id", SqlType::BigInt, true),
            col("title", SqlType::Text, false),
        ],
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

fn foo_to_bar() -> (Snapshot, Snapshot) {
    (
        Snapshot {
            models: vec![meta("Foo", "foo")],
        },
        Snapshot {
            models: vec![meta("Bar", "bar")],
        },
    )
}

#[test]
fn assume_pairs_shape_match_into_rename() {
    let _guard = env_lock().lock().unwrap();
    // SAFETY: single-threaded under the lock; diff() reads the var synchronously.
    unsafe {
        std::env::set_var("UMBRAL_MIGRATIONS_ASSUME_RENAMES", "assume");
    }
    let (prev, curr) = foo_to_bar();
    let ops = diff(&prev, &curr).expect("assume mode must not error");
    unsafe {
        std::env::remove_var("UMBRAL_MIGRATIONS_ASSUME_RENAMES");
    }

    assert_eq!(
        ops.len(),
        1,
        "expected exactly one RenameTable, got: {ops:?}"
    );
    match &ops[0] {
        Operation::RenameTable { from, to } => {
            assert_eq!(from, "foo");
            assert_eq!(to, "bar");
        }
        other => panic!("expected RenameTable, got {other:?}"),
    }
}

#[test]
fn independent_treats_shape_match_as_drop_and_create() {
    let _guard = env_lock().lock().unwrap();
    // SAFETY: single-threaded under the lock.
    unsafe {
        std::env::set_var("UMBRAL_MIGRATIONS_ASSUME_RENAMES", "independent");
    }
    let (prev, curr) = foo_to_bar();
    let ops = diff(&prev, &curr).expect("independent mode must not error");
    unsafe {
        std::env::remove_var("UMBRAL_MIGRATIONS_ASSUME_RENAMES");
    }

    // No RenameTable — `bar` is created empty and `foo` is dropped; no row
    // transfer between the two unrelated models.
    assert!(
        !ops.iter()
            .any(|o| matches!(o, Operation::RenameTable { .. })),
        "independent mode must NOT emit a RenameTable, got: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|o| matches!(o, Operation::CreateTable { table, .. } if table == "bar")),
        "expected CreateTable for `bar`, got: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|o| matches!(o, Operation::DropTable { table, .. } if table == "foo")),
        "expected DropTable for `foo`, got: {ops:?}"
    );
}
