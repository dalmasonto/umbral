# Cross-model search (`Search::across`) — design

**Status:** approved, ready for implementation plan.
**Closes:** `planning/orm_fixes.md` #3 (no cross-model UNION / ranking).

## Goal

Give the ORM a first-class way to search several models at once and return one relevance-ranked list — so the website's header search (plugins + blog posts) stops hand-rolling two queries and a Rust-side merge with no ranking. The surface is `Search::across::<(Plugin, BlogPost)>(query, limit).await?` returning a flat `Vec<SearchHit>` ordered by relevance.

## Background — what already exists

- `SqlType::FullText` → a Postgres `tsvector` column type with an auto-GIN index (gap #33), `FullTextCol<T>` with `matches()` / `matches_websearch()` (boolean `@@` predicates), and a `TsVector` value type. None of this exposes a **rank**, and all of it assumes a **stored** tsvector column.
- `QuerySet::union()` / `intersect()` / `except()` (gap #28) combine two querysets of the **same** model `T`. They can't express a cross-model union (different tables, different columns).
- The consumer today (`umbra_website/plugins/plugin_directory/src/lib.rs::render_search`) runs `PluginModel::objects().filter(name/crate/desc LIKE q).limit(6)` and `BlogPost::objects().filter(title/body LIKE q).limit(4)`, maps each into a `SearchHit`, and concatenates plugins-then-posts. No relevance ranking; arbitrary per-model sub-limits.

The new work is therefore three things the existing infra does not cover: **ranking**, a **cross-model normalized UNION**, and doing it with **nothing persisted**.

## Decisions locked during brainstorming

1. **Inline `to_tsvector` / `ts_rank` — nothing stored.** Postgres builds the lexeme vector transiently per query; no tsvector columns, no migrations, no triggers, zero added bytes or write cost. Ranking quality is identical to a stored column; only index-backed speed at large scale is forgone, which does not matter at the site's row counts. Stored + GIN remains a logged future optimization, not part of this spec.
2. **Search every text column automatically.** A model opts in with a marker impl; the engine enumerates its text columns from `Model` metadata. No per-column listing.
3. **Return a fixed normalized `Vec<SearchHit>`.** The ORM stays UI-agnostic; the caller maps `kind` + `pk` to its own links/icons.

## Non-goals (scope guard)

- No stored/indexed tsvector columns and no migration-engine `GENERATED … STORED` support (separate future gap).
- No change to the existing same-model `union()` / `intersect()` / `except()` — this is a distinct cross-model path.
- No highlighted snippets (`ts_headline`) in v1 — a plain truncation (see §Snippet). Upgradeable later.
- No faceting, pagination cursors, or per-field boosting beyond the title-vs-body weight described below.

## Design

### The `Searchable` trait

```rust
/// A model that can take part in `Search::across`. Opt in with a marker
/// impl; all behavior derives from the model's `Model` metadata.
pub trait Searchable: Model {
    /// Result tag, e.g. "plugin" / "blog". Default: the table name.
    fn kind() -> &'static str { Self::TABLE }

    /// Column shown as the result title. Default: the text column named
    /// (case-insensitively) "title" or "name", else the first text column.
    fn title() -> &'static str { default_title::<Self>() }

    /// Text columns that form the searchable body (concatenated). Default:
    /// every text column EXCEPT ones flagged non-content (see rules below).
    fn body() -> Vec<&'static str> { default_body::<Self>() }

    /// Column whose value becomes `SearchHit.pk` — the identifier the caller
    /// routes on. Default: the model's primary-key column. Override to a
    /// natural key when the URL uses one (e.g. `Plugin` → `slug`), so the
    /// caller builds a link with no extra lookup.
    fn ident() -> &'static str { default_pk_column::<Self>() }
}
```

Typical use is a bare `impl Searchable for Plugin {}`. Overrides are per-method when a default is wrong (e.g. `BlogPost` wanting `kind() -> "blog"` instead of `"blog_post"`).

**Text-column selection rules** (`default_body`), all read from the model's `FieldSpec` list (the same metadata migrations and the admin use):

- Include a column when its `SqlType` is a text type (`Text` / `Varchar` / string-PK `String`).
- Exclude a column whose `FieldSpec` string-subtype is `slug`, `url`, or `email`, or that carries `choices` (enum-like) — these are non-content and pollute results (searching "approved" must not return every approved row). This keeps "all string fields" honest without per-model config.
- The title column is always part of the body too (so a title hit also scores on the body concat), but it is additionally weighted (see ranking).

`default_title::<T>()` and `default_body::<T>()` are free helpers over `T::field_specs()`; they are pure and unit-testable in isolation.

### The result type

```rust
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SearchHit {
    pub kind: String,     // Searchable::kind()
    pub pk: String,       // Searchable::ident() value, CAST to text — the routing key (PK by default, e.g. slug for Plugin)
    pub title: String,
    pub snippet: String,
    pub rank: f32,
}
```

Fixed column aliases (`kind`, `pk`, `title`, `snippet`, `rank`) so every model's branch unions cleanly and `FromRow` decodes identically on both backends.

### The API and the tuple bound

```rust
pub struct Search;

impl Search {
    pub async fn across<S: SearchSources>(query: &str, limit: u64)
        -> Result<Vec<SearchHit>, sqlx::Error>;
}
```

`SearchSources` is implemented for tuples `(A,)`, `(A, B)`, …, up to arity 6 via a declarative macro; each element is `Searchable`. Each implementation asks every member model for its normalized `SelectStatement` (built for the active backend), `UNION ALL`s them, applies `ORDER BY rank DESC LIMIT $limit`, and executes through the ambient pool with the existing per-backend dispatch (`pool_dispatched()`), decoding rows into `SearchHit`.

A blank/whitespace-only `query` short-circuits to `Ok(vec![])` without touching the database.

### SQL generation (per backend)

Each model contributes one branch with the fixed projection. `<body_concat>` is `coalesce(c1,'') || ' ' || coalesce(c2,'') || …` over `body()` columns.

**Postgres** (inline, nothing stored):

```sql
SELECT 'plugin' AS kind,
       slug      AS pk,        -- ident(); CAST(<ident> AS text) in general
       name      AS title,
       left(coalesce(name,'')||' '||coalesce(short_description,''), 200) AS snippet,
       ts_rank(
         setweight(to_tsvector('english', coalesce(name,'')), 'A') ||
         to_tsvector('english', coalesce(short_description,'')),
         websearch_to_tsquery('english', $1)
       ) AS rank
FROM   plugin
WHERE  to_tsvector('english', coalesce(name,'')||' '||coalesce(short_description,''))
       @@ websearch_to_tsquery('english', $1)
```

`setweight(..., 'A')` on the title gives title matches a higher `ts_rank`, satisfying "title outranks body-only". The same `$1` is shared by every UNION branch.

**SQLite** (tests; no tsvector):

```sql
SELECT 'plugin' AS kind,
       slug AS pk,                -- ident(); CAST(<ident> AS TEXT) in general
       name AS title,
       substr(coalesce(name,'')||' '||coalesce(short_description,''), 1, 200) AS snippet,
       ( (CASE WHEN name LIKE ?1 THEN 2.0 ELSE 0 END)
       + (CASE WHEN short_description LIKE ?1 THEN 1.0 ELSE 0 END)
       + (CASE WHEN name LIKE ?2 THEN 1.0 ELSE 0 END) ) AS rank   -- ?2 = prefix 'q%'
FROM   plugin
WHERE  name LIKE ?1 OR short_description LIKE ?1
```

`?1` = `%q%` (substring), `?2` = `q%` (prefix bonus). Coarser than `ts_rank`, but a real title-weighted ordering. LIKE wildcards in the user query are escaped (`%`, `_`, `\`).

Branches are built as sea-query `SelectStatement`s (custom expressions via `Expr::cust_with_values` for the rank/where), combined with `UnionType::All`, matching how `QuerySet::combine` already unions statements — so the builder, parameter binding, and backend dispatch are reused, not reinvented.

### Snippet

`left(<body_concat>, 200)` (Postgres) / `substr(..., 1, 200)` (SQLite). Cheap, no second tsquery pass. The website already shows its own `short_description` in the result row, so the snippet is a fallback; highlighted `ts_headline` is a future upgrade.

### Website rewire

`render_search` drops both hand-written querysets and the Rust merge, and calls:

```rust
let hits = umbra::orm::Search::across::<(PluginModel, BlogPost)>(trimmed, 10).await;
```

mapping each `SearchHit` to the existing template `SearchHit` shape (deriving `href` from `kind` + `pk` and the logo). The "blog table missing in a test DB" resilience stays: wrap the call so a backend error degrades to an empty list + `tracing::warn!` rather than a 500.

Each model gets a one-line opt-in, with `Plugin` overriding `ident()` so the hit carries the URL key directly:

```rust
impl Searchable for PluginModel {
    fn kind() -> &'static str { "plugin" }
    fn ident() -> &'static str { plugin::SLUG.name() }   // hit.pk = slug
}
impl Searchable for BlogPost {
    fn kind() -> &'static str { "blog" }                 // hit.pk = id (PK default)
}
```

The website then builds `/plugins/{pk}` for `kind == "plugin"` and `/blog/{pk}` (or its id-based route) for `kind == "blog"` with no extra lookup — the ORM stays free of URL shapes.

## Surfacing

`Searchable`, `Search`, `SearchHit`, and `SearchSources` live in `umbra-core` (`src/orm/search.rs`), re-exported from the `umbra` facade as `umbra::orm::{Search, Searchable, SearchHit}`. Not added to the prelude (power-user surface; keeps the prelude unambiguous). A short user doc page lands under `documentation/docs/v0.0.1/orm/` per the "ship a feature, ship its doc page" rule.

## Error handling

- Blank query → `Ok(vec![])`, no query issued.
- A malformed `websearch_to_tsquery` input cannot error — `websearch_to_tsquery` is total (unlike `to_tsquery`), which is why it's chosen over `matches()`'s strict `to_tsquery`.
- DB/transport errors propagate as `sqlx::Error`; the website wraps them into its resilient empty-list-with-warn path.

## Testing

Behavioral, against a real in-memory SQLite DB seeding real rows through the public path, then reading the ranked list back:

1. A query matching both a plugin and a post returns **both**, as `SearchHit`s with the right `kind`/`pk`.
2. A row whose **title** matches outranks a row matching only in the **body** (assert ordering, not just membership).
3. `kind` + `pk` round-trip to the originating rows.
4. No-match query → empty `Vec`.
5. Blank/whitespace query → empty `Vec`, and (assert) no query ran.
6. The non-content exclusion: a query equal to a `choices`/`slug` value does **not** match via that column.

Pure-unit tests for `default_title` / `default_body` over hand-built `FieldSpec` lists (title detection precedence; exclusion rules).

A `#[ignore]`/`cfg`-gated Postgres test mirrors (1) and (2) against real `ts_rank` + `setweight`, run in the same CI lane as the existing `plugins/umbra-rest/tests/rest_fts_pg.rs`.

## Future work (logged, not in scope)

- Stored + GIN tsvector columns with migration-engine `GENERATED … STORED` support and a population story, switched in transparently when present (the speed optimization).
- `ts_headline` highlighted snippets.
- Per-field boost configuration beyond title/body.
