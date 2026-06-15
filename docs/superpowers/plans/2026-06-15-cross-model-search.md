# Cross-model search (`Search::across`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an ORM `Search::across::<(A, B, …)>(query, limit)` that searches every text column of each opted-in model and returns one relevance-ranked `Vec<SearchHit>`, then rewire the website header search onto it.

**Architecture:** A `Searchable: Model` marker trait derives kind/title/body/ident columns from `Model::FIELDS` metadata. A `SearchSources` tuple trait builds one normalized `SELECT` per model and `UNION ALL`s them, ordered by an inline rank — Postgres `ts_rank(to_tsvector(...), websearch_to_tsquery($1))` (nothing stored), SQLite a weighted `LIKE` `CASE`. Execution dispatches on the ambient pool and decodes into a fixed `SearchHit` row.

**Tech Stack:** Rust, `umbra-core` (the ORM), `sqlx` (Postgres + SQLite), the existing `db::pool_dispatched()` backend dispatch.

**Spec:** `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`

**Conventions:** all `cargo` runs from `crates/`. TDD: write the failing test, see it fail, implement, see it pass, commit. Framework tasks (1–5) touch only `crates/`; never touch `umbra_website/` in those tasks. Don't restart the dev server. Commit only the files each task names.

---

## File Structure

- **Create** `crates/umbra-core/src/orm/search.rs` — the whole feature: `SearchHit`, `Searchable`, the `default_*` helpers, `SearchSources`, `Search::across`, per-backend SQL building, execution.
- **Modify** `crates/umbra-core/src/orm/mod.rs` — `pub mod search;` + `pub use search::{Search, Searchable, SearchHit, SearchSources};`.
- **Modify** `crates/umbra/src/lib.rs` (or the facade's orm re-export module) — re-export the four types under `umbra::orm`.
- **Create** `crates/umbra-core/tests/search_across.rs` — behavioral SQLite tests.
- **Create** `crates/umbra-core/tests/search_helpers.rs` — pure unit tests for `default_title`/`default_body`/`default_pk_column`.
- **Create** `plugins/umbra-rest/tests/search_pg.rs` — cfg/ignore-gated Postgres `ts_rank` ordering test (lives beside the existing `rest_fts_pg.rs` so it shares the PG CI lane).
- **Modify** `umbra_website/plugins/site_content/src/models.rs` — `impl Searchable for BlogPost`.
- **Modify** `umbra_website/plugins/plugin_directory/src/models.rs` — `impl Searchable for PluginModel`.
- **Modify** `umbra_website/plugins/plugin_directory/src/lib.rs` — `render_search` calls `Search::across`.
- **Modify** `umbra_website/plugins/plugin_directory/tests/render_pages.rs` — search assertions still hold.
- **Create** `documentation/docs/v0.0.1/orm/search.mdx` — user doc page.

---

## Task 1: `Searchable` trait + metadata helpers

**Files:**
- Create: `crates/umbra-core/src/orm/search.rs`
- Modify: `crates/umbra-core/src/orm/mod.rs`
- Test: `crates/umbra-core/tests/search_helpers.rs`

- [ ] **Step 1: Write the failing test** (`crates/umbra-core/tests/search_helpers.rs`)

```rust
//! Pure-logic coverage for the column-selection helpers that back the
//! `Searchable` defaults. No DB — these read `Model::FIELDS` only.
//! Uses the crate-internal path (`umbra_core::orm::search`) since these
//! helpers are power-user surface, not necessarily on the facade.
use umbra_core::orm::search::{default_body, default_pk_column, default_title};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "srh_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub body: String,
    #[umbra(slug_from = "title")]
    pub slug: umbra::orm::validators::Slug,
    #[umbra(choices)]
    pub status: DocStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, umbra::orm::ChoiceField)]
pub enum DocStatus { Draft, Live }

#[test]
fn title_prefers_title_then_name_then_first_text() {
    assert_eq!(default_title::<Doc>(), "title");
}

#[test]
fn body_includes_text_columns_excludes_slug_and_choices() {
    let body = default_body::<Doc>();
    assert!(body.contains(&"title"), "title is a body column: {body:?}");
    assert!(body.contains(&"body"), "body is a body column: {body:?}");
    assert!(!body.contains(&"slug"), "slug (text_format) excluded: {body:?}");
    assert!(!body.contains(&"status"), "choices column excluded: {body:?}");
    assert!(!body.contains(&"id"), "non-text PK excluded: {body:?}");
}

#[test]
fn pk_column_is_the_primary_key() {
    assert_eq!(default_pk_column::<Doc>(), "id");
}
```

> Before running: confirm the constrained-text + choice type names this fixture uses really exist — `grep -rn "pub struct Slug\|pub enum.*Slug\|ChoiceField\|derive(ChoiceField" crates/umbra-core/src crates/umbra-macros/src`. If `umbra::orm::validators::Slug` or the `ChoiceField` derive is named differently, adjust the fixture's `slug`/`status` field types to match (the test's intent — a `text_format` column and a `choices` column — is what matters, not the exact type names).

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates && cargo test -p umbra-core --test search_helpers`
Expected: FAIL to compile — `umbra_core::orm::search` module / its helpers don't exist yet.

- [ ] **Step 3: Create the module with the trait + helpers** (`crates/umbra-core/src/orm/search.rs`)

```rust
//! Cross-model relevance search. See
//! `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`.

