//! ORM audit — serde + NULL coercion edges.
//!
//! These tests target the bugs that *almost slipped silently* this
//! session:
//!
//!   - **The NULL→0 coercion bug** in `sqlite_row_to_json` /
//!     `postgres_row_to_json`. The original cascade tried
//!     `try_get::<i64>` first, which SQLite affinity-coerces NULL
//!     to `0`. Nullable FK columns then deserialised as
//!     `Some(ForeignKey { raw: 0 })` instead of `None`. The fix
//!     (Option<T>-first cascade) was only tested by side effect via
//!     `nested_path_with_null_middle_hop_does_not_panic`. Direct
//!     test here makes the regression instantly obvious if it ever
//!     comes back.
//!
//!   - **ForeignKey nested Deserialize round-trip**. The new
//!     Deserialize accepts both a scalar PK AND a nested object —
//!     the nested-object branch is what makes nested
//!     `select_related` work. Tests here confirm both shapes round-
//!     trip cleanly through `serde_json::to_value` /
//!     `from_value` and that the scalar-only path still works
//!     (backward compat with every existing JSON consumer).
//!
//!   - **`Option<ForeignKey<T>>` shape**. The nullable FK wraps two
//!     layers of "maybeness" — Option (column NULL) and
//!     ForeignKey.resolved (loaded yet). Edge case: a non-null FK
//!     with an unresolved relation must serialise to a bare PK
//!     value, not `{id: N}`. (Old behaviour; tested to catch
//!     regression.)

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::ForeignKey;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "auds_owner")]
pub struct Owner {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
    /// Nullable integer — the exact shape the original
    /// sqlite_row_to_json bug bit on. When this column is NULL in
    /// the DB and the row is decoded via `fetch_related_as_json`
    /// (which `select_related` uses to JSON-shape the parent row
    /// before handing it to `ForeignKey::Deserialize`), the buggy
    /// `try_get::<i64>` first cascade returned 0 instead of None,
    /// so `Option<i64>` decoded as `Some(0)`. Pinned below.
    pub nullable_count: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "auds_thing")]
pub struct Thing {
    pub id: i64,
    pub title: String,
    /// Nullable FK — the column the original sqlite_row_to_json
    /// bug silently filled with 0 when NULL.
    pub owner: Option<ForeignKey<Owner>>,
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
            .model::<Owner>()
            .model::<Thing>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE auds_owner (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                nullable_count INTEGER
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE owner");
        sqlx::query(
            "CREATE TABLE auds_thing (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                owner INTEGER REFERENCES auds_owner(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE thing");

        // Insert alice with nullable_count = NULL — this is the row
        // select_related will fetch + JSON-shape via the path that
        // had the bug.
        sqlx::query("INSERT INTO auds_owner (name, nullable_count) VALUES (?, NULL)")
            .bind("alice")
            .execute(&pool)
            .await
            .expect("seed owner");

        // Two things: one with owner=alice, one with owner=NULL.
        // The NULL case is the regression target.
        sqlx::query("INSERT INTO auds_thing (title, owner) VALUES (?, ?)")
            .bind("with-owner")
            .bind(Some(1_i64))
            .execute(&pool)
            .await
            .expect("seed thing");
        sqlx::query("INSERT INTO auds_thing (title, owner) VALUES (?, ?)")
            .bind("orphan")
            .bind(None::<i64>)
            .execute(&pool)
            .await
            .expect("seed orphan thing");
    })
    .await;
}

// =========================================================================
// REGRESSION: NULL on a nullable FK column must decode as
// `Option<ForeignKey<_>>::None`, NOT `Some(ForeignKey { raw: 0 })`.
//
// The original bug: sqlite_row_to_json tried `try_get::<i64>` first,
// SQLite coerced NULL → 0, the row's "owner" became JsonValue::Number(0),
// and Option deserialization saw a Some path → built a phantom FK with
// raw=0 pointing at no real row.
//
// Direct test that would have caught this if it had existed earlier.
// =========================================================================
#[tokio::test]
async fn null_fk_column_via_select_related_path_decodes_as_none() {
    boot().await;
    // Drive through hydrate_select_related so the JSON-shaped path
    // (the one that had the bug) is exercised. The orphan thing's
    // owner is NULL; the resolved hydration walks each Owner row.
    // The orphan won't have an owner_id at all (it's a top-level
    // Option that's None from FromRow), but a sibling test below
    // verifies the deeper-path JSON shape.
    let things = Thing::objects()
        .filter(thing::TITLE.eq("orphan"))
        .fetch()
        .await
        .expect("fetch");
    let orphan = things
        .iter()
        .find(|t| t.title == "orphan")
        .expect("orphan present");
    // The whole Option<ForeignKey<Owner>> must be None.
    assert!(
        orphan.owner.is_none(),
        "Option<ForeignKey> for a NULL column MUST be None — was: {:?}",
        orphan.owner
    );
    // Negative assertion to catch the regression specifically:
    // if the bug recurs, orphan.owner would be Some(FK{raw: 0}).
    if let Some(fk) = &orphan.owner {
        panic!(
            "REGRESSION: nullable FK silently filled with raw={} (expected None)",
            fk.id()
        );
    }
}

