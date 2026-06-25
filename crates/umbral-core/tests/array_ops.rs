//! Phase 4.2.1 — Postgres array operators.
//!
//! Covers the rendered SQL for `ArrayCol::contains`,
//! `contains_all`, `contained_by`, and `overlaps`. Renders the
//! QuerySet directly against `PostgresQueryBuilder` so the tests
//! pin the exact operator + placeholder shape without needing a
//! live PG server. A full live round-trip lives behind `#[ignore]`
//! gated on `UMBRAL_TEST_POSTGRES_URL`.

use sea_query::{PostgresQueryBuilder, SelectStatement};
use sea_query_binder::SqlxBinder;
use umbral::orm::Model;

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_phase421_event")]
pub struct Event {
    pub id: i64,
    pub tags: Vec<String>,
    pub scores: Option<Vec<i64>>,
}

/// Render the QuerySet's underlying SelectStatement as Postgres SQL,
/// without binding through a pool. Gives us a string to assert against.
fn pg_sql_for(query: SelectStatement) -> String {
    let (sql, _values) = query.build_sqlx(PostgresQueryBuilder);
    sql
}

/// Helper: pull the inner `SelectStatement` out of a QuerySet by
/// going through `to_sql` for the placeholder and re-running the
/// manager. Since QuerySet's `query` field is `pub(crate)` we can't
/// reach it directly from an integration test; instead, build the
/// QuerySet manager-first and call `.to_sql()` for the SQLite-style
/// SQL render (which mirrors the Postgres render in shape).
///
/// For the Postgres-shape assertion, the operator tokens (`@>`, `<@`,
/// `&&`) are identical across builders. The placeholder syntax differs
/// (`?` for SQLite, `$N` for Postgres) but the operator detection is
/// what matters here.
fn manager_sql_contains_operator(sql: &str, op: &str) -> bool {
    sql.contains(op)
}

#[test]
fn contains_renders_at_arrow_operator() {
    let qs = Event::objects().filter(event::TAGS.contains("hello"));
    let sql = qs.to_sql();
    assert!(
        manager_sql_contains_operator(&sql, "@>"),
        "expected `@>` operator in rendered SQL; got {sql}"
    );
    assert!(
        sql.contains("\"tags\""),
        "column identifier should appear quoted; got {sql}"
    );
    assert!(
        sql.contains("ARRAY"),
        "rendered SQL should include the ARRAY literal; got {sql}"
    );
}

#[test]
fn contains_all_renders_multi_element_array() {
    let qs = Event::objects().filter(event::TAGS.contains_all(["alpha", "beta", "gamma"]));
    // Render against the Postgres dialect — `$N` placeholders only
    // come through correctly via `to_sql_pg`. `to_sql` uses the SQLite
    // builder and leaves `$N` tokens untouched (the operators are PG-
    // only, so the SQLite path isn't a real consumer here).
    let sql = qs.to_sql_pg();
    assert!(sql.contains("@>"), "expected `@>`; got {sql}");
    assert!(
        sql.contains("ARRAY"),
        "should render an ARRAY literal; got {sql}"
    );
    // Three values → `$1, $2, $3` in the Postgres-rendered SQL.
    assert!(sql.contains("$1"), "expected $1 placeholder; got {sql}");
    assert!(sql.contains("$2"), "expected $2 placeholder; got {sql}");
    assert!(sql.contains("$3"), "expected $3 placeholder; got {sql}");
}

#[test]
fn contained_by_renders_subset_arrow_operator() {
    let qs = Event::objects().filter(event::TAGS.contained_by(["a", "b", "c"]));
    let sql = qs.to_sql();
    assert!(sql.contains("<@"), "expected `<@` operator; got {sql}");
}

#[test]
fn overlaps_renders_amp_amp_operator() {
    let qs = Event::objects().filter(event::SCORES.overlaps([10i64, 20, 30]));
    let sql = qs.to_sql();
    assert!(sql.contains("&&"), "expected `&&` operator; got {sql}");
    assert!(
        sql.contains("\"scores\""),
        "scores column should appear quoted; got {sql}"
    );
}

