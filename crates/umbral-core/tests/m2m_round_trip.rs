//! End-to-end M2M round-trip against a real in-memory SQLite database.
//!
//! Validates BUG-16 phase 2's claim that `M2M<T, P>` works with any
//! supported PK combination — specifically the motivating shape from
//! `umbral-permissions`: a parent with `id: i64` joined to a child with
//! `codename: String` (composite-codename PK). Without phase 2 the
//! junction's `child_id INTEGER` would reject the string codename at
//! insert time.
//!
//! What's covered:
//!
//! 1. The migration engine emits `CreateM2MTable` with the right
//!    per-side PK type and applies the DDL — `child_id TEXT NOT NULL`
//!    on the SQLite-rendered junction.
//! 2. `M2M::add(&child)` writes the junction row with the typed PKs.
//! 3. `M2M::fetch()` returns the linked child by following the
//!    junction join.
//! 4. `M2M::remove(&child)` removes the junction row.
//! 5. `M2M::set(&[&c1, &c2])` replaces the entire set in one
//!    transaction (clear + re-add).
//! 6. `set_m2m_parent_ids` correctly seeds `parent_id` +
//!    `junction_table` on rows fetched via `Manager::filter().fetch()`
//!    — the second `.add()` against a freshly-loaded row hits the
//!    same junction as the in-memory copy.
//!
//! The integration deliberately uses a synthetic `RoundTripGroup` /
//! `RoundTripPermission` pair rather than the real umbral-permissions
//! models — keeps the test independent of the permissions plugin's
//! own boot fixtures and lets us assert exact junction-table names
//! without worrying about the plugin's standard-permission seed.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::orm::M2M;

/// Parent model: `i64` PK. Identical PK shape to `permissions_group`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "m2mtest")]
pub struct RoundTripGroup {
    pub id: i64,
    pub name: String,
    /// The M2M field — the macro skips it from FIELDS, the diff
    /// engine emits a `CreateM2MTable` for `m2mtest_m2m_test_group_perms`
    /// (snake-case of the struct + field name), and the hydrate
    /// hook seeds parent_id + junction_table on every loaded row.
    #[sqlx(skip)]
    #[serde(skip)]
    pub perms: M2M<RoundTripPermission>,
}

/// Child model: `String` PK (the BUG-16 motivating case — the same
/// `codename: String` shape `permissions_permission` uses).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "m2mtest")]
pub struct RoundTripPermission {
    #[umbral(primary_key, string, max_length = 150)]
    pub codename: String,
    pub label: String,
}

/// One-shot boot. The migration engine runs once per process: build
/// the App, then `make_in` + `run_in` against a tempdir so the
/// `m2mtest_*` tables and the auto-generated junction get created.
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        // File-backed SQLite in a tempdir, not `sqlite::memory:` —
        // every connection sqlx hands out from a `:memory:` pool sees
        // its own private database, so a schema created on the boot
        // connection vanishes from the test connections. The
        // permissions plugin's own integration test uses the same
        // tempdir pattern for exactly this reason.
        let tmp = tempfile::tempdir().expect("create db tempdir");
        let db_path = tmp.path().join("m2m_round_trip.sqlite");
        // Leak the TempDir so it stays alive for the process lifetime
        // — when the OnceCell init function returns, the local `tmp`
        // would Drop and unlink the file mid-test.
        std::mem::forget(tmp);
        let opts = SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .expect("sqlite file pool should connect");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<RoundTripGroup>()
            .model::<RoundTripPermission>()
            .build()
            .expect("App::build should succeed for the M2M round-trip test");

        // Run migrate against a private tempdir so the schema (parent
        // tables + the auto-generated junction) actually exists.
        let migration_tmp = tempfile::tempdir().expect("create migration tempdir");
        let migration_path = migration_tmp.path().to_path_buf();
        std::mem::forget(migration_tmp);
        umbral::migrate::make_in(&migration_path)
            .await
            .expect("make_in should emit the m2mtest seed migration");
        umbral::migrate::run_in(&migration_path)
            .await
            .expect("run_in should apply the m2mtest seed migration");
    })
    .await;
}

/// Direct-pool helper for the table-existence assertion below — the
/// junction isn't a registered Model, so it doesn't have a Manager.
async fn pool_handle() -> SqlitePool {
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p.clone(),
        umbral::db::DbPool::Postgres(_) => panic!("M2M round-trip test targets SQLite only"),
    }
}

