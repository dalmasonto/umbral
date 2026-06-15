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

**Status:** open — compensation workaround in place; true fix is a tx-aware insert.

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
