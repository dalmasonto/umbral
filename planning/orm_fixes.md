# ORM fixes & gaps found during the website build-out

Entries logged when a needed query couldn't be expressed (or a documented ORM
capability didn't behave). Numbers are stable identifiers. Each entry: the
symptom, where it bit, the workaround in place, and the proper fix.

---

## 1. `prefetch_related("<field>")` returns empty buckets for a SECOND reverse-FK field on one model

**Status:** fixed (not a framework bug as diagnosed) — regression tests added; website workaround can be removed.

**Resolution:** the documented root cause was wrong, and so was the headline symptom. Investigated via TDD: a parent with TWO `ReverseSet<C>` fields (two different child models, same `reverse_fk` column name `article`), mirroring the website's `Plugin` (`soft_delete`, explicit `#[umbra(primary_key)] id`, child carrying a `DateTime<Utc>` column) — prefetching only the SECOND set, and both sets together, BOTH populate correctly. The macro emits one `REVERSE_FK_RELATIONS` entry + one `set_m2m_parent_ids` arm + one `set_reverse_fk_resolved_json` arm PER `ReverseSet` field (Vec iteration, not first-only); the runtime dispatch loop hydrates each reverse-FK field independently with its own per-parent bucket. Verified the macro already emitted both arms at the workaround commit (`f1eb714`), so the macro was never the cause. The PK-agnostic hydration refactor (`b624594`, landed ~4h AFTER the workaround) made the loader bucket children via `pk_as_json()`/`pk_key` rather than i64-only, but even the pre-refactor i64 path dispatched per-field correctly. No code path reproduces "second set empty"; the website observation was most likely a data/seed-state artifact misattributed to the framework. Regression tests live in `crates/umbra-core/tests/reverse_fk_prefetch.rs` (`macro_emits_a_reverse_fk_spec_for_every_set`, `prefetch_both_reverse_sets_populates_each_slot`, `prefetch_only_second_reverse_set_populates_it`). The `/prebuilt` `IN`-batch workaround is no longer necessary — `prefetch_related("feature_set")` works.

**Status (original):** open — workaround in place, proper fix pending.

**Where:** `umbra_website/plugins/plugin_directory` — `Plugin` declares two
`ReverseSet` fields: `comment_set: ReverseSet<PluginComment>` (pre-existing) and
`feature_set: ReverseSet<PluginFeature>` (added for `/prebuilt`). Rendering
`/prebuilt` via `PluginModel::objects().filter(...).prefetch_related("feature_set").fetch()`
then reading `p.feature_set.resolved()` returned **no children** — even though the
rows exist and `plugin.reverse::<PluginFeature>()` (the per-parent reverse query)
finds them. No error was raised; `resolved()` just came back empty, so the feature
grid rendered blank.

**Symptom precisely:** the prefetch did NOT error (so `reverse_fk_spec("feature_set")`
resolved), but the children bucket never populated the slot. Suspected cause: the
macro-emitted `set_reverse_fk_resolved_json` (or the parent-id/fk-column wiring in
`set_m2m_parent_ids`) handles only the first `ReverseSet` field on a model, so a
second one silently no-ops. `comment_set` prefetch presumably still works; a model
with exactly one reverse set was the only tested shape (see
`crates/umbra-core/tests/prefetch_related.rs`).

**Workaround (shipped):** `/prebuilt` batch-loads features with an explicit `IN`
query and groups in memory — equally optimized (1 parents + 1 children query, no
N+1):

```rust
let ids: Vec<i64> = plugins.iter().map(|p| p.id).collect();
let rows = PluginFeature::objects()
    .filter(plugin_feature::PLUGIN.in_(&ids))
    .filter(plugin_feature::VISIBLE.eq(true))
    .order_by(plugin_feature::DISPLAY_ORDER.asc())
    .fetch().await?;
// group by f.plugin.id() into a HashMap<i64, Vec<_>>
```

**Proper fix:** in `umbra-macros`, emit the reverse-FK hydration arms
(`set_m2m_parent_ids` parent-id/fk-column seeding AND `set_reverse_fk_resolved_json`)
for EVERY `ReverseSet` field on the model, not just the first. Add a regression test
in `crates/umbra-core/tests/prefetch_related.rs` for a model with two reverse-FK
collections prefetched together (`prefetch_related("a_set", "b_set")`) asserting both
populate. Until then, the `IN`-batch pattern above is the recommended shape for
multi-reverse-set parents.