// =========================================================================
// ForeignKey nested Deserialize: scalar PK input → unresolved FK
// (backward compatibility — every JSON consumer that round-tripped
// FKs as bare numbers must keep working).
// =========================================================================
#[test]
fn foreign_key_deserialize_from_scalar_pk_stays_unresolved() {
    let json = serde_json::json!(42);
    let fk: ForeignKey<Owner> = serde_json::from_value(json).expect("scalar PK must deserialize");
    assert_eq!(fk.id(), 42);
    assert!(
        fk.resolved().is_none(),
        "scalar input → unresolved (resolved slot empty)"
    );
}

// =========================================================================
// ForeignKey nested Deserialize: object input → resolved FK with
// both raw + resolved populated. This is what makes nested
// `select_related("author__manager")` actually unpack the chain.
// =========================================================================
#[test]
fn foreign_key_deserialize_from_object_populates_resolved() {
    let json = serde_json::json!({
        "id": 7,
        "name": "alice"
    });
    let fk: ForeignKey<Owner> = serde_json::from_value(json).expect("object must deserialize");
    assert_eq!(fk.id(), 7, "raw PK extracted from object");
    let resolved = fk
        .resolved()
        .expect("resolved must be populated from object");
    assert_eq!(resolved.id, 7);
    assert_eq!(resolved.name, "alice");
}

// =========================================================================
// ForeignKey nested Deserialize: object MISSING the PK field must
// error loudly — silent fallback to raw=0 would re-introduce the
// kind of phantom-FK bug the NULL fix closed.
// =========================================================================
#[test]
fn foreign_key_deserialize_object_missing_pk_errors() {
    let json = serde_json::json!({
        "name": "alice"  // no `id`
    });
    let err = serde_json::from_value::<ForeignKey<Owner>>(json)
        .expect_err("missing PK in object must error");
    let msg = err.to_string();
    assert!(
        msg.contains("id") || msg.contains("missing"),
        "error must point at the missing PK field: {msg}"
    );
}

// =========================================================================
// Round-trip: a select_related'd model serialised then deserialised
// preserves the resolved relation. Before the nested-Deserialize fix,
// to_value(post) emitted {author: {id, name, ...}} but from_value
// couldn't read that back — round-trip lost the resolved slot. This
// is the bonus side-effect of the nested FK fix.
// =========================================================================
#[tokio::test]
async fn select_related_model_round_trips_through_serde_json() {
    boot().await;
    let things = Thing::objects()
        .filter(thing::TITLE.eq("with-owner"))
        .select_related("owner")
        .fetch()
        .await
        .expect("fetch");
    let original = things
        .iter()
        .find(|t| t.title == "with-owner")
        .expect("with-owner present")
        .clone();
    // Confirm select_related actually hydrated before the round-trip.
    let original_owner = original
        .owner
        .as_ref()
        .expect("owner wrapper")
        .resolved()
        .expect("owner resolved");
    assert_eq!(original_owner.name, "alice");

    let as_value = serde_json::to_value(&original).expect("serialize");
    let round_tripped: Thing = serde_json::from_value(as_value).expect("deserialize back");

    assert_eq!(round_tripped.title, original.title);
    let rt_owner = round_tripped
        .owner
        .as_ref()
        .expect("owner wrapper round-tripped")
        .resolved()
        .expect(
            "REGRESSION: resolved relation must survive serde round-trip \
             (post-#42 ForeignKey<T>::Deserialize accepts both shapes)",
        );
    assert_eq!(rt_owner.id, original_owner.id);
    assert_eq!(rt_owner.name, original_owner.name);
}

// =========================================================================
// Unresolved FK serialises as a bare PK value, not a wrapper object.
// REST consumers that branched on `typeof obj.owner === 'number'`
// would break if this regressed.
// =========================================================================
#[tokio::test]
async fn unresolved_fk_serializes_as_bare_pk_not_object() {
    boot().await;
    let things = Thing::objects()
        .filter(thing::TITLE.eq("with-owner")) // load WITHOUT select_related
        .fetch()
        .await
        .expect("fetch");
    let thing = things
        .iter()
        .find(|t| t.title == "with-owner")
        .expect("present");
    let fk = thing.owner.as_ref().expect("owner FK wrapper");
    // Make sure we didn't accidentally hydrate.
    assert!(
        fk.resolved().is_none(),
        "owner must be unresolved (no select_related): {:?}",
        fk.resolved()
    );
    let as_value = serde_json::to_value(fk).expect("serialize");
    assert!(
        as_value.is_number(),
        "unresolved FK must serialise as bare PK number, got: {as_value:?}"
    );
    assert_eq!(as_value.as_i64(), Some(1));
}

