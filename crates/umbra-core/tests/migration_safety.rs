//! Zero-downtime safety classification (feature #65). `classify_operation`
//! tags each migration operation SAFE / WARNING / UNSAFE so the
//! `checkmigrations` command can gate a blue-green deploy. Pure function
//! over `Operation` values — no DB, no `App::build()`.

use umbra::migrate::{Column, OpSafety, Operation, classify_operation};
use umbra::orm::{FkAction, SqlType};

/// A text column with the given nullability and SQL default. `default`
/// is the empty string for "no default".
fn col(name: &str, nullable: bool, default: &str) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable,
        fk_target: None,
        noform: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: default.to_string(),
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
    }
}

#[test]
fn additive_ops_are_safe() {
    // A brand-new table touches no existing rows and no old code.
    let create = Operation::CreateTable {
        table: "invoice".into(),
        columns: vec![col("id", false, "")],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    assert_eq!(classify_operation(&create), OpSafety::Safe);

    // A nullable column is additive — old inserts simply omit it.
    let add_nullable = Operation::AddColumn {
        table: "invoice".into(),
        column: col("note", true, ""),
    };
    assert_eq!(classify_operation(&add_nullable), OpSafety::Safe);

    // NOT NULL but WITH a default is also additive — the DB fills the gap.
    let add_defaulted = Operation::AddColumn {
        table: "invoice".into(),
        column: col("status", false, "'draft'"),
    };
    assert_eq!(classify_operation(&add_defaulted), OpSafety::Safe);

    let create_m2m = Operation::CreateM2MTable {
        junction_table: "invoice_tags".into(),
        parent_table: "invoice".into(),
        parent_col: "id".into(),
        child_table: "tag".into(),
        child_col: "id".into(),
        parent_ty: SqlType::BigInt,
        child_ty: SqlType::BigInt,
    };
    assert_eq!(classify_operation(&create_m2m), OpSafety::Safe);
}

#[test]
fn not_null_add_without_default_warns() {
    let op = Operation::AddColumn {
        table: "invoice".into(),
        column: col("status", false, ""),
    };
    let safety = classify_operation(&op);
    assert!(safety.is_warning(), "got {safety:?}");
    assert!(safety.reason().contains("NOT NULL"));
}

#[test]
fn drops_are_unsafe() {
    let drop_table = Operation::DropTable {
        table: "legacy".into(),
    };
    assert!(classify_operation(&drop_table).is_unsafe());

    let drop_col = Operation::DropColumn {
        table: "invoice".into(),
        column: "old_total".into(),
    };
    let s = classify_operation(&drop_col);
    assert!(s.is_unsafe());
    assert!(s.reason().contains("expand-contract") || s.reason().contains("Expand-contract"));

    let drop_m2m = Operation::DropM2MTable {
        junction_table: "invoice_tags".into(),
    };
    assert!(classify_operation(&drop_m2m).is_unsafe());
}

#[test]
fn renames_and_alters_warn() {
    let rename_table = Operation::RenameTable {
        from: "invoice".into(),
        to: "bill".into(),
    };
    assert!(classify_operation(&rename_table).is_warning());

    let rename_col = Operation::RenameColumn {
        table: "invoice".into(),
        from: "total".into(),
        to: "amount".into(),
        column: None,
    };
    assert!(classify_operation(&rename_col).is_warning());

    let alter = Operation::AlterColumn {
        table: "invoice".into(),
        column: "total".into(),
        new_columns: vec![col("total", false, "")],
        prev_columns: None,
    };
    let s = classify_operation(&alter);
    assert!(s.is_warning());
    assert!(s.reason().contains("NULL") || s.reason().contains("rewrites"));
}

#[test]
fn safe_tier_has_no_reason() {
    let safe = classify_operation(&Operation::CreateTable {
        table: "t".into(),
        columns: Vec::new(),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    });
    assert_eq!(safe.reason(), "");
    assert!(!safe.is_warning() && !safe.is_unsafe());
}
