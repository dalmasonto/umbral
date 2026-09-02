//! Phase 1, Task 6 — the prelude's relation-traversal surface.
//!
//! The honest split (see `crates/umbral/src/lib.rs`'s `prelude` module doc):
//!
//! - The ENGINE type [`umbral::orm::Relation`] is a name callers write by
//!   hand (a field type, a return type), so it belongs in
//!   `umbral::prelude`. This file imports it ONLY via `use umbral::prelude::*;`
//!   — there is no `use umbral::orm::relation::Relation;` anywhere below —
//!   proving the prelude alone supplies it.
//! - The per-model `<M>Relations` trait that `#[derive(Model)]` generates
//!   CANNOT be preluded: it is defined inside the CALLING crate (this test
//!   crate), not inside `umbral`, so a facade prelude structurally cannot
//!   name it ahead of time. What the derive DOES guarantee is that the
//!   trait is emitted as a `pub` sibling item at the same module scope as
//!   the model struct itself, so the same glob that already brings the
//!   model into scope (`use models::*;` below) also brings its relation
//!   trait into scope — no separate hand-`use` per model, per method.
//!
//! This test proves both halves: `Relation` resolves from the prelude glob
//! alone, and a two-hop mixed (FK then M2M) chain compiles and runs with
//! only `use umbral::prelude::*;` + `use models::*;` in scope — never an
//! explicit `use models::MemberRelations;` or `...TeamRelations;`.

#![allow(dead_code)]

// Deliberately ONLY the prelude glob for the relation-traversal surface —
// no `umbral::orm::relation::...` import anywhere in this file.
use umbral::prelude::*;
use umbral_core::db;

mod models {
    // Models bring their own field-type imports from the prelude too, the
    // same way any user crate would.
    use umbral::prelude::*;

    #[derive(
        Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
    )]
    #[umbral(table = "relprel_group")]
    pub struct Group {
        #[umbral(primary_key)]
        pub id: i64,
        pub name: String,
    }

    #[derive(
        Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
    )]
    #[umbral(table = "relprel_team")]
    pub struct Team {
        #[umbral(primary_key)]
        pub id: i64,
        pub name: String,
        #[sqlx(skip)]
        #[serde(skip)]
        #[umbral(m2m = "relprel_group")]
        pub groups: M2M<Group>,
    }

    #[derive(
        Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
    )]
    #[umbral(table = "relprel_member")]
    pub struct Member {
        #[umbral(primary_key)]
        pub id: i64,
        pub name: String,
        pub team: ForeignKey<Team>,
    }
}

// Only the model module's glob — no `use models::MemberRelations;` /
// `use models::TeamRelations;` anywhere. If the generated traits were not
// discoverable this way, `member.team().groups()` below would fail to
// compile ("no method named `team` found for type `Member`").
use models::*;

// A bare use of `Relation` sourced only from `umbral::prelude::*` — proves
// the type itself resolves from the prelude glob, independent of the
// relation-chain calls below (which never name `Relation` explicitly).
fn _relation_type_is_in_scope_from_prelude_alone(
    r: Relation<models::Team>,
) -> Relation<models::Team> {
    r
}

static BOOT: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Group>()
            .model::<Team>()
            .model::<Member>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        for name in &["core", "infra"] {
            sqlx::query("INSERT INTO relprel_group (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed group");
        }
        sqlx::query("INSERT INTO relprel_team (name) VALUES ('platform')")
            .execute(&pool)
            .await
            .expect("seed team");
        sqlx::query("INSERT INTO relprel_team_groups (parent_id, child_id) VALUES (1, 1), (1, 2)")
            .execute(&pool)
            .await
            .expect("seed team->group");
        sqlx::query("INSERT INTO relprel_member (name, team) VALUES ('ada', 1)")
            .execute(&pool)
            .await
            .expect("seed member");

        pool
    })
    .await
    .clone()
}

/// The deep chain: an object-rooted forward FK hop (`member.team()`)
/// followed by a forward M2M hop (`.groups()`), through generated
/// accessors reached ONLY via `umbral::prelude::*` + `use models::*;`.
#[tokio::test]
async fn deep_chain_compiles_and_runs_from_prelude_plus_model_glob_only() {
    boot().await;

    let member = Member::objects()
        .filter(models::member::NAME.eq("ada"))
        .get()
        .await
        .expect("fetch member");

    let names: Vec<String> = member
        .team()
        .groups()
        .order_by(models::group::NAME.asc())
        .fetch()
        .await
        .expect("member.team().groups()")
        .into_iter()
        .map(|g| g.name)
        .collect();

    assert_eq!(names, vec!["core", "infra"]);
}