/// The junction-table name the macro derives:
/// `<parent_table>_<field_name>` → `m2mtest_round_trip_group_perms`.
/// (`m2mtest_` is the plugin prefix; `round_trip_group` is the
/// snake-cased parent struct name; `perms` is the field ident.)
/// Pinning the constant in one spot makes the rest of the asserts
/// read straightforwardly.
const JUNCTION_TABLE: &str = "m2mtest_round_trip_group_perms";

/// Pin the junction-table schema first — the rest of the round-trip is
/// meaningless if the migration engine emitted the wrong column types.
#[tokio::test]
async fn junction_table_uses_typed_pk_columns() {
    boot().await;
    let pool = pool_handle().await;

    let row: (String,) = sqlx::query_as("SELECT sql FROM sqlite_master WHERE name = ?")
        .bind(JUNCTION_TABLE)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("expected sqlite_master to carry `{JUNCTION_TABLE}`: {e}"));
    let ddl = row.0;
    assert!(
        ddl.contains("\"parent_id\" INTEGER NOT NULL")
            && ddl.contains("\"child_id\" TEXT NOT NULL"),
        "junction DDL must respect the parent (i64) / child (String) PK shapes; \
         BUG-16 phase 2's reason to exist. Got: {ddl}",
    );
}

#[tokio::test]
async fn m2m_add_then_fetch_round_trips_with_string_child_pk() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.publish_first".to_string(),
            label: "Can publish first".to_string(),
        })
        .await
        .expect("create permission");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_one".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    // `create()` round-trips through the database, so the returned
    // row went through `set_m2m_parent_ids` — parent_id + junction
    // table should both be populated.
    assert_eq!(group.perms.parent_id().copied(), Some(group.id));
    assert_eq!(group.perms.junction_table(), Some(JUNCTION_TABLE));

    group.perms.add(&perm).await.expect("m2m add");

    let fetched = group.perms.fetch().await.expect("m2m fetch");
    assert_eq!(
        fetched.len(),
        1,
        "expected one linked permission; got {fetched:?}"
    );
    assert_eq!(fetched[0].codename, perm.codename);
}

#[tokio::test]
async fn m2m_add_is_idempotent_on_conflict() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.publish_idem".to_string(),
            label: "idem".to_string(),
        })
        .await
        .expect("create permission");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_idem".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    group.perms.add(&perm).await.expect("first add");
    group
        .perms
        .add(&perm)
        .await
        .expect("second add must not error (ON CONFLICT DO NOTHING)");
    let fetched = group.perms.fetch().await.expect("m2m fetch");
    assert_eq!(
        fetched.len(),
        1,
        "duplicate adds should collapse to one row"
    );
}

#[tokio::test]
async fn m2m_remove_drops_the_junction_row() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.publish_remove".to_string(),
            label: "removable".to_string(),
        })
        .await
        .expect("create permission");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_remove".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    group.perms.add(&perm).await.expect("add");
    group.perms.remove(&perm).await.expect("remove");
    let fetched = group.perms.fetch().await.expect("fetch after remove");
    assert!(fetched.is_empty(), "remove() should drop the junction row");
}

#[tokio::test]
async fn m2m_set_replaces_the_full_set_atomically() {
    boot().await;

    let perm_a = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.set_a".to_string(),
            label: "A".to_string(),
        })
        .await
        .expect("create A");
    let perm_b = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.set_b".to_string(),
            label: "B".to_string(),
        })
        .await
        .expect("create B");
    let perm_c = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.set_c".to_string(),
            label: "C".to_string(),
        })
        .await
        .expect("create C");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_set".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    // Start with [A, B].
    group
        .perms
        .set(&[&perm_a, &perm_b])
        .await
        .expect("set initial");
    let mut fetched = group.perms.fetch().await.expect("fetch initial");
    fetched.sort_by(|x, y| x.codename.cmp(&y.codename));
    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].codename, perm_a.codename);
    assert_eq!(fetched[1].codename, perm_b.codename);

    // Replace with [B, C] — A should drop, C should appear.
    group
        .perms
        .set(&[&perm_b, &perm_c])
        .await
        .expect("set replace");
    let mut fetched = group.perms.fetch().await.expect("fetch after set");
    fetched.sort_by(|x, y| x.codename.cmp(&y.codename));
    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].codename, perm_b.codename);
    assert_eq!(fetched[1].codename, perm_c.codename);
}

#[tokio::test]
async fn m2m_clear_removes_every_junction_row_for_the_parent() {
    boot().await;

    let perm_x = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.clear_x".to_string(),
            label: "X".to_string(),
        })
        .await
        .expect("create X");
    let perm_y = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.clear_y".to_string(),
            label: "Y".to_string(),
        })
        .await
        .expect("create Y");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_clear".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    group.perms.add(&perm_x).await.unwrap();
    group.perms.add(&perm_y).await.unwrap();
    let removed = group.perms.clear().await.expect("clear");
    assert_eq!(removed, 2, "clear() should report rows removed");
    let fetched = group.perms.fetch().await.expect("fetch after clear");
    assert!(fetched.is_empty(), "clear() must remove every junction row");
}

