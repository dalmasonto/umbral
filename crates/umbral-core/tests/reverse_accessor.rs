//! gaps2 #45 (accessor half) — the ZERO-DECLARATION instance
//! reverse-relation accessor: `post.comment_set.all()` as a
//! generic runtime method on any model instance.
//!
//! Unlike the macro-emitted `<child>_set()` accessor (which the parent
//! gets only because the child's `ForeignKey<Parent>` is visible at the
//! child's derive site), this accessor discovers the FK column on the
//! child `C` at runtime by scanning `C::FIELDS` for `fk_target ==
//! Self::TABLE`. The child type `C` is named at the call site:
//!
//!   parent.reverse::<Child>()              // -> QuerySet<Child>
//!   parent.reverse::<Child>().filter(...).order_by(...).fetch()
//!   parent.reverse_via::<Child2>("fk_a")   // disambiguate 2+ FKs
//!
//! Pins (every assertion reads real rows back, no tautologies):
//! - `reverse::<Child>()` returns exactly the children whose FK = pk,
//! - chaining `.filter(...)` / `.count()` narrows correctly,
//! - a parent with zero children → empty result, not an error,
//! - a child with TWO FKs to the parent → `reverse` errors clearly,
//!   and `reverse_via("fk_a")` resolves the right set,
//! - a parent that ALSO declares a `ReverseSet` still works via both
//!   the declared `prefetch_related` path AND this generic accessor.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, ReverseSet};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rva_parent")]
pub struct Parent {
    pub id: i64,
    pub name: String,
    // A parent that ALSO declares a typed ReverseSet — the generic
    // accessor must not conflict with the declared prefetch path.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "parent")]
    pub child_set: ReverseSet<Child>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rva_child")]
pub struct Child {
    pub id: i64,
    pub label: String,
    pub parent: ForeignKey<Parent>,
}

/// A child with TWO foreign keys to `Parent` — the ambiguity case.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rva_link")]
pub struct Link {
    pub id: i64,
    pub note: String,
    pub fk_a: ForeignKey<Parent>,
    pub fk_b: ForeignKey<Parent>,
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
            .model::<Link>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // p1 (id 1): two children. p2 (id 2): one. p3 (id 3): zero.
        for name in ["p1", "p2", "p3"] {
            sqlx::query("INSERT INTO rva_parent (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed parent");
        }
        for (label, parent) in [("c1", 1), ("c2", 1), ("c3", 2)] {
            sqlx::query("INSERT INTO rva_child (label, parent) VALUES (?, ?)")
                .bind(label)
                .bind(parent)
                .execute(&pool)
                .await
                .expect("seed child");
        }
        // Links: fk_a points at p1, fk_b points at p2 (so the two FKs
        // resolve to DIFFERENT parents — proves reverse_via picks the
        // right column, not just "any FK").
        for (note, fk_a, fk_b) in [("L1", 1, 2), ("L2", 1, 3)] {
            sqlx::query("INSERT INTO rva_link (note, fk_a, fk_b) VALUES (?, ?, ?)")
                .bind(note)
                .bind(fk_a)
                .bind(fk_b)
                .execute(&pool)
                .await
                .expect("seed link");
        }
    })
    .await;
}

use umbral::orm::ReverseRelations;

async fn get_parent(name: &str) -> Parent {
    Parent::objects()
        .filter(parent::NAME.eq(name))
        .first()
        .await
        .expect("query parent")
        .expect("parent row")
}

#[tokio::test]
async fn reverse_returns_children_pointing_at_this_instance() {
    boot().await;
    let p1 = get_parent("p1").await;
    let mut kids = p1
        .reverse::<Child>()
        .expect("discover FK column on Child")
        .fetch()
        .await
        .expect("fetch children");
    kids.sort_by(|a, b| a.label.cmp(&b.label));
    let labels: Vec<&str> = kids.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["c1", "c2"], "exactly p1's two children");
    // Every returned child's FK really points at p1 (read the real FK).
    for c in &kids {
        assert_eq!(c.parent.id(), 1, "child FK must resolve to p1's id");
    }
}

