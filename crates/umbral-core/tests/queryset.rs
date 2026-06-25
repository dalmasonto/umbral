//! End-to-end coverage for the M1 QuerySet against a real SQLite pool.
//!
//! These tests live under `tests/` rather than alongside `src/orm/queryset.rs`
//! for two reasons. First, each file in `tests/` compiles to its own test
//! binary in its own process, so the process-wide `OnceLock`s in
//! `umbral_core::db` start empty for every run and can't be polluted by a
//! sibling test that already called `App::build()`. Second, each test in
//! here threads an explicit pool through `.on(&pool)` so it never touches
//! the ambient pool at all — the resolution rule from
//! `docs/specs/03-orm-querysets.md §pool resolution` puts `.on(&pool)`
//! first, so a fresh per-test in-memory database keeps every case
//! independent without any shared state to reset.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use umbral_core::db;
use umbral_core::orm::Post;
// The column constants live in a sibling `post` module nested inside the
// `post.rs` file (`umbral_core::orm::post::post`). The outer name is the
// file module; the inner one is the field accessor module the queries
// reach for. Re-importing the inner one as `post` matches the call-site
// idiom the spec sketches in `docs/specs/03-orm-querysets.md`.
use umbral_core::orm::post::post;

/// Build a fresh in-memory SQLite pool with the `post` table created and
/// five canonical rows seeded. Each test calls this and gets a clean
/// database, so tests can run in parallel without ordering hazards.
///
/// Seed shape (mirrored by every test that depends on the data):
///
/// | id | title                | body                  | published_at         |
/// |----|----------------------|-----------------------|----------------------|
/// |  1 | Hello world          | first post            | 2026-01-01T00:00:00Z |
/// |  2 | Rust at last         | second post           | 2026-02-15T00:00:00Z |
/// |  3 | DRAFT: ignore        | unpublished thoughts  | NULL                 |
/// |  4 | rust > all           | case-different rust   | 2026-03-01T00:00:00Z |
/// |  5 | The umbral framework  | long post about umbral | 2026-04-10T00:00:00Z |
///
/// The mix covers what the QuerySet surface needs to exercise: an
/// autoincrement primary key, a case-mixed text column for `like` vs
/// `ilike` vs `contains` vs `icontains`, and a nullable datetime column
/// for `is_null` / `is_not_null`.
async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    sqlx::query(
        "CREATE TABLE post (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             title TEXT NOT NULL,\
             body TEXT NOT NULL,\
             published_at DATETIME\
         )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE post should succeed on a fresh in-memory database");

    let seeds: [(i64, &str, &str, Option<DateTime<Utc>>); 5] = [
        (
            1,
            "Hello world",
            "first post",
            Some(parse_ts("2026-01-01T00:00:00Z")),
        ),
        (
            2,
            "Rust at last",
            "second post",
            Some(parse_ts("2026-02-15T00:00:00Z")),
        ),
        (3, "DRAFT: ignore", "unpublished thoughts", None),
        (
            4,
            "rust > all",
            "case-different rust",
            Some(parse_ts("2026-03-01T00:00:00Z")),
        ),
        (
            5,
            "The umbral framework",
            "long post about umbral",
            Some(parse_ts("2026-04-10T00:00:00Z")),
        ),
    ];

    for (id, title, body, published_at) in seeds {
        sqlx::query("INSERT INTO post (id, title, body, published_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(title)
            .bind(body)
            .bind(published_at)
            .execute(&pool)
            .await
            .expect("INSERT into post should succeed");
    }

    pool
}

/// Tiny helper so the seed table reads top-to-bottom without `unwrap`
/// noise on every row.
fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("seed timestamps are valid RFC 3339")
        .with_timezone(&Utc)
}

/// `fetch()` with no filters returns every row in the table.
#[tokio::test]
async fn fetch_returns_all_rows() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .fetch()
        .await
        .expect("unfiltered fetch should succeed");

    assert_eq!(rows.len(), 5, "expected all 5 seeded posts back");
}

/// A primary-key equality filter pins down exactly one row.
#[tokio::test]
async fn filter_eq_returns_matching_row() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(2))
        .fetch()
        .await
        .expect("eq filter should succeed");

    assert_eq!(rows.len(), 1, "post::ID.eq(2) should match a single row");
    assert_eq!(rows[0].id, 2);
    assert_eq!(rows[0].title, "Rust at last");
}