---

## 2. `DynQuerySet::insert_json` has no transaction variant (blocks true-atomic nested writes)

**Status:** fixed (`feat(orm): transactional dynamic insert (insert_json_in_tx) for atomic nested writes`) — added `DynQuerySet::insert_json_in_tx(&self, body, &mut umbra::db::Transaction)`; refactored `insert_json`'s body-normalise + column-build tail into shared helpers (`normalise_insert_body` / `build_insert_plan`) so the pool and tx paths can't drift; the tx path runs the INSERT, PK re-fetch, M2M junction writes (`set_junction_dynamic_in_tx`), M2M read-back, AND FK-existence validation (`validate_on_create_in_tx`, reading the open tx so a child's FK at the uncommitted parent resolves) all on the passed tx; `umbra-rest::create_nested` now opens one `umbra::db::begin()`, inserts parent + all children on it, and `commit()`s — the compensating-delete handler (`compensate` / `scalar_to_string`) is removed. Regression coverage in `crates/umbra-core/tests/dyn_insert_tx.rs` proves true rollback (a failing child leaves zero rows, parent included — never committed), atomic happy-path commit, and FK visibility across the open tx.

**Status (original):** open — compensation workaround in place; true fix is a tx-aware insert.

**Where:** `plugins/umbra-rest` — feature #58 (writable nested serializers). A
`POST /api/order/` with `{ items: [...] }` should create the parent + children in
one **transaction**. The REST plugin writes through the late-bound dynamic path
(`umbra::orm::DynQuerySet::for_meta(meta).insert_json(body)`), which runs on the
ambient pool with auto-commit (`crates/umbra-core/src/orm/dynamic.rs:1010`,
`insert_json`). There is no `insert_json_in_tx(&mut umbra::db::Transaction)` — and
`insert_json` is deeply pool-bound (it re-fetches the row, writes M2M junctions, and
fires `pre_save`/`post_save` signals, all on `pool_dispatched()`), so each child
commits independently. A `umbra::db::Transaction` type exists (`db.rs:348`, used by
the typed `Manager::create_in_tx`), but the dynamic path can't use it.

**Workaround (shipped):** the nested-create handler (`create_nested` in
`plugins/umbra-rest/src/lib.rs`) is **compensating**, not transactional — it inserts
the parent, then each child (FK auto-set from `Column.fk_target`); if any child
fails, it deletes the already-created children + the parent. So a bad child never
leaves a half-created parent (the common case). The gap: a process crash *between*
the parent insert and a child insert could orphan the parent — there's no DB-level
rollback.

**Proper fix:** add `DynQuerySet::insert_json_in_tx(&self, body, &mut Transaction)`
(and route the re-fetch / M2M / signals through the same tx executor), then have
`create_nested` open one `umbra::db::Transaction`, insert parent + all children on
it, and `commit()`. Refactor `insert_json`'s execution tail to be generic over the
executor (pool vs `&mut Transaction`) so both share the build/validate/decode logic.
Add a regression test that a child failure leaves zero rows (true rollback, not
compensation).

---

## 3. No cross-model search: the ORM can't UNION or rank across two models

**Status:** fixed (`feat(orm): Search::across`) — `umbra::orm::Search::across::<(A, B, …)>(query, limit)` searches every text column of each `Searchable` model and returns one `Vec<SearchHit>` ranked by relevance (Postgres inline `ts_rank`/`setweight`, nothing stored; SQLite weighted `LIKE`). The website `render_search` now calls it instead of merging two queries in Rust. Row-visibility is preserved via `Searchable::filter_sql()` (a static `WHERE` fragment — plugins `moderation = 'approved'`, posts `status = 'published'`) plus automatic `deleted_at IS NULL` for soft-deletable models, so unapproved/unpublished/soft-deleted rows don't leak into search. Stored+GIN tsvector remains a logged future optimization.

**Status (original):** open — Rust-side merge in place; a unified ranked search needs its own spec.

**Where:** `umbra_website/plugins/plugin_directory` — `render_search`. The header
command-palette searches BOTH plugins and blog posts and must return one combined
list. The ORM has no surface to UNION two different models' querysets or to rank
results across them, so `render_search` runs two independent queries —
`PluginModel::objects().filter(name/crate/desc contains q).limit(6)` and
`site_content::models::BlogPost::objects().filter(status=published, title/body
contains q).limit(4)` — maps each row into a unified `SearchHit { kind, href, name,
label, short_description, logo }`, and concatenates in Rust (plugins first, then
posts). There is no relevance ranking (order is per-model insertion order, plugins
always before posts), the per-model sub-limits (6 / 4) are arbitrary, and there's no
global "top N across both by score".

**Related — full-text reachability:** the ORM already ships full-text (`FullTextCol::matches`
+ tsvector, used elsewhere), but neither `Plugin` nor `BlogPost` declares a tsvector
column, so this search is `LIKE`-based (`.contains()`), not ranked. A real ranked
search wants tsvector + `ts_rank`, which *also* needs the cross-model UNION to order
hits from different tables against each other.

**Workaround (shipped):** two independent querysets + a Rust-side merge into
`Vec<SearchHit>`, fixed sub-limits, plugins-then-posts ordering. The blog query is
wrapped in `match` + `tracing::warn!` so a missing `site_content_blog_post` table
(e.g. a test DB without that plugin's migrations) degrades to plugins-only instead of
a 500. Acceptable while the combined result set is small and unranked.

**Proper fix (needs a spec):** an ORM surface for cross-model search. Two shapes to
weigh: (a) a `UNION ALL` over a normalized projection (`kind, pk, title, snippet,
rank`) from each participating model with one shared `ORDER BY rank LIMIT N`, exposed
as something like `Search::across::<(Plugin, BlogPost)>(q)`; or (b) a dedicated search
index / materialized view the models feed. The hard part is relevance ranking
(tsvector `ts_rank` vs `LIKE` score) and a stable cross-table ordering key. Postgres-
first; SQLite degrades to `LIKE` + a coarse score. Until then the Rust-merge above is
the recommended shape for a small, unranked combined search.

---

## 4. `#[derive(Model)]` leaks a private model type through generated `pub` items

**Status:** closed (by design) — documented in `documentation/docs/v0.0.1/orm/models.mdx` (a `Callout` on "Declaring models"). A precise compile-time error isn't feasible: the leaking item is generated by a derive that can't see the related model's visibility, and threading visibility through every generated item adds permanent macro complexity for a case the `pub`-by-convention rule already covers. The `form_fk` fixture is `pub` + warning-free (`0b8a379`); the convention is now stated for users.

**Where:** `crates/umbra-core/tests/form_fk.rs`. A model that isn't `pub` trips
rustc's `private_interfaces` lint — and, for a forward-O2O, the hard error E0446 —
because the derive emits `pub` items whose signatures name the (private) model type:
the per-column consts (`author::ID`, …) and, for relations, a reverse-relation
accessor generated **on the OTHER model**. A private `Passport` with
`#[umbra(unique)] holder: ForeignKey<Author>` (forward O2O) made the derive emit a
`pub` reverse-O2O accessor on `Author` returning the private `Passport` → E0446. That
single error failed the whole `umbra-core` test target's compile, silently hiding
every other test in the target (a broken test binary proves nothing).

**Workaround (shipped):** declare any model that participates in a relation `pub` —
which is the convention anyway. Fixed the fixture (`9bcd466` made `Passport` `pub`; a
follow-up makes `Author` / `Book` `pub` too and drops an unused import) so the
`form_fk` target compiles warning-free.

**Proper fix (hard):** a single `#[derive(Model)]` invocation sees only ONE model's
tokens, so it can't know the visibility of the related model on whose `impl` block the
reverse accessor lands — it can't visibility-match an item it emits onto another type.
Options: (a) emit the model's *own* generated items (column consts) with visibility
matching the annotated model, so a `pub(crate)` model gets `pub(crate)` consts — fixes
the `author::ID` leak but not the cross-model reverse accessor; (b) emit a clearer
compile error / `compile_error!` when a relation field targets a model whose visibility
can't be guaranteed, with a message pointing at the "models in relations must be `pub`"
rule; (c) just document the constraint. Low real-world impact (models are `pub` by
convention), so logged-and-tracked rather than worked around silently.