#[tokio::test]
async fn reverse_is_chainable_filter_and_count() {
    boot().await;
    let p1 = get_parent("p1").await;
    // Narrow to a single child by a child-column predicate.
    let narrowed = p1
        .reverse::<Child>()
        .expect("discover FK")
        .filter(child::LABEL.eq("c2"))
        .fetch()
        .await
        .expect("filtered fetch");
    assert_eq!(narrowed.len(), 1);
    assert_eq!(narrowed[0].label, "c2");

    // count() on the chain returns the right number.
    let n = p1
        .reverse::<Child>()
        .expect("discover FK")
        .count()
        .await
        .expect("count children");
    assert_eq!(n, 2, "p1 has two children");
}

#[tokio::test]
async fn reverse_with_zero_children_is_empty_not_error() {
    boot().await;
    let p3 = get_parent("p3").await;
    let kids = p3
        .reverse::<Child>()
        .expect("discover FK column")
        .fetch()
        .await
        .expect("zero children is Ok, not Err");
    assert!(kids.is_empty(), "p3 has no children → empty Vec");
}

#[tokio::test]
async fn ambiguous_two_fks_errors_then_reverse_via_resolves() {
    boot().await;
    let p1 = get_parent("p1").await;

    // Two FKs (fk_a, fk_b) → reverse::<Link>() must error clearly,
    // naming both candidate columns and pointing at reverse_via.
    // (QuerySet<C> isn't Debug, so we can't `.expect_err()`; match.)
    let err = match p1.reverse::<Link>() {
        Ok(_) => panic!("two FKs to Parent must be ambiguous"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("fk_a") && msg.contains("fk_b"),
        "error names both candidate FK columns: {msg}"
    );
    assert!(
        msg.contains("reverse_via"),
        "error directs the caller to reverse_via: {msg}"
    );

    // reverse_via("fk_a"): both links have fk_a = p1 → 2 rows.
    let by_a = p1
        .reverse_via::<Link>("fk_a")
        .expect("explicit fk_a column")
        .fetch()
        .await
        .expect("fetch via fk_a");
    let mut a_notes: Vec<&str> = by_a.iter().map(|l| l.note.as_str()).collect();
    a_notes.sort();
    assert_eq!(a_notes, vec!["L1", "L2"], "fk_a links both point at p1");

    // reverse_via("fk_b") on p1: only L... none have fk_b = 1 (L1.fk_b=2,
    // L2.fk_b=3) → empty. Proves the column selection is real, not a
    // tautology that returns "any FK".
    let by_b = p1
        .reverse_via::<Link>("fk_b")
        .expect("explicit fk_b column")
        .fetch()
        .await
        .expect("fetch via fk_b");
    assert!(by_b.is_empty(), "no link has fk_b pointing at p1");

    // A bogus column name is rejected loudly.
    let bad_msg = match p1.reverse_via::<Link>("nope_col") {
        Ok(_) => panic!("unknown column must error"),
        Err(e) => e.to_string(),
    };
    assert!(bad_msg.contains("nope_col"), "error names the bad column");
}

#[tokio::test]
async fn reverse_to_a_model_with_no_fk_back_errors() {
    boot().await;
    // Parent has no FK to Child, so Child.reverse::<Parent>() finds
    // no fk_target == rva_child and must error clearly.
    let c1 = Child::objects()
        .filter(child::LABEL.eq("c1"))
        .first()
        .await
        .expect("query child")
        .expect("child row");
    let err = match c1.reverse::<Parent>() {
        Ok(_) => panic!("Parent has no FK to Child"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("no foreign key"),
        "error explains there is no FK back: {err}"
    );
}

#[tokio::test]
async fn declared_reverse_set_and_generic_accessor_agree() {
    boot().await;
    // The declared prefetch_related path still works...
    let mut parents = Parent::objects()
        .filter(parent::NAME.eq("p1"))
        .prefetch_related("child_set")
        .fetch()
        .await
        .expect("prefetch child_set");
    let p1 = parents.remove(0);
    let declared = p1
        .child_set
        .resolved()
        .expect("child_set resolved after prefetch");
    let mut declared_labels: Vec<&str> = declared.iter().map(|c| c.label.as_str()).collect();
    declared_labels.sort();
    assert_eq!(declared_labels, vec!["c1", "c2"]);

    // ...and the generic accessor returns the SAME set.
    let mut generic = p1
        .reverse::<Child>()
        .expect("discover FK")
        .fetch()
        .await
        .expect("generic fetch");
    generic.sort_by(|a, b| a.label.cmp(&b.label));
    let generic_labels: Vec<&str> = generic.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        generic_labels, declared_labels,
        "declared ReverseSet and generic accessor agree"
    );
}
