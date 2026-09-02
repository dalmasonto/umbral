//! Phase 1, Task 3 — chainable, ambient-pooled `QuerySet<T>` from an M2M
//! relation via a junction subquery.
//!
//! `M2M::fetch()` is eager-only (Task 2's design note): it always returns
//! the full attached set with no way to `.filter()`/`.order_by()`/`.count()`
//! it further. `M2M::query()` closes that gap — it builds a `QuerySet<T>`
//! scoped to `<T.pk> IN (SELECT child_id FROM <junction> WHERE parent_id =
//! <parent_id>)` and composes onto the existing terminals unchanged.
//!
//! The parent id comes from `M2M.parent_id`, which the `set_m2m_parent_ids`
//! hook seeds on every row `fetch()`/`first()`/`get()` materialises (see
//! `m2m.rs`'s module doc) — so `dev.software_groups.query()` works on any
//! `dev` fetched through the ORM, no extra wiring needed.
//!
//! Behavioral: real rows through the actual public accessor, read the row
//! back — no SQL-string-only assertions.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::M2M;
use umbral::orm::QuerySet;
use umbral::orm::relation::{HopKind, HopSpec, JunctionSpec, to_many_hop, to_one_hop};
use umbral_core::db;

// =========================================================================
// Model declarations
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rmq_software_group")]
pub struct SoftwareGroup {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rmq_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rmq_software_group")]
    pub software_groups: M2M<SoftwareGroup>,
}

// =========================================================================
// Harness — one boot, deterministic seed.
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<SoftwareGroup>()
            .model::<Developer>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Three groups: two active, one inactive.
        for (name, active) in &[("core", true), ("infra", true), ("archived", false)] {
            sqlx::query("INSERT INTO rmq_software_group (name, active) VALUES (?, ?)")
                .bind(*name)
                .bind(*active)
                .execute(&pool)
                .await
                .expect("seed software_group");
        }
        // ada
        sqlx::query("INSERT INTO rmq_developer (name) VALUES (?)")
            .bind("ada")
            .execute(&pool)
            .await
            .expect("seed developer");
        // ada -> core(1), infra(2). NOT archived(3) — proves the junction
        // subquery, not "every active group", drives the result.
        for (parent, child) in &[(1_i64, 1_i64), (1_i64, 2_i64)] {
            sqlx::query(
                "INSERT INTO rmq_developer_software_groups (parent_id, child_id) VALUES (?, ?)",
            )
            .bind(*parent)
            .bind(*child)
            .execute(&pool)
            .await
            .expect("seed junction");
        }

        pool
    })
    .await
    .clone()
}

async fn fetch_ada() -> Developer {
    Developer::objects()
        .filter(developer::NAME.eq("ada"))
        .get()
        .await
        .expect("fetch ada")
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn m2m_query_composes_filter_order_by_and_fetch() {
    boot().await;
    let ada = fetch_ada().await;

    // Directly proves the entry-point subtlety this task resolved: a plain
    // fetched row's M2M.parent_id is populated (no extra wiring), so
    // `.query()` works immediately.
    assert_eq!(
        ada.software_groups.parent_id(),
        Some(&1_i64),
        "set_m2m_parent_ids must have seeded parent_id on a freshly fetched row"
    );

    let groups = ada
        .software_groups
        .query()
        .filter(software_group::ACTIVE.eq(true))
        .order_by(software_group::NAME.asc())
        .fetch()
        .await
        .expect("fetch scoped groups");

    // ada is attached to core+infra (both active); archived is neither
    // attached to ada NOR active, so it must not leak in either direction.
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["core", "infra"],
        "must return exactly ada's attached-and-active groups, ordered by name"
    );
}

#[tokio::test]
async fn m2m_query_count_matches_attached_rows() {
    boot().await;
    let ada = fetch_ada().await;

    let n = ada.software_groups.query().count().await.expect("count");
    assert_eq!(n, 2, "ada is attached to exactly 2 groups");
}

#[tokio::test]
async fn m2m_query_unattached_returns_empty_not_every_row() {
    boot().await;
    // A `Default`/`M2M::empty()` slot has no parent_id/junction — `.query()`
    // must behave like `fetch()`'s documented no-op (empty), never widen to
    // "every row" (that would silently promote an unattached field into a
    // full-table scan).
    let unattached: M2M<SoftwareGroup> = M2M::empty();
    let n = unattached.query().count().await.expect("count");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn to_many_hop_m2m_matches_query_result() {
    boot().await;
    let ada = fetch_ada().await;

    // The general path off the owning object directly, bypassing the M2M
    // field's own cached metadata — proves `to_many_hop` reads the parent pk
    // from `RelationSource` (the owning object), not from the M2M slot.
    let hop = HopSpec {
        kind: HopKind::M2M,
        from_table: "rmq_developer",
        to_table: "rmq_software_group",
        fk_column: "",
        fk_on_from: true,
        required: false,
        junction: Some(JunctionSpec {
            table: "rmq_developer_software_groups",
            parent_column: "parent_id",
            target_column: "child_id",
        }),
    };
    let groups = to_many_hop::<Developer, SoftwareGroup>(&ada, hop)
        .filter(software_group::ACTIVE.eq(true))
        .order_by(software_group::NAME.asc())
        .fetch()
        .await
        .expect("fetch via to_many_hop");
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["core", "infra"]);
}

