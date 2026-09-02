//! gap 70 — dropping a `#[umbral(unique)]` column must APPLY on SQLite.
//!
//! SQLite's `ALTER TABLE <t> DROP COLUMN <c>` REFUSES a column that participates
//! in a UNIQUE / PRIMARY KEY / FOREIGN KEY constraint or is indexed
//! (`cannot drop UNIQUE column: "slug"`). The real bug: removing a unique `slug`
//! field autodetected a plain `DropColumn`, and `migrate` died at apply time.
//!
//! The fix routes a constrained-column drop through the SAME table-recreation
//! dance `AlterColumn` uses on SQLite (create a new table without the column,
//! copy the survivors, drop the old, rename). The diff engine detects the
//! outgoing column is `unique` and carries the post-drop schema in the
//! `DropColumn` op's `new_columns`; the SQLite renderer then rebuilds instead of
//! emitting the bare `DROP COLUMN`. Postgres is unaffected (its native
//! `DROP COLUMN` handles a unique column fine).
//!
//! These tests drive the REAL `diff()` autodetector, assert it picked the
//! rebuild strategy, then apply the engine's rendered SQLite output against a
//! live, POPULATED table (existing rows are the test, per CLAUDE.md) and read
//! the surviving row back.

#![allow(dead_code)]

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType, primary_key: bool, nullable: bool, unique: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
        nullable,
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
        unique,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_uuid: false,
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
        view: None,
        materialized: false,
        name: "Article".to_string(),
        table: "article".to_string(),
        fields: cols,
        display: "Article".to_string(),
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

async fn fresh_pool(filename: &str) -> sqlx::SqlitePool {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join(filename);
    // Keep the dir alive for the test process; the temp file is small.
    std::mem::forget(tmp);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(10))
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("sqlite pool")
}

/// Apply a migration's ops exactly as the engine's private
/// `apply_sqlite_migration_tx` does: on a PINNED connection, bracket the
/// transaction with `PRAGMA foreign_keys=OFF` … `foreign_keys=ON` whenever an op
/// runs the table-recreation dance (an `AlterColumn`, OR — gap 70 — a
/// `DropColumn` carrying a rebuild `new_columns`), so step 3's `DROP TABLE` on a
/// table with inbound FKs doesn't trip error 787. This mirrors the real engine
/// decision so the test proves the rendered output applies under the engine's
/// recipe.
async fn apply_like_engine(pool: &sqlx::SqlitePool, ops: &[Operation]) {
    use sqlx::Acquire as _;

    let needs_fk_off = ops.iter().any(|op| {
        matches!(op, Operation::AlterColumn { .. })
            || matches!(
                op,
                Operation::DropColumn {
                    new_columns: Some(_),
                    ..
                }
            )
    });

    let mut conn = pool.acquire().await.expect("acquire");
    if needs_fk_off {
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *conn)
            .await
            .expect("fk off");
    }
    {
        let mut tx = conn.begin().await.expect("begin");
        for op in ops {
            for sql in render_operation_for(op, "sqlite") {
                sqlx::query(&sql)
                    .execute(&mut *tx)
                    .await
                    .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
            }
        }
        tx.commit().await.expect("commit must succeed (gap 70)");
    }
    if needs_fk_off {
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *conn)
            .await
            .expect("fk on");
    }
}

