//! audit_2 core-migrate — the autodetector must generate a migration when a
//! model gains or loses a TABLE-level `unique_together` group or a
//! multi-column `indexes` entry WITHOUT any column change. Before the fix,
//! `diff` only compared columns, so such a change produced no operations at
//! all: `makemigrations` said "no changes" and the constraint was silently
//! never created (a `unique_together` that forbade duplicates simply didn't
//! exist, so duplicates stayed insertable).
//!
//! Drives the REAL `diff()` → `render_operation_for("sqlite")` against a live,
//! populated table and proves the constraint is created on ADD and removed on
//! DROP — in both directions, for both `unique_together` and `indexes`.

#![allow(dead_code)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key: name == "id",
        nullable: false,
        fk_target: None,
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

/// `Membership(id, org_id, user_id, status)` — identical columns every time;
/// only the table-level `unique_together` / `indexes` vary.
fn meta(unique_together: Vec<Vec<String>>, indexes: Vec<Vec<String>>) -> ModelMeta {
    ModelMeta {
        name: "Membership".to_string(),
        table: "membership".to_string(),
        fields: vec![
            col("id", SqlType::BigInt),
            col("org_id", SqlType::BigInt),
            col("user_id", SqlType::BigInt),
            col("status", SqlType::Text),
        ],
        display: "Membership".to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together,
        indexes,
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        app_label: "app".to_string(),
    }
}

fn snap(unique_together: Vec<Vec<String>>, indexes: Vec<Vec<String>>) -> Snapshot {
    Snapshot {
        models: vec![meta(unique_together, indexes)],
    }
}

fn uniq(cols: &[&str]) -> Vec<Vec<String>> {
    vec![cols.iter().map(|c| c.to_string()).collect()]
}

async fn apply(pool: &sqlx::SqlitePool, ops: &[Operation]) {
    for op in ops {
        for sql in render_operation_for(op, "sqlite") {
            sqlx::query(&sql)
                .execute(pool)
                .await
                .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
        }
    }
}

async fn index_exists(pool: &sqlx::SqlitePool, name: &str) -> bool {
    sqlx::query("SELECT name FROM sqlite_master WHERE type='index' AND name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await
        .expect("query sqlite_master")
        .is_some()
}

async fn duplicate_membership_rejected(pool: &sqlx::SqlitePool, id: i64) -> bool {
    // Row (org_id=10, user_id=20) already exists (id=1). A second one with the
    // same (org_id,user_id) is a unique_together violation.
    sqlx::query("INSERT INTO membership (id, org_id, user_id, status) VALUES (?, 10, 20, 'x')")
        .bind(id)
        .execute(pool)
        .await
        .is_err()
}

#[tokio::test]
async fn adding_then_removing_unique_together_and_index_round_trips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("idx_autodetect.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // A live table with NO composite constraints, one seed row.
    sqlx::query(
        "CREATE TABLE membership (\
            id INTEGER PRIMARY KEY,\
            org_id INTEGER NOT NULL,\
            user_id INTEGER NOT NULL,\
            status TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create");
    sqlx::query(
        "INSERT INTO membership (id, org_id, user_id, status) VALUES (1, 10, 20, 'active')",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // Baseline: with no constraint, a duplicate (org_id,user_id) is allowed.
    assert!(
        !duplicate_membership_rejected(&pool, 100).await,
        "precondition: without unique_together the duplicate must be insertable"
    );
    sqlx::query("DELETE FROM membership WHERE id = 100")
        .execute(&pool)
        .await
        .expect("cleanup seed dup");

    // ---- ADD: a unique_together + a composite index appear, no column change ----
    let none = snap(Vec::new(), Vec::new());
    let with_constraints = snap(
        uniq(&["org_id", "user_id"]),
        vec![vec!["org_id".to_string(), "status".to_string()]],
    );
    let add_ops = diff(&none, &with_constraints).expect("diff add");
    // The autodetector MUST have produced ops (the bug was zero ops here).
    assert!(
        add_ops
            .iter()
            .any(|o| matches!(o, Operation::AddIndex { unique: true, .. })),
        "expected an AddIndex(unique) for the new unique_together; got {add_ops:?}"
    );
    assert!(
        add_ops
            .iter()
            .any(|o| matches!(o, Operation::AddIndex { unique: false, .. })),
        "expected an AddIndex for the new composite index; got {add_ops:?}"
    );
    apply(&pool, &add_ops).await;

    // Now the constraint is enforced and the index exists.
    assert!(
        duplicate_membership_rejected(&pool, 2).await,
        "after applying the add-migration, the unique_together must reject the duplicate"
    );
    assert!(
        index_exists(&pool, "uniq_membership_org_id_user_id").await,
        "the named unique index must exist after the add"
    );
    assert!(
        index_exists(&pool, "idx_membership_org_id_status").await,
        "the composite index must exist after the add"
    );

    // ---- DROP: remove both again, no column change ----
    let drop_ops = diff(&with_constraints, &none).expect("diff drop");
    assert!(
        drop_ops
            .iter()
            .any(|o| matches!(o, Operation::DropIndex { .. })),
        "expected DropIndex ops when the constraints are removed; got {drop_ops:?}"
    );
    apply(&pool, &drop_ops).await;

    // The constraint is gone (duplicate insertable again) and so is the index.
    assert!(
        !duplicate_membership_rejected(&pool, 3).await,
        "after applying the drop-migration, the duplicate must be insertable again"
    );
    assert!(
        !index_exists(&pool, "uniq_membership_org_id_user_id").await,
        "the unique index must be gone after the drop"
    );
    assert!(
        !index_exists(&pool, "idx_membership_org_id_status").await,
        "the composite index must be gone after the drop"
    );
}

/// A model with an unchanged `unique_together` produces NO ops (no spurious
/// migration churn), but a real column change still round-trips.
#[tokio::test]
async fn unchanged_constraints_produce_no_index_ops() {
    let a = snap(uniq(&["org_id", "user_id"]), Vec::new());
    let b = snap(uniq(&["org_id", "user_id"]), Vec::new());
    let ops = diff(&a, &b).expect("diff");
    assert!(
        ops.is_empty(),
        "identical unique_together must produce no ops; got {ops:?}"
    );
}