// =========================================================================
// BUG-16 phase 3 follow-up: typed bulk-across-parents helpers the macro
// emits on the parent struct. Developers never spell the junction-
// table name; the `<field>_contains_any` / `<field>_union_for` methods
// derive it from the parent's table + field ident at expand time.
// =========================================================================

#[tokio::test]
async fn macro_emits_typed_junction_accessor_const() {
    // The escape hatch — the auto-derived junction name is exposed
    // via `<Parent>::<field>_junction_table()`. Application code
    // shouldn't need it, but raw-SQL admin pickers and the like do.
    assert_eq!(
        RoundTripGroup::perms_junction_table(),
        JUNCTION_TABLE,
        "the macro-emitted accessor must agree with the migration \
         engine's `<parent_table>_<field_name>` derivation",
    );
}

#[tokio::test]
async fn perms_contains_any_returns_true_when_any_parent_has_the_child() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.any_holds_yes".to_string(),
            label: "yes".to_string(),
        })
        .await
        .expect("create permission");

    let g1 = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "any_holds_g1".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("g1");
    let g2 = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "any_holds_g2".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("g2");

    // Only g2 holds the perm. perms_contains_any across [g1.id, g2.id]
    // should return true regardless.
    g2.perms.add(&perm).await.expect("add to g2");

    let holds = RoundTripGroup::perms_contains_any(&[g1.id, g2.id], perm.codename.clone())
        .await
        .expect("perms_contains_any");
    assert!(
        holds,
        "perms_contains_any across both groups must find the relation on g2"
    );
}

#[tokio::test]
async fn perms_contains_any_returns_false_when_no_parent_has_the_child() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.any_holds_no".to_string(),
            label: "no".to_string(),
        })
        .await
        .expect("create permission");

    let g = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "any_holds_empty".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("g");

    // No add — the junction is empty for this group.
    let holds = RoundTripGroup::perms_contains_any(&[g.id], perm.codename.clone())
        .await
        .expect("perms_contains_any");
    assert!(
        !holds,
        "perms_contains_any must return false when no junction row matches"
    );
}

#[tokio::test]
async fn perms_contains_any_with_empty_parent_slice_short_circuits_false() {
    boot().await;
    let holds = RoundTripGroup::perms_contains_any(&[], "anything".to_string())
        .await
        .expect("perms_contains_any with empty parents");
    assert!(
        !holds,
        "empty parent slice should short-circuit to Ok(false)"
    );
}

#[tokio::test]
async fn perms_union_for_returns_distinct_union_across_parents() {
    boot().await;

    let perm_a = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.holders_a".to_string(),
            label: "a".to_string(),
        })
        .await
        .expect("create A");
    let perm_b = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.holders_b".to_string(),
            label: "b".to_string(),
        })
        .await
        .expect("create B");
    let perm_c = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.holders_c".to_string(),
            label: "c".to_string(),
        })
        .await
        .expect("create C");

    let g1 = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "holders_g1".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("g1");
    let g2 = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "holders_g2".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("g2");

    // g1: {A, B}, g2: {B, C}. Union is {A, B, C}; B should be de-duped.
    g1.perms.add(&perm_a).await.unwrap();
    g1.perms.add(&perm_b).await.unwrap();
    g2.perms.add(&perm_b).await.unwrap();
    g2.perms.add(&perm_c).await.unwrap();

    let mut all = RoundTripGroup::perms_union_for(&[g1.id, g2.id])
        .await
        .expect("perms_union_for");
    all.sort();
    assert_eq!(
        all,
        vec![
            perm_a.codename.clone(),
            perm_b.codename.clone(),
            perm_c.codename.clone(),
        ],
        "perms_union_for must return the DISTINCT union; B appears once not twice",
    );
}

#[tokio::test]
async fn perms_union_for_with_empty_parent_slice_returns_empty() {
    boot().await;
    let out = RoundTripGroup::perms_union_for(&[])
        .await
        .expect("perms_union_for with empty parents");
    assert!(
        out.is_empty(),
        "empty parent slice should short-circuit to Ok(Vec::new())"
    );
}

// =========================================================================
// BUG-16 admin: `set_junction_dynamic` — the call path the admin form
// handler uses, with typed PKs but no typed `T` wrapper.
// =========================================================================

