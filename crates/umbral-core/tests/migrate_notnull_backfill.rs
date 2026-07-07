//! audit_2 core-migrate #5 — tightening a column nullable→NOT NULL WITH a
//! default must BACKFILL existing NULL rows, not abort. The `diff` guard already
//! allows the tighten when a default is set; the renderers now emit the backfill
//! (Postgres: `UPDATE ... WHERE c IS NULL` before `SET NOT NULL`; SQLite:
//! `COALESCE(c, default)` in the recreation dance's INSERT…SELECT).

#![allow(dead_code)]

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{Column, ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbral::orm::{FkAction, SqlType};

fn col(name: &str, ty: SqlType, nullable: bool, default: &str) -> Column {
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
        trim: false,
        lowercase: false,
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

/// `Widget(id, status)` — `status` is nullable-no-default in `prev`, and
/// NOT NULL with `default = "active"` in `curr` (the tightening).
fn meta(status_nullable: bool, status_default: &str) -> ModelMeta {
    ModelMeta {
        name: "Widget".to_string(),
        table: "widget".to_string(),
        fields: vec![
            col("id", SqlType::BigInt, false, ""),
            col("status", SqlType::Text, status_nullable, status_default),
        ],
        display: "Widget".to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        app_label: "app".to_string(),
    }
}

fn tighten_ops() -> Vec<Operation> {
    let prev = Snapshot {
        models: vec![meta(true, "")],
    };
    let curr = Snapshot {
        models: vec![meta(false, "active")],
    };
    let ops = diff(&prev, &curr).expect("diff");
    assert!(
        ops.iter()
            .any(|o| matches!(o, Operation::AlterColumn { .. })),
        "expected an AlterColumn for the tighten; got {ops:?}"
    );
    ops
}

#[tokio::test]
async fn sqlite_dance_backfills_nulls_when_tightening_to_not_null() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("notnull_backfill.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // Live table: status is nullable, with one NULL row and one real value.
    sqlx::query("CREATE TABLE widget (id INTEGER PRIMARY KEY, status TEXT)")
        .execute(&pool)
        .await
        .expect("create");
    sqlx::query("INSERT INTO widget (id, status) VALUES (1, NULL), (2, 'shipped')")
        .execute(&pool)
        .await
        .expect("seed");

    // Apply the tightening migration (dance is bracketed like gaps3 #13).
    let ops = tighten_ops();
    let mut conn = pool.acquire().await.expect("acquire");
    for op in &ops {
        for sql in render_operation_for(op, "sqlite") {
            sqlx::query(&sql)
                .execute(&mut *conn)
                .await
                .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
        }
    }

    // The NULL row was backfilled to the default; the real value is untouched.
    let s1: String = sqlx::query("SELECT status FROM widget WHERE id = 1")
        .fetch_one(&mut *conn)
        .await
        .expect("row 1")
        .get("status");
    let s2: String = sqlx::query("SELECT status FROM widget WHERE id = 2")
        .fetch_one(&mut *conn)
        .await
        .expect("row 2")
        .get("status");
    assert_eq!(
        s1, "active",
        "the NULL row must be backfilled to the default"
    );
    assert_eq!(
        s2, "shipped",
        "an existing non-NULL value must be preserved"
    );

    // The column is now NOT NULL — inserting a NULL fails.
    let dup = sqlx::query("INSERT INTO widget (id, status) VALUES (3, NULL)")
        .execute(&mut *conn)
        .await;
    assert!(dup.is_err(), "status must now be NOT NULL");
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn postgres_backfills_nulls_when_tightening_to_not_null() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");

    sqlx::query("DROP TABLE IF EXISTS widget")
        .execute(&pool)
        .await
        .expect("drop");
    sqlx::query("CREATE TABLE widget (id BIGINT PRIMARY KEY, status TEXT)")
        .execute(&pool)
        .await
        .expect("create");
    sqlx::query("INSERT INTO widget (id, status) VALUES (1, NULL), (2, 'shipped')")
        .execute(&pool)
        .await
        .expect("seed");

    // Render the tighten for Postgres and apply. Pre-fix, the bare SET NOT NULL
    // aborted here with "column status contains null values".
    for op in &tighten_ops() {
        for sql in render_operation_for(op, "postgres") {
            sqlx::query(&sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("statement failed ({e}): {sql}"));
        }
    }

    let s1: String = sqlx::query("SELECT status FROM widget WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("row 1")
        .get("status");
    assert_eq!(
        s1, "active",
        "the NULL row must be backfilled to the default"
    );

    let null_insert = sqlx::query("INSERT INTO widget (id, status) VALUES (3, NULL)")
        .execute(&pool)
        .await;
    assert!(null_insert.is_err(), "status must now be NOT NULL");

    sqlx::query("DROP TABLE IF EXISTS widget")
        .execute(&pool)
        .await
        .expect("cleanup");
}