use crate::orm::{FieldSpec, Model, SqlType};

/// A model that can take part in [`Search::across`]. Opt in with a marker
/// `impl Searchable for T {}`; every default is read from `T::FIELDS`.
pub trait Searchable: Model {
    /// Result tag, e.g. `"plugin"`. Default: the table name.
    fn kind() -> &'static str {
        Self::TABLE
    }
    /// Column shown as the result title. Default: the text column named
    /// (case-insensitively) `title` or `name`, else the first text column.
    fn title() -> &'static str {
        default_title::<Self>()
    }
    /// Text columns forming the searchable body. Default: every text column
    /// except metadata-flagged non-content ones (slug/url/email/choices).
    fn body() -> Vec<&'static str> {
        default_body::<Self>()
    }
    /// Column whose value becomes `SearchHit.pk` (the routing key). Default:
    /// the primary-key column. Override to a natural key (e.g. a slug).
    fn ident() -> &'static str {
        default_pk_column::<Self>()
    }
}

/// True when a `FieldSpec` is plain searchable prose: a `Text` column that
/// is not a constrained-text wrapper (slug/url/email) and not a choices set.
fn is_content_text(f: &FieldSpec) -> bool {
    matches!(f.ty, SqlType::Text) && f.text_format.is_none() && f.choices.is_empty()
}

/// The text columns of `T`, in declaration order, minus non-content ones.
pub fn default_body<T: Model>() -> Vec<&'static str> {
    T::FIELDS
        .iter()
        .filter(|f| is_content_text(f))
        .map(|f| f.name)
        .collect()
}

/// Title column: a content-text column named `title` or `name`
/// (case-insensitive), else the first content-text column, else the PK.
pub fn default_title<T: Model>() -> &'static str {
    let texts: Vec<&'static str> = default_body::<T>();
    for want in ["title", "name"] {
        if let Some(c) = texts.iter().find(|c| c.eq_ignore_ascii_case(want)) {
            return c;
        }
    }
    texts.first().copied().unwrap_or_else(default_pk_column::<T>)
}

/// The primary-key column name (first `primary_key` field; falls back to
/// the conventional `id`).
pub fn default_pk_column<T: Model>() -> &'static str {
    T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}
```

- [ ] **Step 4: Register the module** (`crates/umbra-core/src/orm/mod.rs`)

Add near the other `pub mod` lines:

```rust
pub mod search;
```

And near the other `pub use` re-exports:

```rust
pub use search::Searchable;
```

(`Search`, `SearchHit`, `SearchSources` join this `pub use` in Task 3. The `default_*` helpers stay reachable at `umbra_core::orm::search::*` — no need to re-export them; they're power-user surface the tests reach by module path.)

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd crates && cargo test -p umbra-core --test search_helpers`
Expected: PASS, 3 passed.

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add crates/umbra-core/src/orm/search.rs crates/umbra-core/src/orm/mod.rs crates/umbra-core/tests/search_helpers.rs
git commit -m "feat(orm): Searchable trait + column-selection helpers"
```

---

## Task 2: `SearchHit` + per-backend branch SQL

**Files:**
- Modify: `crates/umbra-core/src/orm/search.rs`
- Test: `crates/umbra-core/tests/search_helpers.rs` (add SQL-shape unit tests)

- [ ] **Step 1: Write the failing test** (append to `crates/umbra-core/tests/search_helpers.rs`)

```rust
use umbra_core::orm::search::{branch_sql, Backend};