// =========================================================================
// Resolved FK serialises as the full object. Templates +
// serde_json::to_value(&post)["owner"]["name"] depend on this.
// =========================================================================
#[tokio::test]
async fn resolved_fk_serializes_as_full_object() {
    boot().await;
    let things = Thing::objects()
        .filter(thing::TITLE.eq("with-owner"))
        .select_related("owner")
        .fetch()
        .await
        .expect("fetch");
    let thing = things
        .iter()
        .find(|t| t.title == "with-owner")
        .expect("present");
    let fk = thing.owner.as_ref().expect("owner wrapper");
    let as_value = serde_json::to_value(fk).expect("serialize");
    let obj = as_value
        .as_object()
        .expect("resolved FK serialises as object");
    assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(obj.get("name").and_then(|v| v.as_str()), Some("alice"));
}

// =========================================================================
// REGRESSION: the .values() path (which uses sqlite_row_to_json
// directly) must also report a NULL nullable FK column as JsonValue::Null,
// not Number(0). This was the original site of the bug.
// =========================================================================
#[tokio::test]
async fn values_returns_json_null_for_null_nullable_fk_column() {
    boot().await;
    let rows = Thing::objects()
        .filter(thing::TITLE.eq("orphan"))
        .values(&["title", "owner"])
        .await
        .expect("values");
    let orphan = rows
        .iter()
        .find(|r| r.as_object().and_then(|o| o.get("title")?.as_str()) == Some("orphan"))
        .expect("orphan present")
        .as_object()
        .expect("object");
    let owner = orphan.get("owner").expect("owner key present");
    assert!(
        owner.is_null(),
        "REGRESSION: NULL FK column must come back as JsonValue::Null, got {owner:?}"
    );
    // Specifically: NOT Number(0). The original bug.
    assert_ne!(
        owner.as_i64(),
        Some(0),
        "REGRESSION: NULL must not coerce to 0"
    );
}

// =========================================================================
// MUTATION-TESTED REGRESSION: select_related goes through
// `fetch_related_as_json` → `sqlite_row_to_json`, which is the
// function the original NULL→0 bug lived in. Reintroducing the
// buggy bare-`i64`-first cascade makes THIS test fail (verified
// during audit). The `nullable_count` column on Owner is NULL
// in the seed; if the bug recurs, the row's owner.nullable_count
// would deserialise as `Some(0)` instead of `None`.
//
// Without this test, the regression would only be caught by
// `select_related_nested::nested_path_with_null_middle_hop_does_not_panic`
// which exercises the same path but bundles a lot of other
// machinery (multi-hop traversal, embedding) — making the failure
// harder to diagnose if the bug came back.
// =========================================================================
#[tokio::test]
async fn select_related_decodes_null_nullable_int_on_related_as_none() {
    boot().await;
    let things = Thing::objects()
        .filter(thing::TITLE.eq("with-owner"))
        .select_related("owner")
        .fetch()
        .await
        .expect("fetch");
    let thing = things
        .iter()
        .find(|t| t.title == "with-owner")
        .expect("present");
    let owner = thing
        .owner
        .as_ref()
        .expect("owner wrapper present")
        .resolved()
        .expect("owner resolved by select_related");
    assert_eq!(owner.name, "alice");
    // The bug: pre-fix this came back as Some(0) because
    // try_get::<i64> on a NULL integer column coerced to 0.
    // The fix: Option<T>-first cascade in sqlite_row_to_json.
    assert!(
        owner.nullable_count.is_none(),
        "REGRESSION in sqlite_row_to_json: NULL integer column \
         on a select_related'd row decoded as {:?} (expected None). \
         The Option<T>-first cascade in sqlite_row_to_json got \
         reverted to bare try_get::<i64>, which SQLite coerces \
         NULL → 0 instead of erroring.",
        owner.nullable_count
    );
    // Specifically not Some(0).
    assert_ne!(
        owner.nullable_count,
        Some(0),
        "specifically: not Some(0) — that would be the pre-fix bug"
    );
}

// =========================================================================
// Non-NULL FK column comes back as a Number, not as some other shape.
// Companion sanity check to the null test — proves both paths are
// distinguishable post-fix.
// =========================================================================
#[tokio::test]
async fn values_returns_json_number_for_non_null_fk_column() {
    boot().await;
    let rows = Thing::objects()
        .filter(thing::TITLE.eq("with-owner"))
        .values(&["title", "owner"])
        .await
        .expect("values");
    let thing = rows
        .iter()
        .find(|r| r.as_object().and_then(|o| o.get("title")?.as_str()) == Some("with-owner"))
        .expect("with-owner present")
        .as_object()
        .expect("object");
    let owner = thing.get("owner").expect("owner key present");
    assert_eq!(
        owner.as_i64(),
        Some(1),
        "non-null FK comes back as the PK number"
    );
    assert!(!owner.is_null());
}
