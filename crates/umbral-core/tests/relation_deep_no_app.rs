//! Fix-round-1 regression (Task 2, Finding 1): a deep (>=2-hop) to-one path
//! resolved with an explicit `.on(&pool)` but WITHOUT a booted `App` must
//! surface as `Err` — NOT a panic.
//!
//! The multi-hop resolver looks intermediate-table PKs up in the model
//! registry, which is only populated by `App::build()`. A missing registry
//! must become a clean protocol error (the resolver's contract: "errors
//! loudly, never a wrong query / panic").
//!
//! This lives in its OWN test binary precisely so no other test boots an App
//! in-process: the registry `OnceLock`, once set, stays set for the whole
//! binary, so the un-booted state is only reachable in a binary that never
//! calls `App::build()`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use umbral::orm::ForeignKey;
use umbral::orm::relation::{HopKind, HopSpec, Relation, to_one_hop};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "na_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub email: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "na_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub owner: ForeignKey<User>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "na_author")]
pub struct Author {
    #[umbral(primary_key)]
    pub id: i64,
    pub company: ForeignKey<Company>,
}

fn company_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "na_author",
        to_table: "na_company",
        fk_column: "company",
        fk_on_from: true,
        required: true,
        junction: None,
    }
}

fn owner_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "na_company",
        to_table: "na_user",
        fk_column: "owner",
        fk_on_from: true,
        required: true,
        junction: None,
    }
}

/// A 2-hop chain resolved without a booted App returns `Err`, never a panic.
#[tokio::test]
async fn deep_path_without_app_is_err_not_panic() {
    // A pool exists (so `.on(&pool)` is satisfied) but NO `App::build()` runs,
    // so the model registry is empty. The resolver errors before it ever
    // touches the DB, so the pool need not have any tables.
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let author = Author {
        id: 1,
        company: ForeignKey::new(1_i64),
    };

    // author -> company -> owner (User): a 2-hop chain.
    let rel: Relation<User> = to_one_hop::<Company, User>(
        to_one_hop::<Author, Company>(&author, company_hop()),
        owner_hop(),
    )
    .on(&pool);

    let result: Result<Option<User>, _> = rel.get_opt().await;
    assert!(
        result.is_err(),
        "a >=2-hop path with no booted registry must be an Err, not a panic and \
         not a wrong row: {result:?}"
    );
}