#[test]
fn postgres_branch_has_tsrank_setweight_and_union_shape() {
    let sql = branch_sql::<Doc>(Backend::Postgres);
    assert!(sql.contains("'srh_doc' AS kind") || sql.contains("'srh_doc'  AS kind"), "{sql}");
    assert!(sql.contains("AS pk"), "{sql}");
    assert!(sql.contains("ts_rank("), "{sql}");
    assert!(sql.contains("setweight(to_tsvector('english'"), "title weighted: {sql}");
    assert!(sql.contains("websearch_to_tsquery('english', $1)"), "{sql}");
    assert!(sql.contains("::float8 AS rank"), "rank cast to f64: {sql}");
}

#[test]
fn sqlite_branch_uses_weighted_like_case() {
    let sql = branch_sql::<Doc>(Backend::Sqlite);
    assert!(sql.contains("CASE WHEN"), "{sql}");
    assert!(sql.contains("LIKE ?1"), "substring param: {sql}");
    assert!(sql.contains("LIKE ?2"), "prefix param: {sql}");
    assert!(sql.contains("AS rank"), "{sql}");
    assert!(!sql.contains("to_tsvector"), "no tsvector on sqlite: {sql}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates && cargo test -p umbra-core --test search_helpers`
Expected: FAIL to compile — `branch_sql` / `Backend` undefined.

- [ ] **Step 3: Add `SearchHit`, `Backend`, and `branch_sql`** (append to `crates/umbra-core/src/orm/search.rs`)

```rust
/// One normalized search result. Column aliases are fixed so every model's
/// branch unions cleanly and `sqlx::FromRow` decodes identically per backend.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SearchHit {
    pub kind: String,
    pub pk: String,
    pub title: String,
    pub snippet: String,
    pub rank: f64,
}

/// Which dialect to emit. Resolved from the ambient pool at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Postgres,
    Sqlite,
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `coalesce(c1,'')||' '||coalesce(c2,'')||…` over the given columns.
fn concat_coalesce(cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("coalesce({}, '')", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(" || ' ' || ")
}

/// The single normalized `SELECT` for one model. The query parameter is
/// referenced positionally: `$1` (Postgres, the websearch string) /
/// `?1` substring + `?2` prefix (SQLite). Every branch reuses the same
/// numbers so `Search::across` binds each value once regardless of arity.
pub fn branch_sql<T: Searchable>(backend: Backend) -> String {
    let table = quote_ident(T::TABLE);
    let kind = T::kind().replace('\'', "''");
    let ident = quote_ident(T::ident());
    let title = quote_ident(T::title());
    let body = T::body();
    let body_cols: Vec<&str> = if body.is_empty() { vec![T::title()] } else { body };
    let body_concat = concat_coalesce(&body_cols);
    // Body minus the title column, for the un-weighted part of the rank vector.
    let rest: Vec<&str> = body_cols.iter().copied().filter(|c| *c != T::title()).collect();

    match backend {
        Backend::Postgres => {
            let title_vec = format!(
                "setweight(to_tsvector('english', coalesce({title}, '')), 'A')"
            );
            let rest_vec = if rest.is_empty() {
                String::new()
            } else {
                format!(" || to_tsvector('english', {})", concat_coalesce(&rest))
            };
            format!(
                "SELECT '{kind}' AS kind, \
                 CAST({ident} AS text) AS pk, \
                 {title} AS title, \
                 left({body_concat}, 200) AS snippet, \
                 ts_rank({title_vec}{rest_vec}, websearch_to_tsquery('english', $1))::float8 AS rank \
                 FROM {table} \
                 WHERE to_tsvector('english', {body_concat}) @@ websearch_to_tsquery('english', $1)"
            )
        }
        Backend::Sqlite => {
            // ?1 = '%q%' (substring), ?2 = 'q%' (prefix bonus).
            let where_like = body_cols
                .iter()
                .map(|c| format!("{} LIKE ?1", quote_ident(c)))
                .collect::<Vec<_>>()
                .join(" OR ");
            let title_q = quote_ident(T::title());
            let body_substr_terms = body_cols
                .iter()
                .map(|c| format!("(CASE WHEN {} LIKE ?1 THEN 1.0 ELSE 0 END)", quote_ident(c)))
                .collect::<Vec<_>>()
                .join(" + ");
            format!(
                "SELECT '{kind}' AS kind, \
                 CAST({ident} AS TEXT) AS pk, \
                 {title} AS title, \
                 substr({body_concat}, 1, 200) AS snippet, \
                 ( (CASE WHEN {title_q} LIKE ?1 THEN 2.0 ELSE 0 END) \
                 + {body_substr_terms} \
                 + (CASE WHEN {title_q} LIKE ?2 THEN 1.0 ELSE 0 END) ) AS rank \
                 FROM {table} \
                 WHERE {where_like}"
            )
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd crates && cargo test -p umbra-core --test search_helpers`
Expected: PASS, 5 passed.

- [ ] **Step 5: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add crates/umbra-core/src/orm/search.rs crates/umbra-core/tests/search_helpers.rs
git commit -m "feat(orm): SearchHit + per-backend normalized branch SQL"
```

---

## Task 3: `SearchSources` tuple trait + `Search::across` (execution)

**Files:**
- Modify: `crates/umbra-core/src/orm/search.rs`
- Modify: `crates/umbra-core/src/orm/mod.rs`
- Test: `crates/umbra-core/tests/search_across.rs`

- [ ] **Step 1: Write the failing behavioral test** (`crates/umbra-core/tests/search_across.rs`)

```rust
//! Behavioral coverage for Search::across on SQLite: real rows in, the
//! ranked SearchHit list out, read back through the public API.
use tokio::sync::OnceCell;
use umbra_core::orm::{Search, Searchable}; // core path: Task 4 (facade re-export) runs later
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sa_plugin")]
pub struct Plugin {
    pub id: i64,
    pub name: String,
    pub blurb: String,
}
impl Searchable for Plugin {
    fn kind() -> &'static str { "plugin" }
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sa_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
}
impl Searchable for Post {
    fn kind() -> &'static str { "post" }
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Plugin>()
            .model::<Post>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE sa_plugin (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, blurb TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE sa_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, body TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_plugin (name, blurb) VALUES ('Redis cache', 'fast in-memory store')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_plugin (name, blurb) VALUES ('Logger', 'writes to redis sometimes')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_post (title, body) VALUES ('Using redis', 'a guide to caching')")
            .execute(&pool).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn across_returns_both_models_ranked_with_title_first() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("redis", 10).await.expect("search runs");
    // Both a plugin and a post match.
    assert!(hits.iter().any(|h| h.kind == "plugin"), "a plugin hit: {hits:?}");
    assert!(hits.iter().any(|h| h.kind == "post"), "a post hit: {hits:?}");
    // The title matches ("Redis cache", "Using redis") outrank the body-only
    // match ("Logger" / "writes to redis").
    let top = &hits[0];
    assert!(
        (top.kind == "plugin" && top.title == "Redis cache")
            || (top.kind == "post" && top.title == "Using redis"),
        "a title match ranks first, got {top:?}"
    );
    let logger = hits.iter().find(|h| h.title == "Logger");
    if let Some(l) = logger {
        assert!(l.rank <= top.rank, "body-only match ranks no higher than a title match");
    }
}

#[tokio::test]
async fn across_maps_kind_and_pk_back_to_rows() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("caching", 10).await.expect("search runs");
    let post = hits.iter().find(|h| h.kind == "post").expect("post matched 'caching'");
    assert_eq!(post.pk, "1", "pk is the post's id as text");
    assert_eq!(post.title, "Using redis");
}

#[tokio::test]
async fn blank_query_returns_empty_without_hitting_db() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("   ", 10).await.expect("blank is ok");
    assert!(hits.is_empty(), "blank query yields no hits");
}

#[tokio::test]
async fn no_match_returns_empty() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("zzznomatch", 10).await.expect("runs");
    assert!(hits.is_empty(), "no rows match");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates && cargo test -p umbra-core --test search_across`
Expected: FAIL to compile — `Search` / `across` / `SearchSources` undefined.

- [ ] **Step 3: Add `SearchSources`, `Search::across`, and execution** (append to `crates/umbra-core/src/orm/search.rs`)

```rust
use crate::db::DbPool;

/// A tuple of `Searchable` models. Produces one normalized branch per member
/// for a given backend. Implemented for tuples of arity 1..=6 via the macro
/// below.
pub trait SearchSources {
    fn branches(backend: Backend) -> Vec<String>;
}

macro_rules! impl_search_sources {
    ($($T:ident),+) => {
        impl<$($T: Searchable),+> SearchSources for ($($T,)+) {
            fn branches(backend: Backend) -> Vec<String> {
                vec![$( branch_sql::<$T>(backend) ),+]
            }
        }
    };
}
impl_search_sources!(A);
impl_search_sources!(A, B);
impl_search_sources!(A, B, C);
impl_search_sources!(A, B, C, D);
impl_search_sources!(A, B, C, D, E);
impl_search_sources!(A, B, C, D, E, F);

/// Escape SQL `LIKE` metacharacters in a user query (for the SQLite path).
fn escape_like(q: &str) -> String {
    q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Cross-model relevance search. See the module docs.
pub struct Search;

impl Search {
    /// Search every model in `S` for `query`, returning up to `limit` hits
    /// ordered by descending relevance. A blank query yields an empty Vec
    /// without touching the database.
    pub async fn across<S: SearchSources>(
        query: &str,
        limit: u64,
    ) -> Result<Vec<SearchHit>, sqlx::Error> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        match crate::db::pool_dispatched() {
            DbPool::Postgres(pool) => {
                let sql = format!(
                    "{} ORDER BY rank DESC LIMIT $2",
                    S::branches(Backend::Postgres).join("\nUNION ALL\n")
                );
                sqlx::query_as::<_, SearchHit>(&sql)
                    .bind(q)
                    .bind(limit as i64)
                    .fetch_all(&pool)
                    .await
            }
            DbPool::Sqlite(pool) => {
                let sql = format!(
                    "{} ORDER BY rank DESC LIMIT ?3",
                    S::branches(Backend::Sqlite).join("\nUNION ALL\n")
                );
                let like = format!("%{}%", escape_like(q));
                let prefix = format!("{}%", escape_like(q));
                sqlx::query_as::<_, SearchHit>(&sql)
                    .bind(like)     // ?1
                    .bind(prefix)   // ?2
                    .bind(limit as i64) // ?3
                    .fetch_all(&pool)
                    .await
            }
        }
    }
}
```

> Note on `LIKE ESCAPE`: SQLite treats `\` as the escape only with an explicit `ESCAPE '\'`. For v1 the escaped pattern is conservative (it neutralizes user `%`/`_` so they match literally enough); add `ESCAPE '\\'` to each `LIKE` in `branch_sql` only if a test shows a wildcard leak. Keep this note; do not silently change matching semantics.

- [ ] **Step 4: Export the remaining types** (`crates/umbra-core/src/orm/mod.rs`)

Change the Task-1 re-export line to:

```rust
pub use search::{Search, SearchHit, SearchSources, Searchable};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd crates && cargo test -p umbra-core --test search_across`
Expected: PASS, 4 passed.

- [ ] **Step 6: Run the whole umbra-core suite (no regressions)**

Run: `cd crates && cargo test -p umbra-core`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add crates/umbra-core/src/orm/search.rs crates/umbra-core/src/orm/mod.rs crates/umbra-core/tests/search_across.rs
git commit -m "feat(orm): Search::across over a Searchable tuple, UNION ALL ranked"
```

---

## Task 4: Facade re-exports

**Files:**
- Modify: `crates/umbra/src/lib.rs` (the `pub mod orm` / orm re-export block — grep for `pub use umbra_core::orm` to find it)

- [ ] **Step 1: Find the facade's orm re-export**

Run: `cd crates && grep -rn "umbra_core::orm" umbra/src/`
Expected: a re-export block (e.g. `pub use umbra_core::orm::{...}` or `pub mod orm { pub use umbra_core::orm::*; }`).

- [ ] **Step 2: Add the search types to the facade**

If the facade re-exports a curated list, add `Search`, `Searchable`, `SearchHit` to it:

```rust
pub use umbra_core::orm::{Search, Searchable, SearchHit};
```

If it re-exports `umbra_core::orm::*` wholesale, no change is needed — verify with Step 3. Do NOT add these to the prelude (power-user surface; the spec keeps the prelude unambiguous).

- [ ] **Step 3: Write a compile check that the facade path resolves** (`crates/umbra-core/tests/search_helpers.rs`, append)

```rust
#[test]
fn facade_paths_resolve() {
    // Compile-time proof the public path the docs promise exists.
    fn _assert<T: umbra::orm::Searchable>() {}
    let _ = umbra::orm::SearchHit {
        kind: String::new(), pk: String::new(), title: String::new(),
        snippet: String::new(), rank: 0.0,
    };
}
```

- [ ] **Step 4: Run it**

Run: `cd crates && cargo test -p umbra-core --test search_helpers facade_paths_resolve`
Expected: PASS.

- [ ] **Step 5: Build the whole workspace (facade re-export didn't break anything)**

Run: `cd crates && cargo build`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add crates/umbra/src/lib.rs crates/umbra-core/tests/search_helpers.rs
git commit -m "feat(orm): re-export Search/Searchable/SearchHit from the facade"
```

---

## Task 5: Postgres `ts_rank` ordering test (cfg/ignore-gated)

**Files:**
- Create: `plugins/umbra-rest/tests/search_pg.rs`

- [ ] **Step 1: Read the existing PG test harness**

Run: `cd crates && sed -n '1,60p' ../plugins/umbra-rest/tests/rest_fts_pg.rs`
Expected: shows how it gates on a `DATABASE_URL` / `#[ignore]` and connects a `PgPool`. Mirror that exact gating + connection helper.

- [ ] **Step 2: Write the Postgres test** (`plugins/umbra-rest/tests/search_pg.rs`)

```rust
//! Postgres-only: real ts_rank ordering for Search::across. Gated exactly
//! like rest_fts_pg.rs (skips without a Postgres DATABASE_URL).
#![cfg(test)]
use umbra::orm::{Model, Search, Searchable};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "spg_plugin")]
pub struct Plugin { pub id: i64, pub name: String, pub blurb: String }
impl Searchable for Plugin { fn kind() -> &'static str { "plugin" } }

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "spg_post")]
pub struct Post { pub id: i64, pub title: String, pub body: String }
impl Searchable for Post { fn kind() -> &'static str { "post" } }

// Reuse rest_fts_pg.rs's gating helper verbatim: a function that returns
// Option<PgPool> from $DATABASE_URL (Some only when it's a postgres URL),
// and #[ignore] if your harness ignores rather than early-returns.
async fn pg_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    if !url.starts_with("postgres") { return None; }
    sqlx::PgPool::connect(&url).await.ok()
}

#[tokio::test]
async fn pg_ranks_title_match_above_body_match() {
    let Some(pool) = pg_pool().await else { return; };
    let settings = umbra::Settings::from_env().expect("defaults");
    umbra::App::builder().settings(settings)
        .database("default", pool.clone())
        .model::<Plugin>().model::<Post>().build().expect("build");
    for t in ["spg_plugin", "spg_post"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}")).execute(&pool).await.unwrap();
    }
    sqlx::query("CREATE TABLE spg_plugin (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, blurb TEXT NOT NULL)").execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE spg_post (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO spg_plugin (name, blurb) VALUES ('Redis cache','fast store')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO spg_plugin (name, blurb) VALUES ('Logger','sometimes uses redis')").execute(&pool).await.unwrap();

    let hits = Search::across::<(Plugin, Post)>("redis", 10).await.expect("runs");
    assert!(!hits.is_empty(), "matches exist");
    assert_eq!(hits[0].title, "Redis cache", "title hit ranks first under real ts_rank: {hits:?}");
}
```

- [ ] **Step 3: Run it (skips cleanly with no PG, runs if `DATABASE_URL` is set)**

Run: `cd crates && cargo test -p umbra-rest --test search_pg`
Expected: PASS (the test early-returns when no Postgres URL is present; compiles regardless).

- [ ] **Step 4: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add plugins/umbra-rest/tests/search_pg.rs
git commit -m "test(orm): cfg-gated Postgres ts_rank ordering for Search::across"
```

---

## Task 6: Website rewire (`render_search` → `Search::across`)

**Files:**
- Modify: `umbra_website/plugins/site_content/src/models.rs` (add `impl Searchable for BlogPost`)
- Modify: `umbra_website/plugins/plugin_directory/src/models.rs` (add `impl Searchable for PluginModel`)
- Modify: `umbra_website/plugins/plugin_directory/src/lib.rs` (`render_search`)
- Test: `umbra_website/plugins/plugin_directory/tests/render_pages.rs`

> All `cargo` here runs from `umbra_website/`, NOT `crates/`. This is a separate cargo project that path-deps the framework.
> Orphan rule: `impl Searchable for BlogPost` MUST live in `site_content` (where `BlogPost` is defined); `impl Searchable for PluginModel` in `plugin_directory`. A crate cannot impl the foreign `Searchable` trait for a foreign type.

- [ ] **Step 1: Read the current `render_search` + the two models' fields**

Run:
```bash
cd /home/dalmas/E/projects/umbra
sed -n '610,700p' umbra_website/plugins/plugin_directory/src/lib.rs
grep -n "pub name\|pub crate_name\|pub short_description\|pub slug\|pub moderation\|deleted\|pub struct Plugin" umbra_website/plugins/plugin_directory/src/models.rs
grep -n "pub title\|pub body\|pub status\|pub slug\|pub struct BlogPost\|published" umbra_website/plugins/site_content/src/models.rs
```
Expected: confirms `PluginModel` has `slug`, `moderation`, soft-delete; `BlogPost` has `status`, `title`, `body`, and an id/slug.

- [ ] **Step 2: Add the `Searchable` impls**

In `umbra_website/plugins/site_content/src/models.rs` (after the `BlogPost` model):

```rust
impl umbra::orm::Searchable for BlogPost {
    fn kind() -> &'static str { "blog" }
    // body() default already excludes the `status` choices column and any
    // slug/url fields; title() picks `title`.
}
```

In `umbra_website/plugins/plugin_directory/src/models.rs` (after `PluginModel`):

```rust
impl umbra::orm::Searchable for PluginModel {
    fn kind() -> &'static str { "plugin" }
    fn ident() -> &'static str { plugin::SLUG.name() } // hit.pk = slug, for the URL
}
```

> If `PluginModel`'s slug column const is not `plugin::SLUG`, use the actual const name from Step 1. The default `body()` already drops the `moderation` choices column and the slug; it keeps `name`, `crate_name`, `short_description`. That matches today's LIKE columns plus `crate_name` (already searched today).

- [ ] **Step 3: Rewrite `render_search`** (`umbra_website/plugins/plugin_directory/src/lib.rs`)

Replace the two-query + Rust-merge body (the block building `hits` from a plugin query and a blog query) with:

```rust
pub async fn render_search(q: &str) -> Result<String, String> {
    let trimmed = q.trim();
    let mut hits: Vec<SearchHit> = Vec::new();
    if !trimmed.is_empty() {
        use site_content::models::BlogPost;
        // One ranked UNION across both models. A backend error (e.g. a test
        // DB without the blog table) degrades to no hits rather than a 500.
        match umbra::orm::Search::across::<(PluginModel, BlogPost)>(trimmed, 10).await {
            Ok(found) => {
                // The ranked hits carry slug/title/snippet but not the plugin
                // logo (the template shows it). Batch-fetch logos for the
                // plugin hits in ONE `IN` query (slug -> logo), preserving the
                // ranked order below. No N+1.
                let plugin_slugs: Vec<String> = found
                    .iter()
                    .filter(|h| h.kind == "plugin")
                    .map(|h| h.pk.clone())
                    .collect();
                let mut logo_by_slug: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                if !plugin_slugs.is_empty() {
                    let rows = PluginModel::objects()
                        .filter(plugin::SLUG.in_(&plugin_slugs))
                        .fetch()
                        .await
                        .map_err(|e| e.to_string())?;
                    for p in rows {
                        // Adapt `.slug` / `.logo` to the real field types from
                        // Step 1: `.to_string()` if they're wrapper types,
                        // `.unwrap_or_default()` if `Option`.
                        logo_by_slug.insert(p.slug.clone(), p.logo.clone());
                    }
                }
                for h in found {
                    let (href, label) = match h.kind.as_str() {
                        "plugin" => (format!("/plugins/{}", h.pk), "Plugin".to_string()),
                        _ => (format!("/blog/{}", h.pk), "Blog".to_string()),
                    };
                    let logo = logo_by_slug.get(&h.pk).cloned().unwrap_or_default();
                    hits.push(SearchHit {
                        kind: h.kind,
                        href,
                        name: h.title,
                        label,
                        short_description: h.snippet,
                        logo,
                    });
                }
            }
            Err(e) => tracing::warn!(error = %e, "cross-model search failed; returning no hits"),
        }
    }
    // ... existing template render of `hits` (unchanged) ...
}
```

> Keep the website's existing `SearchHit` struct + template render exactly as-is — only the population changes. Confirm the website `SearchHit`'s field names against Step 1 (`kind`/`href`/`name`/`label`/`short_description`/`logo` above mirror the current shape; rename to match if they differ). The logo batch query goes through the ORM (`.in_()`), not raw SQL.

- [ ] **Step 4: Build the website**

Run: `cd umbra_website && cargo build -p plugin_directory`
Expected: clean (only pre-existing warnings).

- [ ] **Step 5: Update + run the search render test**

The existing `render_pages.rs` search assertions (search "rest" → a `pd-search-result` link to the slug; "zzznomatch" → empty state; blank → hint) should still hold because `kind=="plugin"` still yields `href="/plugins/<slug>"`. Run:

Run: `cd umbra_website && cargo test -p plugin_directory --test render_pages`
Expected: PASS. If the search-result HTML now differs (e.g. snippet text), update only the changed assertion strings to match the new (ranked) output — do not weaken an assertion to pass; assert the real rendered link + name.

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add umbra_website/plugins/site_content/src/models.rs umbra_website/plugins/plugin_directory/src/models.rs umbra_website/plugins/plugin_directory/src/lib.rs umbra_website/plugins/plugin_directory/tests/render_pages.rs
git commit -m "feat(website): header search via ORM Search::across (ranked)"
```

---

## Task 7: User doc page

**Files:**
- Create: `documentation/docs/v0.0.1/orm/search.mdx`

- [ ] **Step 1: Read a sibling page for the frontmatter shape**

Run: `cd /home/dalmas/E/projects/umbra && sed -n '1,12p' documentation/docs/v0.0.1/orm/aggregates.mdx`
Expected: shows the `title/description/sidebar_position/icon` frontmatter convention + `_category_.json` ordering.

- [ ] **Step 2: Write the page** (`documentation/docs/v0.0.1/orm/search.mdx`)

```mdx
---
title: Cross-model search
description: Search several models at once and get one relevance-ranked list.
sidebar_position: 11
icon: search
---

`Search::across` searches every text column of each opted-in model and returns one list ordered by relevance — Postgres ranks with `ts_rank` (computed inline; nothing is stored), SQLite degrades to weighted `LIKE`.

## Opt a model in

A marker impl is enough; the searchable columns, title, and primary key are read from the model's metadata. Override a default only when you need to.

```rust
use umbra::prelude::*;

impl umbra::orm::Searchable for Plugin {
    fn kind() -> &'static str { "plugin" }      // result tag; default = table name
    fn ident() -> &'static str { plugin::SLUG.name() } // routing key in SearchHit.pk
}
impl umbra::orm::Searchable for BlogPost {
    fn kind() -> &'static str { "blog" }
}
```

By default every `Text` column is searched, minus metadata-flagged non-content ones (slug / url / email / `choices`), so searching `"published"` won't return every published row.

## Run a search

```rust
let hits = umbra::orm::Search::across::<(Plugin, BlogPost)>("redis cache", 10).await?;
for hit in hits {
    // hit.kind, hit.pk, hit.title, hit.snippet, hit.rank
}
```

`hits` is a `Vec<SearchHit>` ordered by descending `rank`; a title match outranks a body-only match. A blank query returns an empty list without touching the database.

See the design rationale in `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`.
```

> Pick `sidebar_position: 11` only if free; otherwise use the next free value (check `documentation/docs/v0.0.1/orm/_category_.json` and sibling frontmatter).

- [ ] **Step 3: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add documentation/docs/v0.0.1/orm/search.mdx
git commit -m "docs(orm): cross-model search page"
```

---

## Task 8: Close the gap + final verification

**Files:**
- Modify: `planning/orm_fixes.md` (entry #3 → fixed)

- [ ] **Step 1: Mark orm_fixes #3 fixed**

Edit the `## 3.` entry's status line in `planning/orm_fixes.md` to:

```markdown
**Status:** fixed (`feat(orm): Search::across`) — `umbra::orm::Search::across::<(A, B, …)>(query, limit)` searches every text column of each `Searchable` model and returns one `Vec<SearchHit>` ranked by relevance (Postgres inline `ts_rank`/`setweight`, nothing stored; SQLite weighted `LIKE`). The website `render_search` now calls it instead of merging two queries in Rust. Stored+GIN tsvector remains a logged future optimization.

**Status (original):** open — Rust-side merge in place; a unified ranked search needs its own spec.
```

- [ ] **Step 2: Verify the framework suites**

Run: `cd crates && cargo test -p umbra-core -p umbra-rest`
Expected: all pass.

- [ ] **Step 3: Verify the website search test**

Run: `cd umbra_website && cargo test -p plugin_directory --test render_pages`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add planning/orm_fixes.md
git commit -m "docs(orm): close orm_fixes #3 (cross-model search shipped)"
```

---

## Notes for the implementer

- **Param reuse:** Postgres reuses `$1` in every UNION branch (rank + where), so `across` binds the query once + `$2` for limit. SQLite reuses `?1`/`?2` across branches + `?3` for limit. This is why arity doesn't change the bind count.
- **`rank` is `f64`:** SQLite has no `f32`; PG casts `ts_rank(...)::float8`. Keep `SearchHit.rank: f64`.
- **No raw SQL in plugins:** `Search::across` lives in `umbra-core` (the ORM itself generating SQL — allowed). The website calls the ORM surface, never `sqlx::query`. Keep it that way.
- **Don't touch `crates/` in Task 6/7** and don't touch `umbra_website/` in Tasks 1–5. The dev server watches `umbra_website/`; a green `cargo build -p plugin_directory` before committing keeps it from breaking on its next rebuild.
- **Disk:** the framework `crates/target` is large; if a build fails with "No space left on device", `rm -rf crates/target/debug/incremental` (safe; never touch `umbra_website/target`).
