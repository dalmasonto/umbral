//! Phase 4.2 — Postgres JSON operators.
//!
//! Covers `path_text(...).eq()` / `.ne()` / `.is_null()` / `.is_not_null()`
//! and `has_key(...)` on `JsonCol` / `NullableJsonCol`. Renders against
//! Postgres so the `$N` placeholders and operator tokens resolve
//! correctly. Live PG round-trip behind `#[ignore]`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_phase42_event")]
pub struct Event {
    pub id: i64,
    pub payload: serde_json::Value,
    pub meta: Option<serde_json::Value>,
}

#[test]
fn path_text_single_key_renders_single_arrow_text() {
    // path_text(["author"]) → "payload" ->> $1
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author"]).eq("alice"));
    let sql = qs.to_sql_pg();
    assert!(
        sql.contains("\"payload\""),
        "column should appear quoted; got {sql}"
    );
    assert!(sql.contains("->>"), "single-key path uses ->>; got {sql}");
    // Single-key path must NOT also use the chained -> (only ->>).
    let arrow_count = sql.matches("->").count();
    let text_arrow_count = sql.matches("->>").count();
    // Each `->>` is also counted by `->` (since `->>` starts with `->`).
    // For a single-key path: one `->>` → arrow_count = 1, text_arrow_count = 1.
    assert_eq!(
        arrow_count, text_arrow_count,
        "single-key path should have no plain -> steps; got {sql}"
    );
    assert!(sql.contains("= "), "equality fragment present; got {sql}");
}

#[test]
fn path_text_two_keys_renders_chained_arrows() {
    // path_text(["author", "name"]) → "payload" -> $1 ->> $2
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author", "name"]).eq("alice"));
    let sql = qs.to_sql_pg();
    let arrow_count = sql.matches("->").count();
    let text_arrow_count = sql.matches("->>").count();
    // Two-key path: one `->` step + one `->>` step.
    // arrow_count counts both substrings (3 total: -> for step 1, then ->> matches both `->>` and `->` start).
    // Cleaner check: one `->>` and at least one plain `->` BEFORE it.
    assert_eq!(
        text_arrow_count, 1,
        "should have exactly one ->> in a two-key path; got {sql}"
    );
    // Plain -> steps = total arrows minus the ->> ones. For a 2-key path: 1 plain + 1 ->>.
    assert_eq!(
        arrow_count - text_arrow_count,
        1,
        "should have exactly one plain -> step for a 2-key path; got {sql}"
    );
}

#[test]
fn path_text_three_keys_renders_two_plain_arrows() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["a", "b", "c"]).eq("v"));
    let sql = qs.to_sql_pg();
    let arrow_count = sql.matches("->").count();
    let text_arrow_count = sql.matches("->>").count();
    assert_eq!(text_arrow_count, 1);
    assert_eq!(
        arrow_count - text_arrow_count,
        2,
        "three-key path should have two plain -> steps; got {sql}"
    );
}

#[test]
fn path_text_ne_renders_inequality() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["status"]).ne("draft"));
    let sql = qs.to_sql_pg();
    assert!(sql.contains("<>"), "expected <>; got {sql}");
}

#[test]
fn path_text_is_null_renders_is_null_fragment() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author"]).is_null());
    let sql = qs.to_sql_pg();
    assert!(sql.contains("IS NULL"), "got {sql}");
}

#[test]
fn path_text_is_not_null_renders_is_not_null_fragment() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author"]).is_not_null());
    let sql = qs.to_sql_pg();
    assert!(sql.contains("IS NOT NULL"), "got {sql}");
}

#[test]
fn has_key_renders_question_mark_operator() {
    let qs = Event::objects().filter(event::PAYLOAD.has_key("author"));
    let sql = qs.to_sql_pg();
    // Postgres has-key operator is `?`; sea-query's tokenizer doubles
    // `??` back to a literal `?`. The rendered SQL has a single `?`.
    assert!(sql.contains("?"), "expected ? operator; got {sql}");
    assert!(
        sql.contains("'author'"),
        "key should be inline-quoted; got {sql}"
    );
}

#[test]
fn nullable_json_col_path_text_works() {
    let qs = Event::objects().filter(event::META.path_text(&["v"]).eq("x"));
    let sql = qs.to_sql_pg();
    assert!(sql.contains("\"meta\""));
    assert!(sql.contains("->>"));
}

#[test]
#[should_panic(expected = "path must have at least one segment")]
fn empty_path_panics() {
    let _ = event::PAYLOAD.path_text(&[]);
}

#[test]
fn path_text_typechecks_through_fetch_pg() {
    #[allow(dead_code)]
    async fn _unreachable(pg_pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        let _v: Vec<Event> = Event::objects()
            .filter(event::PAYLOAD.path_text(&["author", "name"]).eq("alice"))
            .filter(event::PAYLOAD.has_key("approved"))
            .fetch_pg(pg_pool)
            .await?;
        Ok(())
    }
}

