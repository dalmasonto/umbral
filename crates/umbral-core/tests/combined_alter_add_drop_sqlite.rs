//! audit_2 H21 — a single `makemigrations` diff that combines an `AlterColumn`
//! with an `AddColumn` and/or `DropColumn` on the SAME table must produce a
//! migration that APPLIES on SQLite.
//!
//! The SQLite `AlterColumn` recreation dance rebuilds the table via
//! `INSERT INTO tmp (cols) SELECT cols FROM <old>`. The bug: the op carried
//! `new_columns = current.fields`, which INCLUDES a not-yet-added column (the
//! `SELECT` then hits "no such column") and EXCLUDES a to-be-dropped column
//! (the rebuild removes it early, so the later `DropColumn` fails). The fix
//! shapes `new_columns` like the OLD table — old column set, current defs on
//! survivors — so the rebuild only references existing columns; the add/drop
//! ops (emitted after) finish the job.
//!
//! Drives the REAL `diff()` → `render_operation_for("sqlite")` output against a
//! live, populated table and reads the rows back.

#![allow(dead_code)]

use sqlx::Connection;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, ModelMeta, Snapshot, diff, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType, primary_key: bool, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
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

fn meta(cols: Vec<Column>) -> ModelMeta {
    ModelMeta {
        name: "Account".to_string(),
        table: "acct".to_string(),
        fields: cols,
        display: "Account".to_string(),
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

#[tokio::test]
async fn combined_alter_add_drop_on_one_table_applies_on_sqlite() {
    // prev: id, name (NOT NULL), old_flag (NOT NULL, will be DROPPED),
    //       status (NOT NULL, will be ALTERED to nullable).
    let prev = Snapshot {
        models: vec![meta(vec![
            col("id", SqlType::BigInt, true, false),
            col("name", SqlType::Text, false, false),
            col("old_flag", SqlType::Boolean, false, false),
            col("status", SqlType::Text, false, false),
        ])],
    };
    // current: status is now nullable (ALTER), old_flag gone (DROP),
    //          extra added (ADD, nullable so the add is legal on a populated table).
    let current = Snapshot {
        models: vec![meta(vec![
            col("id", SqlType::BigInt, true, false),
            col("name", SqlType::Text, false, false),
            col("status", SqlType::Text, false, true),
            col("extra", SqlType::Text, false, true),
        ])],
    };

    let ops = diff(&prev, &current).expect("diff must not error");
    // Sanity: the diff really did combine an alter + a drop + an add.
    let kinds: Vec<&str> = ops
        .iter()
        .map(|o| match o {
            umbral::migrate::Operation::AlterColumn { .. } => "alter",
            umbral::migrate::Operation::AddColumn { .. } => "add",
            umbral::migrate::Operation::DropColumn { .. } => "drop",
            _ => "other",
        })
        .collect();
    assert!(
        kinds.contains(&"alter"),
        "expected an AlterColumn: {kinds:?}"
    );
    assert!(kinds.contains(&"add"), "expected an AddColumn: {kinds:?}");
    assert!(kinds.contains(&"drop"), "expected a DropColumn: {kinds:?}");

    // Live, populated table.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("combined.sqlite");
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
        .expect("sqlite pool");
    sqlx::query(
        "CREATE TABLE acct (\
            id INTEGER PRIMARY KEY,\
            name TEXT NOT NULL,\
            old_flag BOOLEAN NOT NULL,\
            status TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create acct");
    sqlx::query("INSERT INTO acct (id, name, old_flag, status) VALUES (1, 'alice', 1, 'active')")
        .execute(&pool)
        .await
        .expect("seed row");

    // Apply exactly as the engine does — the gaps3 #13 recipe brackets the
    // AlterColumn dance with FK enforcement off on a pinned connection.
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
        tx.commit().await.expect("commit must succeed (H21)");
    }
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *conn)
        .await
        .expect("fk on");

    // The row survived and the schema is the new shape: status kept its value,
    // old_flag is gone, extra exists (NULL).
    let row = sqlx::query("SELECT id, name, status, extra FROM acct WHERE id = 1")
        .fetch_one(&mut *conn)
        .await
        .expect("row survived the combined migration");
    assert_eq!(row.get::<i64, _>("id"), 1);
    assert_eq!(row.get::<String, _>("name"), "alice");
    assert_eq!(row.get::<String, _>("status"), "active");
    assert!(
        row.try_get::<Option<String>, _>("extra")
            .expect("extra col exists")
            .is_none(),
        "added column present and NULL"
    );

    // old_flag column is truly gone.
    let dropped = sqlx::query("SELECT old_flag FROM acct")
        .fetch_optional(&mut *conn)
        .await;
    assert!(
        dropped.is_err(),
        "old_flag must no longer exist after the drop"
    );
}
