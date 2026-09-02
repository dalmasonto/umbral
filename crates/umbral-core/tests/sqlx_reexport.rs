//! gaps4 #65(a) — `umbral::sqlx::FromRow` is a real, working derive path.
//!
//! `umbral::sqlx` is now a first-class (not `#[doc(hidden)]`) re-export of
//! the exact sqlx umbral itself pins, feature-flagged with `derive` so the
//! `FromRow` derive macro — not just the trait — resolves through
//! `umbral::sqlx::FromRow`. This test proves that path actually works: the
//! derive expands (compiles) and the resulting impl round-trips through the
//! ORM's own read path (`objects()` + `filter` + `first`) against a real
//! in-memory SQLite pool — not just an "it compiles" smoke test.
//!
//! One honest limitation this test does NOT paper over: `umbral::sqlx` does
//! not let a crate drop its OWN `sqlx` dependency and still derive
//! `FromRow`. sqlx's derive macro expands to code with hardcoded absolute
//! `::sqlx::...` paths (no `#[sqlx(crate = "...")]` escape hatch, unlike
//! serde), so `::sqlx::` only resolves when the deriving crate itself
//! declares `sqlx` directly — this crate (`umbral-core`) does, which is WHY
//! `umbral::sqlx::FromRow` resolves here. See the doc comment on
//! `umbral::sqlx` in `crates/umbral/src/lib.rs` for the full explanation and
//! what actually prevents the version drift gaps4 #65 describes (matching
//! umbral-core's pin + `umbral doctor`, gaps4 #65c).

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbral_core::db;

// The struct under test names NO bare `sqlx::FromRow` anywhere — only
// `umbral::sqlx::FromRow`. If umbral's re-export ever stopped including the
// derive macro (e.g. the `derive` feature got dropped from
// `crates/umbral/Cargo.toml`), this file fails to compile.
#[derive(
    Debug,
    Clone,
    PartialEq,
    umbral::sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbral::orm::Model,
)]
#[umbral(table = "sqlx_reexport_widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE sqlx_reexport_widget (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    // Seed via raw SQL (test fixture, not framework/plugin code — the
    // ORM-only rule targets production code paths). The read side below is
    // what actually exercises `umbral::sqlx::FromRow`.
    sqlx::query("INSERT INTO sqlx_reexport_widget (name) VALUES (?)")
        .bind("gizmo")
        .execute(&pool)
        .await
        .expect("seed");
    pool
}

#[tokio::test]
async fn derive_via_umbral_sqlx_from_row_round_trips_through_the_orm() {
    let pool = fresh_pool().await;

    // The read path decodes rows into `Widget` via the `FromRow` impl that
    // `#[derive(umbral::sqlx::FromRow)]` generated above.
    let row = Widget::objects()
        .on(&pool)
        .filter(widget::NAME.eq("gizmo"))
        .first()
        .await
        .expect("query")
        .expect("row present");

    assert_eq!(row.name, "gizmo");
    assert_eq!(row.id, 1);
}