// =====================================================================
// Phase 4.2.2 — SQLite JSON1 fallback.
//
// The same `path_text(...).eq(...)` and `has_key(...)` calls render
// differently when the resolved pool is SQLite. The predicates carry
// both forms; the QuerySet picks via `to_sql()` (SQLite) vs
// `to_sql_pg()` (Postgres).
// =====================================================================

#[test]
fn path_text_renders_json_extract_under_sqlite() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author", "name"]).eq("alice"));
    let sql = qs.to_sql();
    assert!(
        sql.contains("json_extract"),
        "SQLite render uses json_extract; got {sql}"
    );
    assert!(
        sql.contains("\"payload\""),
        "column should be quoted; got {sql}"
    );
    assert!(
        !sql.contains("->>"),
        "SQLite render should NOT use ->>; got {sql}"
    );
}

#[test]
fn has_key_renders_json_extract_is_not_null_under_sqlite() {
    let qs = Event::objects().filter(event::PAYLOAD.has_key("author"));
    let sql = qs.to_sql();
    assert!(
        sql.contains("json_extract"),
        "SQLite has_key uses json_extract; got {sql}"
    );
    assert!(
        sql.contains("IS NOT NULL"),
        "SQLite has_key uses IS NOT NULL; got {sql}"
    );
}

#[test]
fn path_text_is_null_renders_json_extract_is_null_under_sqlite() {
    let qs = Event::objects().filter(event::PAYLOAD.path_text(&["author"]).is_null());
    let sql = qs.to_sql();
    assert!(
        sql.contains("json_extract"),
        "SQLite is_null uses json_extract; got {sql}"
    );
    assert!(sql.contains("IS NULL"), "got {sql}");
}

/// End-to-end JSON-operator filtering against SQLite. SQLite's JSON1
/// extension is built into sqlx-sqlite by default, so this test runs
/// in CI without any external setup.
#[tokio::test]
async fn json_operators_filter_real_sqlite_rows() {
    use serde_json::json;
    let pool = umbral::db::connect_sqlite("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase42_event ( \
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            payload TEXT NOT NULL, \
            meta TEXT \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let alice = json!({ "author": { "name": "alice" }, "status": "published" });
    let bob = json!({ "author": { "name": "bob" }, "status": "draft" });

    sqlx::query("INSERT INTO umbral_phase42_event (payload) VALUES (?)")
        .bind(alice.to_string())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbral_phase42_event (payload) VALUES (?)")
        .bind(bob.to_string())
        .execute(&pool)
        .await
        .unwrap();

    let alice_only = Event::objects()
        .filter(event::PAYLOAD.path_text(&["author", "name"]).eq("alice"))
        .on(&pool)
        .fetch()
        .await
        .unwrap();
    assert_eq!(alice_only.len(), 1);
    assert_eq!(alice_only[0].payload["author"]["name"], json!("alice"));

    let with_author = Event::objects()
        .filter(event::PAYLOAD.has_key("author"))
        .on(&pool)
        .fetch()
        .await
        .unwrap();
    assert_eq!(with_author.len(), 2);

    let published_only = Event::objects()
        .filter(event::PAYLOAD.path_text(&["status"]).ne("draft"))
        .on(&pool)
        .fetch()
        .await
        .unwrap();
    assert_eq!(published_only.len(), 1);
}

/// Full live round-trip against Postgres. Set `UMBRAL_TEST_POSTGRES_URL`
/// and run with `--ignored`.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn json_operators_filter_real_postgres_rows() {
    use serde_json::json;
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbral_phase42_event")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase42_event ( \
            id BIGSERIAL PRIMARY KEY, \
            payload JSONB NOT NULL, \
            meta JSONB \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO umbral_phase42_event (payload, meta) VALUES ($1, $2)")
        .bind(json!({ "author": { "name": "alice" }, "status": "published" }))
        .bind(Some(json!({ "extra": true })))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbral_phase42_event (payload, meta) VALUES ($1, $2)")
        .bind(json!({ "author": { "name": "bob" }, "status": "draft" }))
        .bind(Option::<serde_json::Value>::None)
        .execute(&pool)
        .await
        .unwrap();

    // path_text deep filter: pick alice's row.
    let alice_only = Event::objects()
        .filter(event::PAYLOAD.path_text(&["author", "name"]).eq("alice"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(alice_only.len(), 1);
    assert_eq!(alice_only[0].payload["author"]["name"], json!("alice"));

    // has_key: every row has `author` at top level → both rows.
    let with_author = Event::objects()
        .filter(event::PAYLOAD.has_key("author"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(with_author.len(), 2);

    // path_text != draft → only published row.
    let published_only = Event::objects()
        .filter(event::PAYLOAD.path_text(&["status"]).ne("draft"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(published_only.len(), 1);
}
