//! Phase 1, Task 4 — deep to-many chain resolution to the leaf, with the
//! DISTINCT-by-leaf-PK default and the `.with_duplicates()` opt-out.
//!
//! A chain that crosses a to-many hop (`dev.software_groups().software()`,
//! M2M→M2M) resolves to a `QuerySet<Leaf>` whose base query expresses the
//! whole traversal as junction JOINs rooted at the starting PK. A leaf
//! reachable via two paths (a `Software` in two of the dev's groups) appears
//! ONCE by default (`SELECT DISTINCT` on the leaf PK) and TWICE under
//! `.with_duplicates()` (raw JOIN multiplicity).
//!
//! Behavioral: real rows + real junctions through the actual public
//! `to_many_hop` accessor, read the object graph back — never a SQL-string
//! assertion as a proxy.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::M2M;
use umbral::orm::relation::{HopKind, HopSpec, JunctionSpec, to_many_hop, to_one_hop};
use umbral_core::db;

// =========================================================================
// Model declarations — dev --M2M--> group --M2M--> software
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rdm_software")]
pub struct Software {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rdm_software_group")]
pub struct SoftwareGroup {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rdm_software")]
    pub software: M2M<Software>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rdm_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rdm_software_group")]
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
            .model::<Software>()
            .model::<SoftwareGroup>()
            .model::<Developer>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Two groups (core=1, infra=2), one archived (3, inactive & empty).
        for (name, active) in &[("core", true), ("infra", true), ("archived", false)] {
            sqlx::query("INSERT INTO rdm_software_group (name, active) VALUES (?, ?)")
                .bind(*name)
                .bind(*active)
                .execute(&pool)
                .await
                .expect("seed software_group");
        }
        // Two software rows: editor=1, compiler=2.
        for (name, active) in &[("editor", true), ("compiler", true)] {
            sqlx::query("INSERT INTO rdm_software (name, active) VALUES (?, ?)")
                .bind(*name)
                .bind(*active)
                .execute(&pool)
                .await
                .expect("seed software");
        }
        // ada = developer 1.
        sqlx::query("INSERT INTO rdm_developer (name) VALUES (?)")
            .bind("ada")
            .execute(&pool)
            .await
            .expect("seed developer");

        // ada -> core(1), infra(2). NOT archived(3).
        for (parent, child) in &[(1_i64, 1_i64), (1_i64, 2_i64)] {
            sqlx::query(
                "INSERT INTO rdm_developer_software_groups (parent_id, child_id) VALUES (?, ?)",
            )
            .bind(*parent)
            .bind(*child)
            .execute(&pool)
            .await
            .expect("seed dev->group junction");
        }
        // group -> software:
        //   core(1)  -> editor(1), compiler(2)
        //   infra(2) -> editor(1)          (editor now reachable via TWO groups)
        //   archived(3) -> (nothing)
        for (parent, child) in &[(1_i64, 1_i64), (1_i64, 2_i64), (2_i64, 1_i64)] {
            sqlx::query(
                "INSERT INTO rdm_software_group_software (parent_id, child_id) VALUES (?, ?)",
            )
            .bind(*parent)
            .bind(*child)
            .execute(&pool)
            .await
            .expect("seed group->software junction");
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

fn dev_to_group_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::M2M,
        from_table: "rdm_developer",
        to_table: "rdm_software_group",
        fk_column: "",
        fk_on_from: true,
        required: false,
        junction: Some(JunctionSpec {
            table: "rdm_developer_software_groups",
            parent_column: "parent_id",
            target_column: "child_id",
        }),
    }
}

fn group_to_software_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::M2M,
        from_table: "rdm_software_group",
        to_table: "rdm_software",
        fk_column: "",
        fk_on_from: true,
        required: false,
        junction: Some(JunctionSpec {
            table: "rdm_software_group_software",
            parent_column: "parent_id",
            target_column: "child_id",
        }),
    }
}

/// Build `ada.software_groups().software()` (M2M→M2M) as a chainable
/// `QuerySet<Software>` by nesting the public `to_many_hop` accessor.
fn ada_software(ada: &Developer) -> umbral::orm::QuerySet<Software> {
    let inner = to_many_hop::<Developer, SoftwareGroup>(ada, dev_to_group_hop());
    to_many_hop::<SoftwareGroup, Software>(inner, group_to_software_hop())
}

// =========================================================================
// Tests
// =========================================================================

