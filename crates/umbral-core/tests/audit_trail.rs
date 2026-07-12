//! Model-level audit trail — `#[umbral(audited)]` (gaps3 #54).
//!
//! The property that matters is COVERAGE. An audit log that silently misses
//! writes is worse than none, because it is the one you would testify from. So
//! these tests drive **both** write paths — the typed `QuerySet` and
//! `DynQuerySet` (which is what admin and REST run on) — and assert each records
//! the change.
//!
//! They also pin the three judgement calls: only *changed* fields are recorded,
//! a soft delete is logged as a DELETE (not an UPDATE, which is what it is on the
//! wire but not what a human means), and an unauthenticated write records
//! `actor = NULL` rather than inventing an author.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use umbral::orm::Model as _;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "invoice", audited)]
pub struct Invoice {
    pub id: i64,
    pub label: String,
    pub amount: i64,
}

/// Audited AND soft-delete: a soft delete must log as a DELETE.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "note", audited, soft_delete)]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// NOT audited — must produce no rows at all.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "plain")]
pub struct Plain {
    pub id: i64,
    pub name: String,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Invoice>()
            .model::<Note>()
            .model::<Plain>()
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
        for ddl in [
            "CREATE TABLE invoice (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL, amount INTEGER NOT NULL)",
            "CREATE TABLE note (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL, deleted_at TEXT)",
            "CREATE TABLE plain (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            // The audit table is auto-registered; create it as migrate would.
            "CREATE TABLE umbral_audit (id INTEGER PRIMARY KEY AUTOINCREMENT, table_name TEXT NOT NULL, \
             row_pk TEXT NOT NULL, action TEXT NOT NULL, actor TEXT, at TEXT NOT NULL, changes TEXT NOT NULL)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
    })
    .await;
}

/// Audit rows for one table, newest last.
async fn entries(table: &str) -> Vec<(String, Option<String>, Value)> {
    let pool = umbral::db::pool();
    let rows = sqlx::query_as::<_, (String, Option<String>, String)>(
        "SELECT action, actor, changes FROM umbral_audit WHERE table_name = ? ORDER BY id",
    )
    .bind(table)
    .fetch_all(&pool)
    .await
    .expect("select audit");
    rows.into_iter()
        .map(|(a, actor, c)| (a, actor, serde_json::from_str(&c).unwrap_or(Value::Null)))
        .collect()
}

async fn clear_audit() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM umbral_audit")
        .execute(&pool)
        .await
        .expect("clear");
}

/// The **typed** path: `Model::objects()` writes are audited — create, update,
/// delete — with a field-level before/after.
#[tokio::test]
async fn the_typed_path_is_audited() {
    let _g = lock().lock().await;
    boot().await;
    clear_audit().await;

    let inv = Invoice {
        id: 0,
        label: "acme".into(),
        amount: 100,
    };
    let created = Invoice::objects().create(inv).await.expect("create");

    Invoice::objects()
        .filter(invoice::ID.eq(created.id))
        .update_values(
            serde_json::json!({"amount": 250})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .expect("update");

    Invoice::objects()
        .filter(invoice::ID.eq(created.id))
        .delete()
        .await
        .expect("delete");

    let log = entries("invoice").await;
    let actions: Vec<&str> = log.iter().map(|(a, _, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "update", "delete"],
        "every typed write is recorded, in order; got: {log:?}",
    );

    // The update records the field that changed, from what to what — and ONLY it.
    let (_, _, changes) = &log[1];
    assert_eq!(changes["amount"]["from"], json!(100), "got: {changes}");
    assert_eq!(changes["amount"]["to"], json!(250), "got: {changes}");
    assert!(
        changes.get("label").is_none(),
        "an unchanged field must not be recorded — a diff full of unchanged \
         columns buries the one that moved; got: {changes}",
    );
}

/// The **dynamic** path — which is what admin and REST run on. Hooking only the
/// typed path would leave every REST/admin write unaudited with a green suite.
#[tokio::test]
async fn the_dynamic_path_is_audited() {
    let _g = lock().lock().await;
    boot().await;
    clear_audit().await;

    let meta = umbral::migrate::ModelMeta::for_::<Invoice>();
    let row = umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(json!({"label": "dyn", "amount": 10}).as_object().unwrap())
        .await
        .expect("dyn insert");
    let id = row["id"].as_i64().expect("id");

    umbral::orm::DynQuerySet::for_meta(&meta)
        .filter_in_i64("id", &[id])
        .update_json(json!({"amount": 99}).as_object().unwrap())
        .await
        .expect("dyn update");

    umbral::orm::DynQuerySet::for_meta(&meta)
        .filter_in_i64("id", &[id])
        .delete()
        .await
        .expect("dyn delete");

    let log = entries("invoice").await;
    let actions: Vec<&str> = log.iter().map(|(a, _, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "update", "delete"],
        "admin + REST run on DynQuerySet — those writes must be audited too; got: {log:?}",
    );
    let (_, _, changes) = &log[1];
    assert_eq!(changes["amount"]["from"], json!(10), "got: {changes}");
    assert_eq!(changes["amount"]["to"], json!(99), "got: {changes}");
}

/// A soft delete is an UPDATE on the wire. It must be logged as a **delete** —
/// otherwise the audit trail does not say what a human means by "deleted".
#[tokio::test]
async fn a_soft_delete_is_logged_as_a_delete() {
    let _g = lock().lock().await;
    boot().await;
    clear_audit().await;

    let note = Note::objects()
        .create(Note {
            id: 0,
            body: "hi".into(),
            deleted_at: None,
        })
        .await
        .expect("create");

    Note::objects()
        .filter(note::ID.eq(note.id))
        .delete() // soft
        .await
        .expect("soft delete");

    let log = entries("note").await;
    let actions: Vec<&str> = log.iter().map(|(a, _, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "delete"],
        "a soft delete must read as `delete`, not `update`; got: {log:?}",
    );
}

/// No authenticated caller → `actor = NULL`. A background job, the CLI, a
/// migration: we record that nobody was authenticated, never a guess.
#[tokio::test]
async fn an_unauthenticated_write_records_a_null_actor() {
    let _g = lock().lock().await;
    boot().await;
    clear_audit().await;

    Invoice::objects()
        .create(Invoice {
            id: 0,
            label: "cli".into(),
            amount: 1,
        })
        .await
        .expect("create");

    let log = entries("invoice").await;
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0].1, None,
        "no caller → NULL actor, not a fabricated one"
    );
}

/// A model without `#[umbral(audited)]` records nothing — and pays no SELECT.
#[tokio::test]
async fn an_unaudited_model_records_nothing() {
    let _g = lock().lock().await;
    boot().await;
    clear_audit().await;

    let p = Plain::objects()
        .create(Plain {
            id: 0,
            name: "x".into(),
        })
        .await
        .expect("create");
    Plain::objects()
        .filter(plain::ID.eq(p.id))
        .delete()
        .await
        .expect("delete");

    assert!(
        entries("plain").await.is_empty(),
        "audit is opt-in; an unaudited model must produce no rows",
    );
}