/// `is_not_null` on the nullable column excludes the draft row.
#[tokio::test]
async fn filter_is_not_null_excludes_drafts() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::PUBLISHED_AT.is_not_null())
        .fetch()
        .await
        .expect("is_not_null filter should succeed");

    assert_eq!(rows.len(), 4, "4 of the 5 seeds have a non-null timestamp");
    assert!(
        rows.iter().all(|p| p.published_at.is_some()),
        "every returned row should have a non-null published_at",
    );
}

/// `is_null` returns just the one draft row.
#[tokio::test]
async fn filter_is_null_returns_drafts() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::PUBLISHED_AT.is_null())
        .fetch()
        .await
        .expect("is_null filter should succeed");

    assert_eq!(rows.len(), 1, "only the draft row has a null published_at");
    assert_eq!(rows[0].id, 3);
    assert!(rows[0].published_at.is_none());
}

/// `like` builds a SQL `LIKE` predicate. The case-sensitivity of `LIKE`
/// is a backend-level decision: SQLite's default `LIKE` is case-insensitive
/// for ASCII (so this query also picks up the lowercase "rust > all"),
/// while Postgres's `LIKE` is case-sensitive. The framework's contract is
/// just "emit `LIKE`"; the asymmetry across backends is documented in
/// `docs/specs/03-orm-querysets.md`. This test pins the portable
/// behaviour: the predicate matches every title whose ASCII letters spell
/// out "rust" at the start.
#[tokio::test]
async fn filter_like_matches_rust_prefix_on_sqlite() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::TITLE.like("Rust%"))
        .fetch()
        .await
        .expect("like filter should succeed");

    let mut titles: Vec<&str> = rows.iter().map(|p| p.title.as_str()).collect();
    titles.sort();
    assert_eq!(
        titles,
        vec!["Rust at last", "rust > all"],
        "SQLite's LIKE is ASCII-case-insensitive by default, so 'Rust%' \
         matches both 'Rust at last' and 'rust > all'",
    );
}

/// `ilike` is case-insensitive, picking up both "Rust" and "rust" titles.
#[tokio::test]
async fn filter_ilike_is_case_insensitive() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::TITLE.ilike("rust%"))
        .fetch()
        .await
        .expect("ilike filter should succeed");

    assert_eq!(
        rows.len(),
        2,
        "ILIKE 'rust%' should match both 'Rust at last' and 'rust > all'",
    );
}

/// `contains` wraps the substring in `%...%` and emits `LIKE '%val%'`.
/// As with bare `LIKE`, case-sensitivity depends on the backend; on
/// SQLite both rust-titled posts match `contains("rust")` because LIKE
/// is ASCII-case-insensitive there. The portable contract is just "wrap
/// in `%...%` and emit `LIKE`".
#[tokio::test]
async fn filter_contains_matches_rust_substring_on_sqlite() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::TITLE.contains("rust"))
        .fetch()
        .await
        .expect("contains filter should succeed");

    let mut titles: Vec<&str> = rows.iter().map(|p| p.title.as_str()).collect();
    titles.sort();
    assert_eq!(
        titles,
        vec!["Rust at last", "rust > all"],
        "SQLite's LIKE matches ASCII-insensitively, so contains('rust') \
         finds both 'Rust at last' and 'rust > all'",
    );
}

/// `icontains` does the same `%...%` wrap but case-insensitively, so it
/// pulls back both titles regardless of capitalisation.
#[tokio::test]
async fn filter_icontains_is_case_insensitive() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::TITLE.icontains("rust"))
        .fetch()
        .await
        .expect("icontains filter should succeed");

    assert_eq!(
        rows.len(),
        2,
        "icontains('rust') should pick up both 'Rust at last' and 'rust > all'",
    );
    let mut titles: Vec<&str> = rows.iter().map(|p| p.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, vec!["Rust at last", "rust > all"]);
}