/// The headline: a leaf reachable via two paths (editor is in core AND infra,
/// both attached to ada) is deduped to one row by default.
#[tokio::test]
async fn deep_m2m_chain_dedupes_leaf_by_pk_by_default() {
    boot().await;
    let ada = fetch_ada().await;

    let sw = ada_software(&ada)
        .order_by(software::NAME.asc())
        .fetch()
        .await
        .expect("resolve dev.software_groups().software()");
    let names: Vec<&str> = sw.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["compiler", "editor"],
        "editor is reachable via two groups but must appear ONCE by default"
    );
}

/// `.with_duplicates()` drops the DISTINCT and returns raw JOIN multiplicity:
/// editor appears twice (via core and via infra).
#[tokio::test]
async fn deep_m2m_chain_with_duplicates_returns_join_multiplicity() {
    boot().await;
    let ada = fetch_ada().await;

    let sw = ada_software(&ada)
        .with_duplicates()
        .order_by(software::NAME.asc())
        .fetch()
        .await
        .expect("resolve with duplicates");
    let names: Vec<&str> = sw.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["compiler", "editor", "editor"],
        "with_duplicates must surface editor's two reachability paths"
    );
}

/// `count()` honours the same DISTINCT-by-default / multiplicity opt-out.
#[tokio::test]
async fn deep_m2m_chain_count_respects_distinct() {
    boot().await;
    let ada = fetch_ada().await;

    let distinct = ada_software(&ada).count().await.expect("distinct count");
    assert_eq!(distinct, 2, "two distinct software (editor, compiler)");

    let raw = ada_software(&ada)
        .with_duplicates()
        .count()
        .await
        .expect("raw count");
    assert_eq!(raw, 3, "three junction paths (editor×2, compiler×1)");
}

/// The leaf QuerySet composes with `.filter()` on the leaf table's own
/// columns — the resolver has established software as the query root.
#[tokio::test]
async fn deep_m2m_chain_composes_leaf_filter() {
    boot().await;
    let ada = fetch_ada().await;

    let only_editor = ada_software(&ada)
        .filter(software::NAME.eq("editor"))
        .count()
        .await
        .expect("filtered count");
    assert_eq!(
        only_editor, 1,
        "distinct editor, filtered on the leaf column"
    );

    let sw = ada_software(&ada)
        .filter(software::NAME.eq("compiler"))
        .fetch()
        .await
        .expect("filtered fetch");
    assert_eq!(sw.len(), 1);
    assert_eq!(sw[0].name, "compiler");
}

/// A single to-many hop off a bare object still resolves (Task 3 path) and
/// now also carries a `RelPath`, so it can be extended by a second hop.
#[tokio::test]
async fn single_to_many_hop_still_resolves() {
    boot().await;
    let ada = fetch_ada().await;

    let groups = to_many_hop::<Developer, SoftwareGroup>(&ada, dev_to_group_hop())
        .filter(software_group::ACTIVE.eq(true))
        .order_by(software_group::NAME.asc())
        .fetch()
        .await
        .expect("single hop still works");
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["core", "infra"]);
}

/// A to-one hop AFTER a to-many hop (per-row JOIN fan-out) is deferred past
/// Phase 1 — the chain must POISON (fail loudly at the terminal), never run a
/// wrong query. Reached by composing public calls: to-many → to-one → to-many
/// (ends in a to-many, so the builder returns a `QuerySet`).
#[tokio::test]
async fn to_one_after_to_many_is_poisoned() {
    boot().await;
    let ada = fetch_ada().await;

    // to-many (dev->group), then a to-one FK hop off the group, then to-many.
    let fk_after = HopSpec {
        kind: HopKind::Fk,
        from_table: "rdm_software_group",
        to_table: "rdm_software_group",
        fk_column: "id",
        fk_on_from: true,
        required: true,
        junction: None,
    };
    let via_to_one = to_one_hop::<SoftwareGroup, SoftwareGroup>(
        to_many_hop::<Developer, SoftwareGroup>(&ada, dev_to_group_hop()),
        fk_after,
    );
    let poisoned = to_many_hop::<SoftwareGroup, Software>(via_to_one, group_to_software_hop());

    let err = poisoned
        .fetch()
        .await
        .expect_err("a to-one-after-to-many chain must fail loudly");
    let msg = err.to_string();
    assert!(
        msg.contains("to-one") && msg.contains("to-many"),
        "poison must name the deferred to-one-after-to-many shape: {msg}"
    );
}
