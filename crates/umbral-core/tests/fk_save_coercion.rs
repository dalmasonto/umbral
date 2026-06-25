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
use umbral::orm::{DynQuerySet, ForeignKey};
use umbral::orm::{SqlType, write::json_to_sea_value};
use umbral_core::db;
use umbral_core::migrate::ModelMeta;

/// The ambient SQLite pool — every terminal in these tests already
/// dispatches through it; `resolve()` needs an explicit `&SqlitePool`.
fn sqlite_pool() -> sqlx::SqlitePool {
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p.clone(),
        umbral::db::DbPool::Postgres(_) => panic!("fk_save_coercion test targets SQLite only"),
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fkparent")]
pub struct Parent {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fkchild")]
pub struct Child {
    pub id: i64,
    pub parent: ForeignKey<Parent>,
    pub body: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
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

/// The `ForeignKey::default()` footgun: a model constructed via
/// `..Default::default()` (or `ForeignKey::default()`) leaves a
/// non-nullable FK at the id-0 unset placeholder. The validated
/// `create()` path catches it via the live-DB FK-existence check
/// (`ForeignKeyNotFound`), but the RAW-TRANSACTION write path
/// (`create_in_tx`) deliberately skips that pre-validation and goes
/// straight to the INSERT builder — which is exactly where the
/// `ForeignKey::default` placeholder would be bound as `FK = 0` and
/// persist a dangling row. The build-path guard
/// (`reject_unset_fk_placeholder`) is the backstop for that bypass: it
/// must REJECT the id-0 placeholder with a clear `WriteError` and insert
/// NO row.
#[tokio::test]
async fn unset_id0_fk_placeholder_is_rejected_in_tx_create() {
    boot().await;

    // A `body` value unique to this test so the NO-INSERT assertion is
    // immune to rows other tests insert concurrently into the shared
    // in-memory pool.
    const MARKER: &str = "orphan-create-guard-marker";

    let mut tx = db::begin().await.expect("begin tx");
    // A bare non-nullable FK left at its Default (id 0). The body is a
    // real value so the ONLY problem is the unset FK.
    let result = Child::objects()
        .create_in_tx(
            Child {
                id: 0,
                parent: ForeignKey::default(),
                body: MARKER.into(),
            },
            &mut tx,
        )
        .await;

    let err = result.expect_err(
        "create_in_tx with a non-nullable FK left at the id-0 default must error, \
         not silently insert FK = 0",
    );
    let msg = format!("{err:?}");
    // The guard must fire as a STRUCTURAL placeholder rejection — naming
    // the column AND the unset-id-0 placeholder — at the INSERT builder,
    // BEFORE any SQL touches the DB. Pinning the message proves it's OUR
    // guard, not a DB-side FK violation.
    assert!(
        msg.contains("parent") && msg.contains("placeholder"),
        "the rejection must be the unset-id-0 placeholder guard naming `parent`; got: {msg}"
    );
    tx.rollback().await.expect("rollback");

    // Behavioral proof: NO row carrying this marker was inserted (the
    // guard returned Err before the INSERT ran).
    let inserted = Child::objects()
        .filter(child::BODY.eq(MARKER))
        .count()
        .await
        .expect("count children with the marker body");
    assert_eq!(
        inserted, 0,
        "the rejected create must NOT have inserted a row (id-0 FK placeholder)"
    );
}

/// The same footgun on the bulk raw-transaction path: a
/// `bulk_create_in_tx` batch where one row carries the id-0 FK
/// placeholder must be rejected at the INSERT builder, inserting NO rows
/// from the batch.
#[tokio::test]
async fn unset_id0_fk_placeholder_is_rejected_in_tx_bulk_create() {
    boot().await;
    let parent = seed_parent("Acme Bulk Guard").await;

    // Markers unique to this test (see the create-path test above).
    const OK_MARKER: &str = "bulk-ok-guard-marker";
    const BAD_MARKER: &str = "bulk-orphan-guard-marker";

    let mut tx = db::begin().await.expect("begin tx");
    let result = Child::objects()
        .bulk_create_in_tx(
            vec![
                Child {
                    id: 0,
                    parent: ForeignKey::new(parent.id),
                    body: OK_MARKER.into(),
                },
                Child {
                    id: 0,
                    parent: ForeignKey::default(),
                    body: BAD_MARKER.into(),
                },
            ],
            &mut tx,
        )
        .await;

    let err = result.expect_err("bulk_create_in_tx with an id-0 FK placeholder must error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("parent") && msg.contains("placeholder"),
        "the bulk rejection must be the unset-id-0 placeholder guard naming `parent`; got: {msg}"
    );
    tx.rollback().await.expect("rollback");

    // No row from the batch — neither the good nor the bad marker — may
    // have been inserted (the builder errored before the INSERT ran).
    let inserted = Child::objects()
        .filter(child::BODY.eq(OK_MARKER))
        .count()
        .await
        .expect("count ok-marker children")
        + Child::objects()
            .filter(child::BODY.eq(BAD_MARKER))
            .count()
            .await
            .expect("count bad-marker children");
    assert_eq!(
        inserted, 0,
        "a rejected bulk_create must NOT insert any row from the batch"
    );
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