/// ORM-1 regression: `contains` / `startswith` treat their argument as a
/// literal substring, so LIKE wildcards (`%`, `_`) in it must match
/// *literally*, not as wildcards. Before the escaping fix, `contains("50%")`
/// emitted `LIKE '%50%%'` and over-matched every title containing "50";
/// `contains("a_b")` matched "axb". With escaping it matches only the rows
/// that literally contain the typed characters.
#[tokio::test]
async fn contains_escapes_like_wildcards_in_user_input() {
    let pool = fresh_pool().await;

    // Extra rows whose titles distinguish "literal match" from "wildcard
    // over-match". ids start at 6 to avoid the 1..=5 seed.
    let extra: [(i64, &str); 4] = [
        (6, "50% discount today"), // literally contains "50%"
        (7, "500 items left"),     // contains "50" but NOT "50%"
        (8, "grep a_b please"),    // literally contains "a_b"
        (9, "grep axb please"),    // matches "a_b" only if `_` is a wildcard
    ];
    for (id, title) in extra {
        sqlx::query("INSERT INTO post (id, title, body, published_at) VALUES (?, ?, ?, NULL)")
            .bind(id)
            .bind(title)
            .bind("wildcard seed body")
            .execute(&pool)
            .await
            .expect("seed wildcard row");
    }

    // `%` must be literal: only the "50% discount" row, never "500 items".
    let pct: Vec<String> = Post::objects()
        .on(&pool)
        .filter(post::TITLE.contains("50%"))
        .fetch()
        .await
        .expect("contains('50%')")
        .into_iter()
        .map(|p| p.title)
        .collect();
    assert_eq!(
        pct,
        vec!["50% discount today".to_string()],
        "contains('50%') must match the literal percent, not '500 items'",
    );

    // `_` must be literal: only "a_b", never "axb".
    let underscore: Vec<String> = Post::objects()
        .on(&pool)
        .filter(post::TITLE.contains("a_b"))
        .fetch()
        .await
        .expect("contains('a_b')")
        .into_iter()
        .map(|p| p.title)
        .collect();
    assert_eq!(
        underscore,
        vec!["grep a_b please".to_string()],
        "contains('a_b') must treat '_' literally, not as a single-char wildcard",
    );

    // startswith honours the same escaping.
    let starts: Vec<String> = Post::objects()
        .on(&pool)
        .filter(post::TITLE.startswith("50%"))
        .fetch()
        .await
        .expect("startswith('50%')")
        .into_iter()
        .map(|p| p.title)
        .collect();
    assert_eq!(starts, vec!["50% discount today".to_string()]);
}

/// `&` composes two predicates as SQL `AND`: published AND title contains
/// "rust" lands the two published Rust-flavoured posts.
#[tokio::test]
async fn compose_predicates_with_and() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::PUBLISHED_AT.is_not_null() & post::TITLE.icontains("rust"))
        .fetch()
        .await
        .expect("AND-composed filter should succeed");

    assert_eq!(
        rows.len(),
        2,
        "is_not_null AND icontains('rust') should match exactly the two published rust posts",
    );
}

/// `|` composes two predicates as SQL `OR`: id=1 OR id=3 returns both.
#[tokio::test]
async fn compose_predicates_with_or() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(1) | post::ID.eq(3))
        .fetch()
        .await
        .expect("OR-composed filter should succeed");

    assert_eq!(rows.len(), 2);
    let mut ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 3]);
}

/// `order_by(desc)` + `limit` returns the last N rows in descending order.
#[tokio::test]
async fn order_by_desc_and_limit() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .order_by(post::ID.desc())
        .limit(2)
        .fetch()
        .await
        .expect("ordered + limited fetch should succeed");

    assert_eq!(rows.len(), 2, "limit(2) caps the result at two rows");
    assert_eq!(rows[0].id, 5, "first row should be the largest id");
    assert_eq!(rows[1].id, 4, "second row should be the next id down");
}

/// `offset` after `order_by` skips the first N rows.
#[tokio::test]
async fn order_by_with_offset_skips_leading_rows() {
    let pool = fresh_pool().await;

    let rows = Post::objects()
        .on(&pool)
        .order_by(post::ID.asc())
        .offset(2)
        .limit(2)
        .fetch()
        .await
        .expect("ordered + offset + limit fetch should succeed");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, 3, "offset(2) skips the first two ascending ids");
    assert_eq!(rows[1].id, 4);
}

/// `count` with no filter counts every row in the table.
#[tokio::test]
async fn count_returns_total_rows() {
    let pool = fresh_pool().await;

    let total = Post::objects()
        .on(&pool)
        .count()
        .await
        .expect("count() should succeed");

    assert_eq!(total, 5);
}

/// `count` respects the active filters.
#[tokio::test]
async fn count_with_filter_counts_matching_rows() {
    let pool = fresh_pool().await;

    let total = Post::objects()
        .on(&pool)
        .filter(post::PUBLISHED_AT.is_not_null())
        .count()
        .await
        .expect("filtered count() should succeed");

    assert_eq!(total, 4, "4 seeded rows have a non-null published_at");
}