#[tokio::test]
async fn drop_unique_column_rebuilds_and_applies_on_sqlite() {
    // prev: id (PK), title (NOT NULL), slug (NOT NULL, UNIQUE — the constrained
    // column about to be dropped).
    let prev = Snapshot {
        models: vec![meta(vec![
            col("id", SqlType::BigInt, true, false, false),
            col("title", SqlType::Text, false, false, false),
            col("slug", SqlType::Text, false, false, true),
        ])],
    };
    // current: slug is gone.
    let current = Snapshot {
        models: vec![meta(vec![
            col("id", SqlType::BigInt, true, false, false),
            col("title", SqlType::Text, false, false, false),
        ])],
    };

    // Real autodetect. It must emit exactly one DropColumn, and — because slug
    // is UNIQUE — that op must carry `new_columns` (the rebuild strategy), not a
    // bare drop. This is the assertion that proves autodetect picked the
    // SQLite-safe path.
    let ops = diff(&prev, &current).expect("diff must not error");
    let drops: Vec<&Operation> = ops
        .iter()
        .filter(|o| matches!(o, Operation::DropColumn { .. }))
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected exactly one DropColumn; got {ops:?}"
    );
    match drops[0] {
        Operation::DropColumn {
            column,
            new_columns,
            ..
        } => {
            assert_eq!(column, "slug");
            let survivors = new_columns
                .as_ref()
                .expect("gap 70: dropping a UNIQUE column must carry the rebuild new_columns");
            let names: Vec<&str> = survivors.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["id", "title"],
                "rebuild new_columns must be the survivors, sans slug"
            );
        }
        other => panic!("expected DropColumn, got {other:?}"),
    }

    // Live, POPULATED table with the UNIQUE constraint a bare DROP COLUMN would
    // reject.
    let pool = fresh_pool("gap70_rebuild.sqlite").await;
    sqlx::query(
        "CREATE TABLE article (\
            id INTEGER PRIMARY KEY,\
            title TEXT NOT NULL,\
            slug TEXT NOT NULL UNIQUE\
         )",
    )
    .execute(&pool)
    .await
    .expect("create article");
    sqlx::query("INSERT INTO article (id, title, slug) VALUES (1, 'Hello', 'hello')")
        .execute(&pool)
        .await
        .expect("seed row");

    // Apply through the engine's recipe. Must NOT raise "cannot drop UNIQUE
    // column".
    apply_like_engine(&pool, &ops).await;

    // The pre-existing row survived, with its other columns intact.
    let row = sqlx::query("SELECT id, title FROM article WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("row survived the drop-unique migration");
    assert_eq!(row.get::<i64, _>("id"), 1);
    assert_eq!(row.get::<String, _>("title"), "Hello");

    // The slug column is truly gone.
    let gone = sqlx::query("SELECT slug FROM article")
        .fetch_optional(&pool)
        .await;
    assert!(gone.is_err(), "slug must no longer exist after the drop");

    // And the table still works: a new insert with a duplicate former-slug value
    // is now accepted (the UNIQUE constraint went away with the column).
    sqlx::query("INSERT INTO article (id, title) VALUES (2, 'Hello')")
        .execute(&pool)
        .await
        .expect("insert into the rebuilt table works");
    let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM article")
        .fetch_one(&pool)
        .await
        .expect("count")
        .get::<i64, _>("n");
    assert_eq!(count, 2, "both rows present after the rebuild");
}

/// Regression witness: the OLD behaviour (a bare `ALTER TABLE DROP COLUMN` on a
/// UNIQUE column) is exactly what SQLite rejects. This documents the failure the
/// fix removes — it hand-builds the pre-fix op shape (`new_columns: None`) purely
/// to show the raw error, and asserts the message the real bug produced.
#[tokio::test]
async fn bare_drop_of_unique_column_is_the_error_the_fix_removes() {
    let pool = fresh_pool("gap70_bare.sqlite").await;
    sqlx::query(
        "CREATE TABLE article (\
            id INTEGER PRIMARY KEY,\
            title TEXT NOT NULL,\
            slug TEXT NOT NULL UNIQUE\
         )",
    )
    .execute(&pool)
    .await
    .expect("create article");
    sqlx::query("INSERT INTO article (id, title, slug) VALUES (1, 'Hello', 'hello')")
        .execute(&pool)
        .await
        .expect("seed row");

    // The pre-fix op: a bare drop with no rebuild payload.
    let bare = Operation::DropColumn {
        table: "article".to_string(),
        column: "slug".to_string(),
        new_columns: None,
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let sql = &render_operation_for(&bare, "sqlite")[0];
    let err = sqlx::query(sql)
        .execute(&pool)
        .await
        .expect_err("SQLite must reject dropping a UNIQUE column with a bare DROP COLUMN");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot drop UNIQUE column"),
        "expected the gap-70 error, got: {msg}"
    );
}