#[test]
fn empty_contained_by_returns_false_predicate() {
    let empty: Vec<&str> = Vec::new();
    let qs = Event::objects().filter(event::TAGS.contained_by(empty));
    let sql = qs.to_sql();
    // Empty `contained_by` is documented to render as `1 = 0` —
    // "the subset of nothing is empty," which is the honest answer.
    assert!(
        sql.contains("1 = 0"),
        "empty contained_by should render `1 = 0`; got {sql}"
    );
    assert!(
        !sql.contains("<@"),
        "empty contained_by should NOT emit the operator; got {sql}"
    );
}

#[test]
fn empty_overlaps_returns_false_predicate() {
    let empty: Vec<i64> = Vec::new();
    let qs = Event::objects().filter(event::SCORES.overlaps(empty));
    let sql = qs.to_sql();
    assert!(
        sql.contains("1 = 0"),
        "empty overlaps should render `1 = 0`; got {sql}"
    );
}

#[test]
fn empty_contains_all_renders_tautology() {
    let empty: Vec<&str> = Vec::new();
    let qs = Event::objects().filter(event::TAGS.contains_all(empty));
    let sql = qs.to_sql();
    // Postgres semantics: `col @> ARRAY[]` is true for any col. We
    // render the tautology `1 = 1` to skip an empty-array literal.
    assert!(
        sql.contains("1 = 1"),
        "empty contains_all should render `1 = 1`; got {sql}"
    );
}

/// `to_sql_pg()` is the public Postgres-render accessor added in
/// Phase 4.2.1 alongside the array operators. Pins that the dialect-
/// specific render works for the new operator templates.
#[test]
fn to_sql_pg_emits_dollar_placeholders_and_at_arrow() {
    let qs = Event::objects().filter(event::TAGS.contains_all(["x", "y"]));
    let sql = qs.to_sql_pg();
    // The operator string survives the render.
    assert!(sql.contains("@>"), "got {sql}");
    // Two values → at least `$1` and `$2`.
    assert!(sql.contains("$1") && sql.contains("$2"), "got {sql}");
}

/// Type-level pin: the manager surface accepts a `&PgPool` via
/// `.fetch_pg(&pool)` for an array-having model. If the Phase 4.2.1
/// operators interfere with the Phase 4.1 type surface, this fails
/// at compile time.
#[test]
fn array_op_predicates_typecheck_through_fetch_pg() {
    #[allow(dead_code)]
    async fn _unreachable(pg_pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        let _v: Vec<Event> = Event::objects()
            .filter(event::TAGS.contains("alpha"))
            .filter(event::SCORES.overlaps([1i64, 2, 3]))
            .fetch_pg(pg_pool)
            .await?;
        Ok(())
    }
    // Body never runs; the unreachable function's types check at compile.
    let _ = <Event as Model>::TABLE;
}

/// Full live round-trip against Postgres. Set
/// `UMBRAL_TEST_POSTGRES_URL` and run with `--ignored`.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn array_operators_filter_real_postgres_rows() {
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbral_phase421_event")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase421_event ( \
            id BIGSERIAL PRIMARY KEY, \
            tags TEXT[] NOT NULL, \
            scores BIGINT[] \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO umbral_phase421_event (tags, scores) VALUES ($1, $2)")
        .bind(vec!["info".to_string(), "boot".to_string()])
        .bind(Option::<Vec<i64>>::Some(vec![10, 20, 30]))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbral_phase421_event (tags, scores) VALUES ($1, $2)")
        .bind(vec!["wip".to_string()])
        .bind(Option::<Vec<i64>>::None)
        .execute(&pool)
        .await
        .unwrap();

    // contains("info") → first row only.
    let info_only = Event::objects()
        .filter(event::TAGS.contains("info"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(info_only.len(), 1);
    assert_eq!(
        info_only[0].tags,
        vec!["info".to_string(), "boot".to_string()]
    );

    // overlaps([20, 99]) on scores → first row only (20 is shared).
    let overlap_hits = Event::objects()
        .filter(event::SCORES.overlaps([20i64, 99]))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(overlap_hits.len(), 1);

    // contained_by(["info", "boot", "extra"]) on tags → first row.
    let subset = Event::objects()
        .filter(event::TAGS.contained_by(["info", "boot", "extra"]))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(subset.len(), 1);

    let _unused = pg_sql_for(SelectStatement::new());
}
