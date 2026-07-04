//! gaps3 #13 — an `AlterColumn` (nullable flip → SQLite table-recreation
//! dance) on a table with **inbound** foreign keys (a child row references it)
//! must APPLY, not die with `FOREIGN KEY constraint failed` (SQLite error 787).
//!
//! With `foreign_keys=ON` (what `connect_sqlite` sets), step 3 of the dance —
//! `DROP TABLE <parent>` — fails immediately because child rows still point at
//! it. The fix prepends `PRAGMA defer_foreign_keys = ON`, deferring the check
//! to commit; by then step 4 has recreated the table with the same rows, so
//! the children's references resolve and the commit succeeds. This drives the
//! REAL rendered statements against a live FK-enforcing connection + child row.

#![allow(dead_code)]

use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, Operation, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType, primary_key: bool, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
        nullable,
        fk_target: None,
        noform: false,
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

#[tokio::test]
async fn alter_column_on_table_with_inbound_fk_applies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("inbound_fk.sqlite");
    std::mem::forget(tmp);
    // One connection + FK enforcement ON, exactly like `connect_sqlite`.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("sqlite pool");

    // Parent + a child that references it, each with a live row.
    sqlx::query("CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create parent");
    sqlx::query(
        "CREATE TABLE child (\
            id INTEGER PRIMARY KEY,\
            parent_id INTEGER NOT NULL REFERENCES parent(id)\
         )",
    )
    .execute(&pool)
    .await
    .expect("create child");
    sqlx::query("INSERT INTO parent (id, name) VALUES (1, 'keep-me')")
        .execute(&pool)
        .await
        .expect("insert parent");
    sqlx::query("INSERT INTO child (id, parent_id) VALUES (1, 1)")
        .execute(&pool)
        .await
        .expect("insert child");

    // AlterColumn: flip parent.name NOT NULL → nullable (forces the dance).
    let op = Operation::AlterColumn {
        table: "parent".to_string(),
        column: "name".to_string(),
        new_columns: vec![
            col("id", SqlType::BigInt, true, false),
            col("name", SqlType::Text, false, true),
        ],
        prev_columns: None,
    };
    let stmts = render_operation_for(&op, "sqlite");

    // Apply as the migration engine does (gaps3 #13 recipe): FK enforcement
    // OFF *outside* the transaction (a no-op inside one), recreate, verify with
    // foreign_key_check, commit, FK enforcement back ON — all on ONE pinned
    // connection so the pragmas and the tx share it.
    let mut conn = pool.acquire().await.expect("acquire");
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *conn)
        .await
        .expect("fk off");
    {
        let mut tx = conn.begin().await.expect("begin");
        for sql in &stmts {
            sqlx::query(sql)
                .execute(&mut *tx)
                .await
                .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
        }
        let violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *tx)
            .await
            .expect("foreign_key_check");
        assert!(
            violations.is_empty(),
            "no FK violations after the recreation"
        );
        tx.commit().await.expect("commit must succeed (gaps3 #13)");
    }
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *conn)
        .await
        .expect("fk on");
    drop(conn);

    // Parent survived with its row; the child's FK still resolves.
    let name: String = sqlx::query_scalar("SELECT name FROM parent WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("parent row survives the recreation");
    assert_eq!(name, "keep-me");
    let child_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM child WHERE parent_id = 1")
        .fetch_one(&pool)
        .await
        .expect("child query");
    assert_eq!(child_count, 1, "the child row and its FK are intact");

    // And the column really is nullable now.
    sqlx::query("INSERT INTO parent (id, name) VALUES (2, NULL)")
        .execute(&pool)
        .await
        .expect("name is nullable after the alter");
}