/// `exists` is true when the filter matches at least one row.
#[tokio::test]
async fn exists_returns_true_when_match() {
    let pool = fresh_pool().await;

    let found = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(1))
        .exists()
        .await
        .expect("exists() should succeed");

    assert!(found, "id=1 is seeded, so exists() must be true");
}

/// `exists` is false when no row matches.
#[tokio::test]
async fn exists_returns_false_when_no_match() {
    let pool = fresh_pool().await;

    let found = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(999))
        .exists()
        .await
        .expect("exists() should succeed");

    assert!(!found, "id=999 is not seeded, so exists() must be false");
}

/// `.get()` returns the row when exactly one matches.
#[tokio::test]
async fn get_returns_row_when_exactly_one_matches() {
    use umbral_core::orm::GetError;
    let pool = fresh_pool().await;

    let row = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(2))
        .get()
        .await
        .expect("get() should succeed on a unique PK match");

    assert_eq!(row.id, 2);
    assert_eq!(row.title, "Rust at last");
    let _ = GetError::NotFound;
}

/// `.get()` returns `GetError::NotFound` when zero rows match.
#[tokio::test]
async fn get_returns_not_found_when_zero_rows_match() {
    use umbral_core::orm::GetError;
    let pool = fresh_pool().await;

    let err = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(999))
        .get()
        .await
        .expect_err("get() should error on no match");

    assert!(matches!(err, GetError::NotFound), "got {err:?}");
}

/// `.get()` returns `GetError::MultipleObjectsReturned` when more than
/// one row matches — a non-unique filter is the classic case.
#[tokio::test]
async fn get_returns_multiple_when_filter_is_not_unique() {
    use umbral_core::orm::GetError;
    let pool = fresh_pool().await;

    // No filter — table has 5 rows, so .get() must reject.
    let err = Post::objects()
        .on(&pool)
        .get()
        .await
        .expect_err("get() with no filter on a multi-row table should error");

    assert!(
        matches!(err, GetError::MultipleObjectsReturned),
        "got {err:?}"
    );
}

/// `first` returns `Some` when at least one row matches.
#[tokio::test]
async fn first_returns_some_when_match() {
    let pool = fresh_pool().await;

    let row = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(1))
        .first()
        .await
        .expect("first() should succeed");

    let row = row.expect("id=1 is seeded, so first() must return Some");
    assert_eq!(row.id, 1);
    assert_eq!(row.title, "Hello world");
}

/// `first` returns `None` when nothing matches the filter.
#[tokio::test]
async fn first_returns_none_when_no_match() {
    let pool = fresh_pool().await;

    let row = Post::objects()
        .on(&pool)
        .filter(post::ID.eq(999))
        .first()
        .await
        .expect("first() should succeed");

    assert!(
        row.is_none(),
        "id=999 is not seeded, so first() must be None"
    );
}

/// `QuerySet::to_sql` renders the prepared statement without
/// executing it — useful for debugging and pinning the shape of the
/// generated SQL. The rendered string carries `?` placeholders for
/// every bound value, which is what sqlx would send to the driver.
///
/// No pool is required; the method is pure rendering.
#[test]
fn to_sql_renders_the_select_without_executing() {
    let sql = Post::objects()
        .filter(post::TITLE.eq("hello"))
        .order_by(post::ID.asc())
        .limit(5)
        .to_sql();

    // Spot-check the salient pieces. We don't pin the exact string
    // because sea-query's formatter may evolve; we pin the
    // invariants instead.
    let lower = sql.to_ascii_lowercase();
    assert!(
        lower.contains("select"),
        "expected SELECT keyword; got {sql}"
    );
    assert!(
        lower.contains("from \"post\""),
        "expected FROM \"post\"; got {sql}"
    );
    assert!(
        lower.contains("where") && lower.contains("\"title\" = ?"),
        "expected WHERE clause with bound placeholder for title; got {sql}",
    );
    assert!(
        lower.contains("order by \"id\" asc"),
        "expected ORDER BY id asc; got {sql}",
    );
    assert!(lower.contains("limit"), "expected LIMIT clause; got {sql}");
    assert!(
        !sql.contains("hello"),
        "the bound value MUST stay out of the SQL string (it's a parameter, not literal); got {sql}",
    );
}
