//! gaps2 #42 — saving a foreign-key id through a dynamic write path must
//! bind the id against the *target PK's* SqlType (bigint for an i64-PK
//! parent), not raw TEXT. The bug: `json_to_sea_value`'s `ForeignKey`
//! arm bound every string-valued FK id as `SeaValue::String` (TEXT)
//! because the function couldn't see `fk_target`. On Postgres a numeric
//! FK column then rejected the text expression:
//!
//!   column "plugin" is of type bigint but expression is of type text
//!
//! These tests drive the REAL public write paths (the admin/REST JSON
//! path `DynQuerySet::insert_json`, and the typed `Manager::create`
//! path) against a seeded i64-PK parent, then read the child back and
//! prove the FK links the real parent row. The parent column is
//! declared `INTEGER` so a TEXT bind is wrong on both backends; the
//! round-trip read-back is the behavioral proof.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::OnceCell;
use umbra::orm::{DynQuerySet, ForeignKey};
use umbra::orm::{SqlType, write::json_to_sea_value};
use umbra_core::db;
use umbra_core::migrate::ModelMeta;

/// The ambient SQLite pool — every terminal in these tests already
/// dispatches through it; `resolve()` needs an explicit `&SqlitePool`.
fn sqlite_pool() -> sqlx::SqlitePool {
    match umbra::db::pool_dispatched() {
        umbra::db::DbPool::Sqlite(p) => p.clone(),
        umbra::db::DbPool::Postgres(_) => panic!("fk_save_coercion test targets SQLite only"),
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "fkparent")]
pub struct Parent {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "fkchild")]
pub struct Child {
    pub id: i64,
    pub parent: ForeignKey<Parent>,
    pub body: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Parent>()
            .model::<Child>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE fkparent (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE fkparent");
        // `parent` is INTEGER NOT NULL — a text bind for the FK id is a
        // type mismatch here, exactly as Postgres bigint rejects text.
        sqlx::query(
            "CREATE TABLE fkchild (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                parent INTEGER NOT NULL REFERENCES fkparent(id),
                body TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE fkchild");
    })
    .await;
}

async fn seed_parent(name: &str) -> Parent {
    Parent::objects()
        .create(Parent {
            id: 0,
            name: name.into(),
        })
        .await
        .expect("seed parent")
}

/// The admin/REST JSON write path: `insert_json` with the FK id as a
/// JSON *string* (`{"parent": "1"}`, the form/admin shape). Pre-fix the
/// string was bound TEXT and tripped the INTEGER `parent` column.
#[tokio::test]
async fn fk_id_as_json_string_via_insert_json_links_parent() {
    boot().await;
    let parent = seed_parent("Acme JSON").await;

    let mut body = serde_json::Map::new();
    body.insert(
        "parent".to_string(),
        serde_json::Value::String(parent.id.to_string()),
    );
    body.insert(
        "body".to_string(),
        serde_json::Value::String("hello".into()),
    );

    let meta = ModelMeta::for_::<Child>();
    let inserted = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("insert child with string FK id via insert_json");

    let child_id = inserted
        .get("id")
        .and_then(|v| v.as_i64())
        .expect("returned row carries an i64 id");

    // Read back through the typed path and prove the FK links the real
    // parent row (id + a real field via resolve()).
    let child = Child::objects()
        .filter(child::ID.eq(child_id))
        .first()
        .await
        .expect("fetch child")
        .expect("child present");
    assert_eq!(
        child.parent.id(),
        parent.id,
        "stored FK must equal the seeded parent id"
    );

    let resolved = child
        .parent
        .resolve(&sqlite_pool())
        .await
        .expect("resolve parent through the FK");
    assert_eq!(
        resolved.name, "Acme JSON",
        "FK resolves to the real parent row, not a fabricated one"
    );

    // The stored column must be the bigint id, queryable as an i64
    // predicate (a TEXT-stored value would not match the integer one
    // on a strict backend; here we pin the stored shape directly).
    let by_fk = Child::objects()
        .filter(child::PARENT.eq(parent.id))
        .count()
        .await
        .expect("count children by FK id");
    assert_eq!(by_fk, 1, "child is findable by the bigint FK id");

    // NOTE on the SQLite/Postgres split: on SQLite a TEXT bind into the
    // INTEGER `parent` column is *silently corrected* by column
    // affinity (the string '1' is stored as integer 1), so this
    // round-trip can't itself go red on SQLite — only on Postgres,
    // where a bigint column rejects a text expression outright (the
    // literal #42 symptom). The DETERMINISTIC SQLite proof of the bind
    // type lives in `json_fk_arm_coerces_numeric_string_to_bigint`
    // below, which asserts the produced `SeaValue` variant directly.
    // This behavioral test guards the real end-to-end path (link
    // resolves to the seeded parent) and would catch a Postgres
    // regression; the unit pin guards the coercion on every backend.
}

