# Search and data governance: two plugins over the ORM's existing primitives

Status: draft (proposes gaps5 #33 `umbral-search` and gaps5 #34 data-governance metadata; final call is the maintainer's)
Date: 2026-08-08
Drafts: planning/gaps5.md #33 (tf #246), planning/gaps5.md #34 (tf #247)
Relates: planning/gaps5.md #86 (compliance workflows), docs/decisions/2026-08-08-product-north-star.md (Stage 2 self-hosted platform posture)

## Framing

Both items sit inside the north star's Stage 2 (self-hosted platform), and both obey the one rule that governs everything here: they are **plugins over primitives the ORM already owns**, not new engines. #33 wraps the real full-text search that exists today; #34 is a metadata layer over the `#[umbral(...)]` field-attribute system that exists today. Neither reimplements a primitive, and neither belongs in `umbral-core`: search is an optional dependency (most apps do not need faceting or an external engine), and governance is a plugin because classification is app policy, not framework mechanism. The core keeps only the seams both build on.

This doc is deliberately scoped to the model and field layer for #34. The DSAR *workflow engine* (the orchestration of a subject-access or erasure request through approval, audit, and delivery) is gaps5 #86 and is referenced here, not designed here. What #34 defines is the metadata that #86 will read.

---

## Part 1: `umbral-search` (gaps5 #33)

### What already exists (the real FTS API to build on)

The ORM already ships Postgres full-text search. This plugin wraps it; it does not replace it. The concrete surface, all in `crates/umbral-core/src/orm/`:

1. **`SqlType::FullText`** (`model.rs`) - a column kind that the migration engine emits as a bare Postgres `tsvector`. It is Postgres-only; the M4 boot system check rejects a model that uses it on SQLite. The migration engine emits the bare `tsvector` declaration and leaves population to the app (a trigger or a `GENERATED ALWAYS AS (to_tsvector(...)) STORED` clause). A GIN index is the intended companion.
2. **`FullTextCol<T>` / `NullableFullTextCol<T>`** (`column.rs`) - typed column handles over a `tsvector` column with two predicate builders:
 - `.matches(query)` emits `"col" @@ to_tsquery($1)` (strict operator syntax: `&` AND, `|` OR, `!` NOT, `:*` prefix).
 - `.matches_websearch(query)` emits `"col" @@ websearch_to_tsquery($1)` (forgiving, user-typed: space-separated AND, `OR`, `-term`, `"quoted phrase"`).
 - Plus `.asc()` / `.desc()` for ordering. These return boolean `Predicate<T>` values; **none of them expose a rank.**
3. **`TsVector`** (`tsvector.rs`) - the value type that decodes a `tsvector` column off the wire (via the `tsvector::text` cast form).
4. **`Search::across::<(A, B, ...)>(query, limit)`** (`search.rs`) - the cross-model relevance search. A model opts in with a marker `impl Searchable for T {}`; everything else derives from `T::FIELDS`. The `Searchable` trait's hooks are `kind()` (result tag, default table name), `title()` (default the text column named `title`/`name`, else first text column), `body()` (default every content-text column, excluding slug/url/email/choices), `ident()` (default the PK column, overridable to a natural key like a slug), and `filter_sql()` (a **compile-time author constant** ANDed into the WHERE for row visibility, e.g. `"status = 'published'"` - spliced verbatim, never parameterized, so never request-derived). It returns `Vec<SearchHit>` where `SearchHit` is `{ kind, pk, title, snippet, rank }`.
 - The Postgres branch builds the vector inline per query: `setweight(to_tsvector('english', coalesce(title,'')), 'A')` for the title, unweighted `to_tsvector('english', ...)` for the rest of the body, ranks with `ts_rank(vec, websearch_to_tsquery('english', $1))::float8`, snippets with `left(body_concat, 200)`, and soft-delete is auto-excluded (`deleted_at IS NULL`) for soft-delete models.
 - The SQLite branch is a `LIKE`-based fallback with a synthetic rank (title match weighted above body, prefix bonus). This is what tests run against; Postgres is the production target.
 - The design is **inline `to_tsvector`, nothing stored** (per `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`): no stored tsvector columns, no triggers, zero write cost. Stored + GIN is a logged future optimization.
5. **`DynQuerySet::search(fields, term)`** (`dynamic.rs`) - the late-bound `LIKE`-based search the admin's list filter uses. Not FTS; string containment over selected columns.

The honest gaps in that surface, and therefore the plugin's reason to exist:

- The text-search config is **hard-coded `'english'`** everywhere. No per-model or per-request language / stemming choice.
- `Search::across` exposes **no faceting, no highlighting** (`left(...)` truncation, not `ts_headline`), and **no per-field boosting** beyond the single title-vs-body A-weight.
- There is **no fuzzy / typo tolerance** (no `pg_trgm`).
- There is **no path to an external engine** (Meilisearch / Typesense / OpenSearch) for apps that outgrow inline Postgres FTS.

`umbral-search` closes exactly those, and nothing else. It never re-emits `to_tsvector` from scratch; it parameterizes and extends the calls `search.rs` already makes.

### The plugin surface

`umbral-search` is a plugin crate under `plugins/`, depending only on the `umbral` facade. It contributes no models of its own in the base (Postgres) mode - that is the whole point of the "nothing stored" design. It adds:

**A ranking + query DSL over the real FTS.** A `SearchQuery` builder that compiles down to the exact SQL shapes `search.rs` uses, but with the knobs the raw functions hide:

```rust
let hits = SearchQuery::new("wireless noise cancelling")
 .over::<(Product, Article)>() // reuse the Searchable trait as-is
 .config(TextConfig::English) // was hard-coded; now a parameter
 .boost::<Product>("title", 3.0) // per-field weight -> setweight A/B/C/D
 .rank(RankNormalization::LogLength) // ts_rank normalization flags
 .highlight(Highlight::headline()) // ts_headline instead of left(,200)
 .facet_by(&["brand", "category"]) // GROUP BY counts, second query
 .fuzzy(Fuzzy::Trigram { threshold: 0.3 }) // pg_trgm similarity fallback
 .limit(20)
 .fetch()
 .await?;
```

Each knob is a thin, honest mapping onto a Postgres feature, not a new engine:

- **`config(TextConfig)`** replaces the literal `'english'` in the `to_tsvector(...)` / `websearch_to_tsquery(...)` calls with a `regconfig` (e.g. `'simple'`, `'spanish'`). This is the "multilingual stemming config" line item: Postgres text-search configs *are* the stemmers. Per-model default via a `Searchable::config()` override (added alongside the existing hooks); per-query override via the builder. Nothing else changes in the emitted SQL.
- **`boost(field, weight)`** generalizes the single `setweight(..., 'A')` the title already gets to the full A/B/C/D ladder, mapping a caller weight bucket to a `setweight` label and feeding a `ts_rank` weights array `{D, C, B, A}`. Default behavior (title = A, body = unweighted) is preserved when no boost is set, so `Search::across`'s current output is unchanged.
- **`rank(RankNormalization)`** exposes `ts_rank`'s normalization integer (document-length division, unique-word division, etc.) that the current `ts_rank(vec, query)::float8` call leaves at the default `0`.
- **`highlight(Highlight)`** swaps the `left(body_concat, 200)` snippet for `ts_headline(config, body, query, options)`, returning marked-up fragments. The `SearchHit.snippet` field already exists; highlighting only changes how it is produced, so it is backward compatible.
- **`facet_by(&[field])`** issues a second, aggregate query (`SELECT field, COUNT(*) ... WHERE <same match> GROUP BY field`) reusing the identical match predicate, and returns counts alongside the hit list. Faceting is a `GROUP BY`, not a new index.
- **`fuzzy(Fuzzy::Trigram)`** is the one part that touches DDL: trigram search needs the `pg_trgm` extension and a GIN/GiST `gin_trgm_ops` index to be fast. The plugin's migration (it owns exactly one) enables `pg_trgm` and, for columns a model opts into fuzzy search on, creates the trigram index. The query then adds a `similarity(col, $1) > threshold` (or `col % $1`) branch OR-ed with the FTS match, so a misspelling that the lexeme match misses still ranks. This is the sanctioned "backend-specific feature the ORM does not model" exception from CLAUDE.md: gated on `DbPool::Postgres`, with the SQLite branch keeping today's `LIKE` fallback (already the case in `search.rs`).

**All of this stays inside the ORM/plugin boundary.** The plugin builds SQL through the same `sqlx::query_as::<_, SearchHit>` terminal `Search::across` already uses; it does not hand-roll row-level reads through raw `sqlx::query` on app tables. The DDL it does emit (the `pg_trgm` extension + trigram indexes) is the migration-engine exception, owned by the plugin's own migration.

### Optional external-engine sync (Meilisearch / Typesense / OpenSearch)

At a scale where inline Postgres FTS stops being enough (millions of rows, sub-50ms typo-tolerant search, instant-search-as-you-type), the app can mirror searchable models into a dedicated engine. This is **opt-in, off by default, and driven entirely through the task queue** - no synchronous coupling of a write path to an external service.

The design:

- **A `SearchIndex` registration** names which `Searchable` models mirror to which engine and which fields map to which index attributes. This is the same "declare the model, get the plumbing" shape as the rest of umbral: the plugin reads `T::FIELDS` for the schema and the app only names the engine endpoint (a setting) and the fields to index.
- **Sync rides the existing signals + task queue, never the request.** The plugin subscribes to the ORM's post-save / post-delete signals for registered models and enqueues an `umbral-tasks` job (`reindex(table, pk)` / `deindex(table, pk)`). The worker performs the HTTP upsert/delete against the engine. This means: the app's write latency is unchanged, a down search engine never fails a write (the job retries via the queue's existing retry/backoff), and a full reindex is a bulk-enqueue management command (`umbral search reindex <model>`). Using the DB-backed queue as the durable outbox is exactly its job; the plugin adds no second delivery mechanism.
- **A unified read surface.** `SearchQuery::fetch()` dispatches: if the model is registered to an external engine, it queries the engine and maps results back into the same `Vec<SearchHit>`; otherwise it runs the inline Postgres path. The caller's code is identical either way - swapping the backend is a settings change, not a rewrite. That parity is the deliverable: an app starts on Postgres FTS and moves to Meilisearch without touching handler code.
- **Engine adapters are behind a trait**, one per engine (`MeilisearchBackend`, `TypesenseBackend`, `OpenSearchBackend`), each a thin `reqwest` client mapping the umbral `SearchQuery` to that engine's query DSL and its documents back to `SearchHit`. Each is feature-gated so an app pulls only the client it uses.

### What is deferred

- Stored/indexed tsvector columns with `GENERATED ... STORED` migration support (already a logged FTS future optimization; the plugin works without it and gains from it transparently when it lands).
- Cross-engine query federation (query Postgres AND Meilisearch and merge). v1 routes each model to exactly one backend.
- Learned ranking / synonyms / query rewriting beyond what the chosen engine provides natively.

---

## Part 2: Data-governance metadata (gaps5 #34)

### Scope boundary with gaps5 #86

This item is the **classification and metadata layer**: how a model or field declares that it holds PII, what retention class it falls under, where it may reside, and whether it is under legal hold - plus the registry that makes that machine-readable, and the read hooks a DSAR export or delete uses to find and act on the data. The **workflow engine** that runs a data-subject-access-request end to end (intake, identity verification, approval, staged deletion, audit trail, delivery) is gaps5 #86. The relationship is deliberate: #34 is the schema #86 queries. Ship #34 first, because a workflow with nothing to read is inert, and classification metadata is independently useful (data maps, audit evidence, `Masked<T>` correlation) even before the workflow exists.

### What already exists (the substrate)

umbral already has the two things this builds on:

1. **The `#[umbral(...)]` field-attribute system.** Attributes on a struct field are parsed by `parse_umbral_field_attr` (`crates/umbral-macros/src/lib.rs`) and lowered into `FieldSpec` consts in `Model::FIELDS` (`crates/umbral-core/src/orm/model.rs`), which the migration engine, admin, REST, and OpenAPI all read. The existing catalogue includes `noform`, `private`, `secret`, `privileged`, `unique`, `index`, `help`, `on_delete`, `auto_now_add`, `text_format`, and roughly twenty more. Adding a governance attribute is adding a field to `FieldSpec` and a match arm to the parser - the same well-trodden path every existing attribute took. Crucially, `help` already proves the pattern of a field attribute that flows to *runtime* metadata (it becomes a Postgres `COMMENT ON COLUMN` and an OpenAPI description), so classification riding into `ModelMeta` is not a new mechanism.
2. **The confidentiality tiers already in core** (`crates/umbral-core/src/orm/secrets.rs` + `FieldSpec`): `#[umbral(private)]` (default-deny read, explicit `allow_private` unlock), `#[umbral(secret)]` (never serialized, no unlock, auto-applied to every `Masked<T>`), `#[umbral(privileged)]` (default-deny write / mass-assignment guard), and `Masked<T>` (encrypt-at-rest with crypto-shredding via private-key destruction, `crates/umbral-core/src/orm/masked.rs`). These are *access* controls. Governance metadata is the *classification* layer that sits beside them: `private`/`secret` say "who may see it"; `pii`/`retention`/`residency` say "what it is and how long we may keep it." They compose - a `Masked<String>` phone field is both `secret` (access) and `pii` + a retention class (governance).

### The metadata attributes

New field-level `#[umbral(...)]` attributes, each lowering to a new `FieldSpec` field and propagating into `ModelMeta`/`Column` so plugins read it at runtime exactly like `help`:

```rust
#[derive(Model)]
struct Customer {
 id: i64,

 #[umbral(pii, retention = "customer_data", residency = "eu")]
 email: Email,

 #[umbral(pii = "sensitive", retention = "customer_data")] // special-category data
 phone: Masked<String>,

 #[umbral(retention = "audit_log", legal_hold_exempt)]
 created_at: DateTime<Utc>,
}
```

- **`pii`** (bare) / **`pii = "sensitive"`** - marks the column as personal data, with an optional sensitivity tier (`"basic"` default vs `"sensitive"` for special-category / GDPR Art. 9 data). Stored as `FieldSpec.pii: Option<PiiClass>`. This is the flag a DSAR export enumerates and a data map renders.
- **`retention = "<class>"`** - names a **retention class** (see below) the column belongs to. Free-form string keyed into the retention registry; the migration engine does not interpret it, so an unknown class is a config error surfaced by the plugin's boot check, not a schema break. Stored as `FieldSpec.retention: &'static str`.
- **`residency = "<region>"`** - a data-residency tag (`"eu"`, `"us"`, `"any"`). At v1 this is metadata for auditing and for a future residency-routing decision (gaps5 #85, deferred); the plugin's boot check can warn when a `residency = "eu"` column lives on a pool the app has tagged non-EU, but it does not itself route storage.
- **`legal_hold_exempt`** - marks a column (typically an immutable audit timestamp) that a legal hold and an erasure request both leave untouched, so "delete this subject" never corrupts the audit trail.

A **model-level** attribute complements the field ones for tables that are wholly about a data subject:

```rust
#[umbral(data_subject = "user_id")] // this table's rows belong to the subject named by user_id
#[derive(Model)]
struct OrderHistory { ... }
```

`data_subject = "<fk_column>"` names the column that ties every row to a data subject (usually the FK to the user). This is what makes a DSAR *find* the rows: "export/delete everything for subject X" walks every model carrying a `data_subject` pointer to X. Without it, a DSAR can only reach the user row itself, not the graph hanging off it.

### The classification registry

The attributes above are declarations scattered across models; the registry is the single machine-readable inventory assembled from them at build time. `GovernancePlugin` walks every registered model's `ModelMeta`, collects every `pii` / `retention` / `residency` / `data_subject` marker, and exposes:

- **`ClassificationRegistry::data_map()`** - the full inventory: for every model, which columns are PII, at what sensitivity, in which retention class, in which region. This is the "record of processing activities" (GDPR Art. 30) an org needs, generated from the code instead of maintained in a spreadsheet that drifts. It also feeds a `umbral governance datamap` CLI command (JSON / Markdown output) and an admin page.
- **`ClassificationRegistry::pii_columns(table)`** / **`subject_links()`** - the runtime lookups the DSAR read hooks (below) use.
- **A boot check** that fails fast on incoherent metadata: a `retention` naming an unregistered class, a `residency` on an unknown region, a `data_subject` FK column that does not exist. Same posture as the rest of umbral: a mismatch is caught at boot, not in prod.

### Retention classes

A **retention class** is a named policy registered once at the app level and referenced by the `retention = "..."` attribute, so the policy lives in one place and columns point at it:

```rust
GovernancePlugin::default()
 .retention_class("customer_data", Retention::days(365 * 3).on_delete(RetentionAction::Anonymize))
 .retention_class("audit_log", Retention::years(7).on_delete(RetentionAction::Retain))
 .retention_class("session_data", Retention::days(30).on_delete(RetentionAction::HardDelete))
```

A class carries a duration and a default expiry action (`HardDelete`, `Anonymize` - overwrite with a tombstone / crypto-shred the `Masked` value, `Retain` - keep, typically for legally-mandated records). The *enforcement* (a periodic sweep that finds rows past their class's horizon and applies the action) is a scheduled `umbral-tasks` beat job the plugin ships - the same queue-and-beat substrate #33's external sync uses, not a new scheduler. Anonymize on a `Masked<T>` column is where classification and encryption meet: destroying the per-subject key crypto-shreds the value, which the `masked.rs` design already calls out as the fast bulk-erasure path for "right to be forgotten."

### Legal hold

A **legal hold** is a runtime override that suspends retention-driven deletion for a set of rows (a subject, a case, a matter) regardless of their retention class, so a litigation hold does not get silently swept away by the retention job:

- A small core table (`governance_legal_hold`, owned by the plugin's migration) records active holds keyed by subject id and/or `(table, pk)` scope.
- The retention sweep and any DSAR *delete* both consult the hold table first and **skip held rows**, logging that they were skipped rather than failing silently. `legal_hold_exempt` columns (audit timestamps) are the narrow exception that a hold does not need to protect because erasure never touches them anyway.
- Placing / lifting a hold is an admin action and an API call; the *workflow* around who may do so and its approval chain is gaps5 #86.

### How DSAR export / delete reads this metadata

This is the seam #34 delivers for #86 to build the workflow on. Two read-oriented operations, both pure functions of the registry plus the ORM, with **no workflow orchestration** (that is #86):

- **DSAR export (portability / access request).** Given a subject id, the exporter walks `subject_links()` to find every model whose rows belong to the subject (`data_subject` FK match) plus the subject's own row, selects them through the ORM, and emits a structured bundle (JSON) of every PII column - reading `Masked<T>` values via the loudly-named reveal path (the same deliberate unlock `dumpdata` uses for backups), and honoring `private`/`secret` tiers by *including* them in the subject's *own* export (the subject is entitled to their data) while never leaking one subject's data into another's. The export is a read; it mutates nothing.
- **DSAR delete / anonymize (erasure request).** Given a subject id, the eraser walks the same `subject_links()` graph, checks the legal-hold table (skipping held rows), and applies each column's retention-class action: `HardDelete` removes the row through the ORM's `delete()`, `Anonymize` overwrites PII columns with tombstones or crypto-shreds `Masked` values, `Retain` (legally-mandated records) is left in place with the non-retained PII around it anonymized. FK ordering reuses the migration engine's existing dependency graph so children go before parents. Every action is logged for the audit trail #86 assembles.

Both operations go **through the ORM**, never raw SQL - they are exactly the row-level reads/writes CLAUDE.md says plugins must route through the ORM. If a needed operation is missing (e.g. a bulk anonymize-in-place), that is an ORM gap to file, not a raw-SQL workaround.

### What is deferred to gaps5 #86 (and elsewhere)

- The **DSAR workflow engine**: request intake, identity verification, human approval steps, SLA timers, staged/reversible deletion, delivery, and the audit-trail assembly. #34 gives #86 the metadata and the export/delete read hooks; #86 orchestrates them.
- **Residency *routing*** (actually storing `residency = "eu"` data on an EU pool): metadata only at v1; routing is gaps5 #85 (multi-region), deferred by the north star.
- **Consent tracking / lawful-basis records**: a separate governance concern; not part of the classification layer.

---

## Why these are two items in one doc

They share one spine: both are Stage-2 platform capabilities expressed as plugins over primitives core already owns (FTS for #33, the field-attribute + confidentiality-tier system for #34), both lean on the `umbral-tasks` queue-and-beat substrate for their asynchronous work (external-engine sync; retention sweeps), and both refuse to reimplement anything the ORM already does. Keeping them in one decision doc records that shared shape: umbral grows platform breadth by wrapping its own primitives in plugins, never by bolting on a parallel engine.
