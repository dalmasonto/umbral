//! gaps3 #31 — a `#[derive(Choices)]` enum field must decode from a
//! `VARCHAR` column on Postgres, not only `TEXT`.
//!
//! The `Choices` derive round-trips the enum as a string: `type_info()`
//! reports `TEXT`. sqlx's *default* `Type::compatible` accepts only the
//! exact `type_info()` (`TEXT`), so a typed read (`sqlx::query_as` /
//! `Model::objects().fetch()`) of a row whose column is `VARCHAR` — which
//! is what an umbral 0.0.4 migration emitted, and what any DR restore of
//! that schema still produces — fails on Postgres with:
//!
//! ```text
//! mismatched types; Rust type `FixtureStatus` (as SQL type `TEXT`)
//! is not compatible with SQL type `VARCHAR`
//! ```
//!
//! SQLite never trips this (VARCHAR ≡ TEXT affinity), so it hid in dev and
//! took down a core feature in prod (every RSVP 500'd). The fix delegates
//! the derive's `compatible` to `String`, which accepts the whole text
//! family (TEXT / VARCHAR / BPCHAR / NAME / citext). This is a decode-side
//! fix: existing `VARCHAR` columns start decoding with no migration.
//!
//! The `..._compatible_with_text_family` test reproduces the bug with **no
//! database** — `Type::compatible` is a pure function over type info, and
//! `PgTypeInfo::with_name("varchar")` name-matches the built-in `VARCHAR`.
//! Before the fix it returns `false`; after, `true`. A live typed
//! round-trip against a real `VARCHAR` column lives behind `#[ignore]`
//! (SQLite can't catch this class).

use umbral::_sqlx::Type;
use umbral::_sqlx::postgres::PgTypeInfo;
use umbral::_sqlx::postgres::Postgres;
use umbral::orm::{Model, SqlType};

/// Mirrors the web3clubs_fc `FixtureStatus` that surfaced the bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, umbral::orm::Choices)]
#[choices(rename_all = "lowercase")]
pub enum FixtureStatus {
    Scheduled,
    Live,
    Done,
}

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_gaps3_31_fixture")]
pub struct Fixture {
    pub id: i64,
    pub opponent: String,
    #[umbral(choices, default = "scheduled")]
    pub status: FixtureStatus,
}

/// The derive emits `SqlType::Text` for a choices column today, so fresh
/// migrations are already `TEXT`. The compatibility fix is what rescues
/// the `VARCHAR` columns that older migrations (0.0.4) baked in.
#[test]
fn choices_field_classifies_as_text() {
    let status = <Fixture as Model>::FIELDS
        .iter()
        .find(|f| f.name == "status")
        .expect("status field");
    assert_eq!(status.ty, SqlType::Text);
    assert!(!status.nullable);
}

/// The regression guard, runnable with no database. `Type::compatible` is
/// a pure function over the column's type info, and
/// `PgTypeInfo::with_name(n)` name-matches the built-in type `n`. Before
/// the fix the default `compatible` accepted only the enum's own
/// `type_info()` (`TEXT`), so `VARCHAR` returned `false` (the 500); after,
/// the whole text family decodes — but non-text types still don't (we
/// delegated to `String`, not "accept anything").
///
/// (`with_name` is used deliberately over `with_oid`: an OID-declared type
/// info compares equal to *any* resolved type under sqlx's `==` soft-eq
/// escape hatch, which would mask the regression. Name matching does not.)
#[test]
fn choices_decode_is_compatible_with_the_text_family() {
    let compatible =
        |name| <FixtureStatus as Type<Postgres>>::compatible(&PgTypeInfo::with_name(name));

    // The bug: a VARCHAR column must decode.
    assert!(
        compatible("varchar"),
        "VARCHAR must be decodable — gaps3 #31"
    );
    // The rest of the text family String accepts.
    assert!(compatible("text"), "TEXT must be decodable");
    assert!(compatible("bpchar"), "CHAR/BPCHAR must be decodable");
    assert!(compatible("name"), "NAME must be decodable");

    // Sanity: the fix widens to the text family, it does not accept every
    // type. A non-text column (INT4) is still rejected.
    assert!(!compatible("int4"), "INT4 must stay incompatible");
}

/// End-to-end typed round-trip on SQLite: proves the derive's
/// Encode/Decode pair works through `sqlx::query_as`. SQLite treats
/// VARCHAR and TEXT identically, so this passes regardless of the fix —
/// it guards the derive, not the Postgres-specific strictness.
#[tokio::test]
async fn choices_round_trips_typed_on_sqlite() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    // Column deliberately declared VARCHAR to mirror the 0.0.4 schema.
    sqlx::query(
        "CREATE TABLE umbral_gaps3_31_fixture (\
            id INTEGER PRIMARY KEY, \
            opponent TEXT NOT NULL, \
            status VARCHAR(20) NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    sqlx::query("INSERT INTO umbral_gaps3_31_fixture (id, opponent, status) VALUES (?, ?, ?)")
        .bind(1_i64)
        .bind("Rivals FC")
        .bind(FixtureStatus::Live)
        .execute(&pool)
        .await
        .expect("insert row");

    let row: Fixture = sqlx::query_as::<_, Fixture>(
        "SELECT id, opponent, status FROM umbral_gaps3_31_fixture WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("typed read of a Choices column");
    assert_eq!(row.status, FixtureStatus::Live);
    assert_eq!(row.opponent, "Rivals FC");
}

/// The exact production repro, behind `#[ignore]`: a typed read of a
/// `VARCHAR` Choices column on a live Postgres. Before the fix this errors
/// with "mismatched types ... TEXT is not compatible with VARCHAR"; after,
/// it decodes. Run with `UMBRAL_TEST_POSTGRES_URL=... cargo test -- --ignored`.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn choices_typed_read_from_varchar_column_postgres() {
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.expect("connect postgres");

    sqlx::query("DROP TABLE IF EXISTS umbral_gaps3_31_fixture")
        .execute(&pool)
        .await
        .unwrap();
    // VARCHAR(20), exactly as the 0.0.4 migration emitted it.
    sqlx::query(
        "CREATE TABLE umbral_gaps3_31_fixture (\
            id BIGSERIAL PRIMARY KEY, \
            opponent TEXT NOT NULL, \
            status VARCHAR(20) NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO umbral_gaps3_31_fixture (opponent, status) VALUES ($1, $2)")
        .bind("Rivals FC")
        .bind(FixtureStatus::Scheduled)
        .execute(&pool)
        .await
        .unwrap();

    // The typed read that 500'd in production before the fix.
    let row: Fixture = sqlx::query_as::<_, Fixture>(
        "SELECT id, opponent, status FROM umbral_gaps3_31_fixture WHERE opponent = $1",
    )
    .bind("Rivals FC")
    .fetch_one(&pool)
    .await
    .expect("typed read of a VARCHAR Choices column must not 500 (gaps3 #31)");
    assert_eq!(row.status, FixtureStatus::Scheduled);
}