#[tokio::test]
async fn set_junction_dynamic_replaces_selection_with_typed_values() {
    boot().await;

    let perm_a = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.dyn_a".to_string(),
            label: "A".to_string(),
        })
        .await
        .expect("create A");
    let perm_b = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.dyn_b".to_string(),
            label: "B".to_string(),
        })
        .await
        .expect("create B");

    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "dynamic_apply".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");

    // Mimic the admin's form-handler flow: parse parent + child PKs
    // through json_to_sea_value, then hand to set_junction_dynamic.
    let parent_value = umbral::orm::write::json_to_sea_value(
        umbral::orm::SqlType::BigInt,
        &serde_json::Value::String(group.id.to_string()),
        false,
        "id",
        None,
    )
    .unwrap();
    let child_values: Vec<_> = [&perm_a, &perm_b]
        .iter()
        .map(|p| {
            umbral::orm::write::json_to_sea_value(
                umbral::orm::SqlType::Text,
                &serde_json::Value::String(p.codename.clone()),
                false,
                "codename",
                None,
            )
            .unwrap()
        })
        .collect();

    umbral::orm::set_junction_dynamic(JUNCTION_TABLE, parent_value.clone(), child_values, None)
        .await
        .expect("set_junction_dynamic");

    // Verify via the typed-API fetch — both rows should be linked.
    let mut fetched = group.perms.fetch().await.expect("fetch");
    fetched.sort_by(|x, y| x.codename.cmp(&y.codename));
    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].codename, perm_a.codename);
    assert_eq!(fetched[1].codename, perm_b.codename);

    // Re-set with just B — A should drop.
    let only_b = vec![
        umbral::orm::write::json_to_sea_value(
            umbral::orm::SqlType::Text,
            &serde_json::Value::String(perm_b.codename.clone()),
            false,
            "codename",
            None,
        )
        .unwrap(),
    ];
    umbral::orm::set_junction_dynamic(JUNCTION_TABLE, parent_value.clone(), only_b, None)
        .await
        .expect("set_junction_dynamic with new selection");
    let fetched = group.perms.fetch().await.expect("fetch after replace");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].codename, perm_b.codename);

    // Empty selection clears the junction.
    umbral::orm::set_junction_dynamic(JUNCTION_TABLE, parent_value, Vec::new(), None)
        .await
        .expect("set_junction_dynamic with empty selection");
    let fetched = group.perms.fetch().await.expect("fetch after clear");
    assert!(
        fetched.is_empty(),
        "empty child_ids should leave no junction rows for this parent",
    );
}

#[tokio::test]
async fn load_junction_selection_returns_current_child_pks_as_strings() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.load_sel".to_string(),
            label: "load".to_string(),
        })
        .await
        .expect("create permission");
    let group = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "load_sel_group".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");
    group.perms.add(&perm).await.expect("add");

    let parent_value = umbral::orm::write::json_to_sea_value(
        umbral::orm::SqlType::BigInt,
        &serde_json::Value::String(group.id.to_string()),
        false,
        "id",
        None,
    )
    .unwrap();
    let selection = umbral::orm::load_junction_selection(
        JUNCTION_TABLE,
        parent_value,
        umbral::orm::SqlType::Text,
        None,
    )
    .await
    .expect("load_junction_selection");
    assert_eq!(selection, vec![perm.codename.clone()]);
}

#[tokio::test]
async fn m2m_hydration_works_on_freshly_loaded_rows_via_filter_fetch() {
    boot().await;

    let perm = RoundTripPermission::objects()
        .create(RoundTripPermission {
            codename: "m2mtest.filter_fetch".to_string(),
            label: "filter".to_string(),
        })
        .await
        .expect("create permission");

    let created = RoundTripGroup::objects()
        .create(RoundTripGroup {
            id: 0,
            name: "editors_filter".to_string(),
            perms: M2M::empty(),
        })
        .await
        .expect("create group");
    created
        .perms
        .add(&perm)
        .await
        .expect("add via in-memory row");

    // Re-fetch via `filter(...).first()` — the M2M slot starts as
    // Default (no parent_id, no junction). The hydrate hook must
    // populate both so `.fetch()` finds the same junction row.
    let reloaded = RoundTripGroup::objects()
        .filter(round_trip_group::ID.eq(created.id))
        .first()
        .await
        .expect("first")
        .expect("group exists");
    assert_eq!(reloaded.perms.parent_id().copied(), Some(created.id));
    assert_eq!(reloaded.perms.junction_table(), Some(JUNCTION_TABLE));
    let fetched = reloaded.perms.fetch().await.expect("fetch via reloaded");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].codename, perm.codename);
}
