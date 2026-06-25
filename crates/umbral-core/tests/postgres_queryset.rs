//! Coverage for Phase 2.5 of the Postgres rollout: the QuerySet
//! terminals dispatch on the `DbPool` variant, so the same `Manager<T>`
//! works against a `PgPool` exactly the way it does against a
//! `SqlitePool`.
//!
//! Two layers of coverage:
//!
//! - **Type-level pin.** A function whose body never runs but whose
//!   types the compiler still checks. If the Phase 2.5 contract
//!   regresses (a trait bound vanishes, `.on_pg` is dropped, the
//!   `FromRow<PgRow>` impl stops emitting), this fails at compile
//!   time. No Postgres server needed.
//! - **Full round trip.** A `#[tokio::test]` marked `#[ignore]` that
//!   runs only when `UMBRAL_TEST_POSTGRES_URL` is set in the
//!   environment. Boots a real PgPool, creates the table, inserts a
//!   couple of rows, exercises every terminal (fetch / first / count
//!   / exists), and asserts the results. CI without Postgres skips
//!   it silently; a developer with a local Postgres can run it via
//!   `cargo test --test postgres_queryset -- --ignored`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use umbral::orm::Manager;

// The Article model is parallel to `umbral_core::orm::Post` (the
// SQLite-shaped test fixture). We derive it freshly here so the test
// owns its own table and migration schema; otherwise the Postgres
// round-trip would have to coexist with the SQLite test fixtures'
// `post` table on the same instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_phase25_article")]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<DateTime<Utc>>,
}

// --------------------------------------------------------------------- //
// Type-level pin                                                         //
// --------------------------------------------------------------------- //

/// Compile-only coverage: the Phase 2.5 surface accepts a `&PgPool`
/// at every terminal point. If this stops compiling, the contract has
/// regressed. The body is never executed (the test function has an
/// empty body), but the helper inside it gets type-checked.
#[test]
fn pg_pool_typechecks_against_every_queryset_terminal() {
    // `_unreachable` is never called, but the compiler still checks
    // its body. That body proves the type system accepts the call
    // path against a `&PgPool`.
    #[allow(dead_code)]
    async fn _unreachable(pg_pool: &PgPool) -> Result<(), sqlx::Error> {
        // Manager::on_pg path.
        let _v: Vec<Article> = Article::objects().on_pg(pg_pool).fetch().await?;
        let _f: Option<Article> = Article::objects().on_pg(pg_pool).first().await?;
        let _c: i64 = Article::objects().on_pg(pg_pool).count().await?;
        let _e: bool = Article::objects().on_pg(pg_pool).exists().await?;

        // QuerySet::on_pg path — same surface but reached directly
        // (used by tests that chain through `.filter` or `.order_by`
        // before pinning the pool).
        let _q = Manager::<Article>::default();
        // chained construct then pin via on_pg.
        let _v2: Vec<Article> = Article::objects()
            .limit(5)
            .offset(0)
            .on_pg(pg_pool)
            .fetch()
            .await?;

        Ok(())
    }

    // The test body itself is empty — the compiler did the work above.
}

/// Same compile-only pin for the SQLite side. The Phase 2.5 refactor
/// kept `.on(&SqlitePool)` working; this test fails at compile time
/// if that contract regresses.
#[test]
fn sqlite_pool_still_typechecks_after_phase25() {
    #[allow(dead_code)]
    async fn _unreachable(sqlite_pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
        let _v: Vec<Article> = Article::objects().on(sqlite_pool).fetch().await?;
        let _f: Option<Article> = Article::objects().on(sqlite_pool).first().await?;
        let _c: i64 = Article::objects().on(sqlite_pool).count().await?;
        let _e: bool = Article::objects().on(sqlite_pool).exists().await?;
        Ok(())
    }
}

// --------------------------------------------------------------------- //
// Full round trip — runs only with UMBRAL_TEST_POSTGRES_URL set.          //
// --------------------------------------------------------------------- //

/// End-to-end against a real Postgres. Boots the pool, creates the
/// table, seeds two rows, and exercises every terminal.
///
/// Set `UMBRAL_TEST_POSTGRES_URL` to a writable Postgres URL to run
/// this. Example:
///
/// ```text
/// UMBRAL_TEST_POSTGRES_URL=postgres://umbral:umbral@localhost/umbral_test \
///     cargo test --test postgres_queryset -- --ignored
/// ```
///
/// The `#[ignore]` keeps CI green when no Postgres server is around.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn full_round_trip_against_real_postgres() {
    let url = std::env::var("UMBRAL_TEST_POSTGRES_URL")
        .expect("UMBRAL_TEST_POSTGRES_URL must be set to run the ignored Postgres test");
    let pool = PgPool::connect(&url)
        .await
        .expect("connect to Postgres at UMBRAL_TEST_POSTGRES_URL");

    // Clean any prior state from a previous run; the table is owned
    // exclusively by this test so a DROP IF EXISTS is safe.
    sqlx::query("DROP TABLE IF EXISTS umbral_phase25_article")
        .execute(&pool)
        .await
        .expect("drop prior table");

    sqlx::query(
        "CREATE TABLE umbral_phase25_article (\
             id BIGSERIAL PRIMARY KEY, \
             title TEXT NOT NULL, \
             body TEXT NOT NULL, \
             published_at TIMESTAMPTZ\
         )",
    )
    .execute(&pool)
    .await
    .expect("create article table");

    // Two rows. One published, one draft.
    sqlx::query(
        "INSERT INTO umbral_phase25_article (title, body, published_at) \
         VALUES ($1, $2, $3), ($4, $5, NULL)",
    )
    .bind("Hello postgres")
    .bind("first article")
    .bind(DateTime::parse_from_rfc3339("2026-05-30T00:00:00Z").unwrap())
    .bind("Draft")
    .bind("not yet")
    .execute(&pool)
    .await
    .expect("insert seed rows");

    // count() — both rows.
    let n = Article::objects()
        .on_pg(&pool)
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "two seeded rows");

    // exists() — true.
    let exists = Article::objects()
        .on_pg(&pool)
        .exists()
        .await
        .expect("exists");
    assert!(exists, "rows exist");

    // first() — returns one row.
    let first = Article::objects()
        .on_pg(&pool)
        .first()
        .await
        .expect("first");
    assert!(first.is_some(), "first should return a row");

    // fetch() — returns both rows; field round-trip is intact.
    let mut rows = Article::objects()
        .on_pg(&pool)
        .fetch()
        .await
        .expect("fetch");
    rows.sort_by_key(|r| r.id);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].title, "Hello postgres");
    assert!(rows[0].published_at.is_some());
    assert_eq!(rows[1].title, "Draft");
    assert!(rows[1].published_at.is_none(), "nullable round-trip");
}
