//! audit_2 core-migrate #10 — the SQLite `AlterColumn` recreation dance must
//! RE-CREATE the table's composite UNIQUE constraints and multi-column indexes,
//! not silently drop them. Before the fix a routine nullable-flip / safe-cast
//! alter rebuilt the table without them, so duplicates that a `unique_together`
//! forbade became insertable (integrity loss) and secondary indexes vanished.
//!
//! Drives the REAL `diff()` → `render_operation_for("sqlite")` against a live,
//! populated table that carries the constraints, then proves they survive.

#![allow(dead_code)]

use sqlx::Connection;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, ModelMeta, Snapshot, diff, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key: name == "id",
        nullable,
        fk_target: None,
        noform: false,
        privileged: false,
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

fn meta(status_nullable: bool) -> ModelMeta {
    ModelMeta {
        name: "Membership".to_string(),
        table: "membership".to_string(),
        fields: vec![
            col("id", SqlType::BigInt, false),
            col("org_id", SqlType::BigInt, false),
            col("user_id", SqlType::BigInt, false),
            col("status", SqlType::Text, status_nullable),
        ],
        display: "Membership".to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        // A user may belong to an org only once.
        unique_together: vec![vec!["org_id".to_string(), "user_id".to_string()]],
        // A composite lookup index.
        indexes: vec![vec!["org_id".to_string(), "status".to_string()]],
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        audited: false,
        app_label: "app".to_string(),
    }
}

#[tokio::test]
async fn alter_column_dance_preserves_unique_together_and_indexes() {
    // The only diff is a nullable flip on `status` → forces the recreation dance.
    let prev = Snapshot {
        models: vec![meta(false)],
    };
    let current = Snapshot {
        models: vec![meta(true)],
    };
    let ops = diff(&prev, &current).expect("diff");
    assert!(
        ops.iter()
            .any(|o| matches!(o, umbral::migrate::Operation::AlterColumn { .. })),
        "expected an AlterColumn: {ops:?}"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("dance_constraints.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("pool");

    // The live table carries BOTH constraints the dance must preserve.
    sqlx::query(
        "CREATE TABLE membership (\
            id INTEGER PRIMARY KEY,\
            org_id INTEGER NOT NULL,\
            user_id INTEGER NOT NULL,\
            status TEXT NOT NULL,\
            UNIQUE (org_id, user_id)\
         )",
    )
    .execute(&pool)
    .await
    .expect("create");
    sqlx::query("CREATE INDEX idx_membership_org_id_status ON membership (org_id, status)")
        .execute(&pool)
        .await
        .expect("index");
    sqlx::query(
        "INSERT INTO membership (id, org_id, user_id, status) VALUES (1, 10, 20, 'active')",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // Apply the migration (gaps3 #13 recipe brackets the dance).
    let mut conn = pool.acquire().await.expect("acquire");
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *conn)
        .await
        .expect("fk off");
    {
        let mut tx = conn.begin().await.expect("begin");
        for op in &ops {
            for sql in render_operation_for(op, "sqlite") {
                sqlx::query(&sql)
                    .execute(&mut *tx)
                    .await
                    .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
            }
        }
        tx.commit().await.expect("commit");
    }
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *conn)
        .await
        .expect("fk on");

    // 1. The row survived + the nullable flip applied.
    let row = sqlx::query("SELECT status FROM membership WHERE id = 1")
        .fetch_one(&mut *conn)
        .await
        .expect("row survived");
    assert_eq!(row.get::<String, _>("status"), "active");

    // 2. The composite UNIQUE constraint SURVIVED — a duplicate (org_id,user_id)
    //    is rejected. Before the fix this INSERT succeeded (integrity loss).
    let dup = sqlx::query(
        "INSERT INTO membership (id, org_id, user_id, status) VALUES (2, 10, 20, 'pending')",
    )
    .execute(&mut *conn)
    .await;
    assert!(
        dup.is_err(),
        "the unique_together (org_id, user_id) constraint must survive the dance"
    );

    // 3. The composite index SURVIVED — it's back in sqlite_master.
    let idx = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='membership' \
         AND name='idx_membership_org_id_status'",
    )
    .fetch_optional(&mut *conn)
    .await
    .expect("query sqlite_master");
    assert!(
        idx.is_some(),
        "the composite index must be re-created by the dance"
    );
}