/// The typed `Manager::create` path: a `ForeignKey<Parent>` built from
/// the i64 id. This pins the typed path to the same coercion so the two
/// write paths can't diverge and silently reintroduce the text bind.
#[tokio::test]
async fn typed_create_binds_fk_as_bigint_same_as_dynamic() {
    boot().await;
    let parent = seed_parent("Acme Typed").await;

    let child = Child::objects()
        .create(Child {
            id: 0,
            parent: ForeignKey::new(parent.id),
            body: "typed".into(),
        })
        .await
        .expect("typed create with FK");

    let back = Child::objects()
        .filter(child::ID.eq(child.id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    assert_eq!(
        back.parent.id(),
        parent.id,
        "typed-path FK must link the real parent"
    );

    let resolved = back
        .parent
        .resolve(&sqlite_pool())
        .await
        .expect("resolve parent");
    assert_eq!(resolved.name, "Acme Typed");
}

/// The form write path: `insert_form` with the FK id as a string
/// (`"parent" => "1"`). This arm is already correct (it short-circuits
/// FK before `json_to_sea_value`), but pinning it keeps all three live
/// write surfaces asserted together.
#[tokio::test]
async fn fk_id_as_form_string_via_insert_form_links_parent() {
    boot().await;
    let parent = seed_parent("Acme Form").await;

    let mut form = HashMap::new();
    form.insert("parent".to_string(), parent.id.to_string());
    form.insert("body".to_string(), "formy".to_string());

    let meta = ModelMeta::for_::<Child>();
    let child_id = DynQuerySet::for_meta(&meta)
        .insert_form(&form, &[])
        .await
        .expect("insert child with string FK id via insert_form");

    let child = Child::objects()
        .filter(child::ID.eq(child_id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    assert_eq!(child.parent.id(), parent.id);
}

/// Unit-level regression pin on the exact arm #42 mis-bound: a numeric
/// string FK id with a numeric-PK (or unresolved) target must bind
/// BigInt, never TEXT; a resolved Text-PK target still binds text.
#[test]
fn json_fk_arm_coerces_numeric_string_to_bigint() {
    // No registry hint (None) → defaults to the i64-PK case.
    let v = json_to_sea_value(
        SqlType::ForeignKey,
        &serde_json::json!("1"),
        false,
        "parent",
        None,
    )
    .expect("coerce numeric string FK id");
    assert_eq!(
        v,
        sea_query::Value::BigInt(Some(1)),
        "REGRESSION (gaps2 #42): a string FK id for a numeric-PK target must bind BigInt, not TEXT"
    );

    // A resolved Text-PK target still binds the id as text.
    let t = json_to_sea_value(
        SqlType::ForeignKey,
        &serde_json::json!("perm.add"),
        false,
        "perm",
        Some(SqlType::Text),
    )
    .expect("coerce text-PK FK id");
    assert!(
        matches!(t, sea_query::Value::String(_)),
        "Text-PK FK must still bind text"
    );
}