/// Review fix (round 1) — a deep to-many chain, reached by composing two
/// PUBLIC calls (`to_one_hop` then `to_many_hop` off the resulting
/// `Relation`), used to hit an `assert!` in `to_many_hop` and panic the
/// caller's process. `Relation<T>` implements `RelationSource<T>`, so
/// nothing in the public API stops a caller from doing exactly this — it
/// must surface as a loud `Err` at the first fallible terminal instead,
/// via the same poison-now/fail-at-the-terminal mechanism `QuerySet`
/// already uses for other builder-time-unrejectable shapes.
#[tokio::test]
async fn to_many_hop_deep_chain_is_poisoned_not_panic() {
    boot().await;
    let ada = fetch_ada().await;

    // Hop 1 (to-one, hand-built — its exact target never resolves to SQL
    // here, since the deep-chain shape is caught before any query runs).
    let hop1 = HopSpec {
        kind: HopKind::Fk,
        from_table: "rmq_developer",
        to_table: "rmq_software_group",
        fk_column: "id",
        fk_on_from: true,
        required: true,
        junction: None,
    };
    let deep = to_one_hop::<Developer, SoftwareGroup>(&ada, hop1);

    // Hop 2 (to-many) off `deep`, which already carries hop1 — the deep
    // chain `to_many_hop` doesn't resolve yet.
    let hop2 = HopSpec {
        kind: HopKind::M2M,
        from_table: "rmq_software_group",
        to_table: "rmq_software_group",
        fk_column: "",
        fk_on_from: true,
        required: false,
        junction: Some(JunctionSpec {
            table: "rmq_developer_software_groups",
            parent_column: "parent_id",
            target_column: "child_id",
        }),
    };

    // Building the QuerySet must not panic ...
    let poisoned = to_many_hop::<SoftwareGroup, SoftwareGroup>(deep, hop2);
    // ... and the first fallible terminal must report the gap loudly.
    let err = poisoned
        .fetch()
        .await
        .expect_err("a deep to-many chain must fail loudly, not silently run a wrong query");
    let msg = err.to_string();
    assert!(
        msg.contains("Task 4") && msg.contains("deep to-many"),
        "error should name the deep-to-many-chain gap and point at Task 4: {msg}"
    );

    // count() must independently surface the same poison — a caller who
    // only calls .count() (never .fetch()) must not slip through.
    let err2 = to_many_hop::<SoftwareGroup, SoftwareGroup>(
        to_one_hop::<Developer, SoftwareGroup>(&ada, hop1),
        hop2,
    )
    .count()
    .await
    .expect_err("count() must also surface the poison");
    assert!(err2.to_string().contains("Task 4"));
}

/// Build the same poisoned deep-to-many-chain `QuerySet<SoftwareGroup>` the
/// tests above construct by hand, factored out so the write-terminal test
/// below doesn't repeat the hop wiring.
fn deep_chain_poisoned_queryset(ada: &Developer) -> QuerySet<SoftwareGroup> {
    let hop1 = HopSpec {
        kind: HopKind::Fk,
        from_table: "rmq_developer",
        to_table: "rmq_software_group",
        fk_column: "id",
        fk_on_from: true,
        required: true,
        junction: None,
    };
    let hop2 = HopSpec {
        kind: HopKind::M2M,
        from_table: "rmq_software_group",
        to_table: "rmq_software_group",
        fk_column: "",
        fk_on_from: true,
        required: false,
        junction: Some(JunctionSpec {
            table: "rmq_developer_software_groups",
            parent_column: "parent_id",
            target_column: "child_id",
        }),
    };
    to_many_hop::<SoftwareGroup, SoftwareGroup>(
        to_one_hop::<Developer, SoftwareGroup>(ada, hop1),
        hop2,
    )
}

/// Review fix (round 2) — round 1 closed the panic, but `check_annotations()`
/// (the poison-surfacing check) was only wired into `fetch`/`count`/`explain`/
/// `fetch_annotated`. The poisoned deep-chain `QuerySet` carries NO scoping
/// predicate (`Predicate::new(Expr::cust("1 = 1"))`), so `.delete()` on it
/// would otherwise silently DELETE EVERY ROW of `rmq_software_group` — the
/// exact "silently running a wrong query" the poison mechanism claims to
/// prevent. Proves `.delete()` and `.values()` both refuse instead, and that
/// the 3 seeded rows survive the attempted delete untouched.
#[tokio::test]
async fn to_many_hop_deep_chain_poison_blocks_delete_and_values() {
    boot().await;
    let ada = fetch_ada().await;

    let before = SoftwareGroup::objects()
        .count()
        .await
        .expect("count before");
    assert_eq!(before, 3, "sanity: 3 seeded software_group rows");

    let delete_err = deep_chain_poisoned_queryset(&ada)
        .delete()
        .await
        .expect_err("a poisoned QuerySet must refuse delete(), not mass-delete the table");
    assert!(
        delete_err.to_string().contains("Task 4"),
        "delete() error should name the same deep-to-many-chain gap: {delete_err}"
    );

    let values_err = deep_chain_poisoned_queryset(&ada)
        .values(&["id", "name"])
        .await
        .expect_err("a poisoned QuerySet must refuse values(), not return every row");
    assert!(values_err.to_string().contains("Task 4"));

    // The rows must be untouched — the DELETE never reached the database.
    let after = SoftwareGroup::objects().count().await.expect("count after");
    assert_eq!(
        after, before,
        "delete() on a poisoned QuerySet must not have removed any row"
    );
}
