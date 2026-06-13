# ORM fixes & gaps found during the website build-out

Entries logged when a needed query couldn't be expressed (or a documented ORM
capability didn't behave). Numbers are stable identifiers. Each entry: the
symptom, where it bit, the workaround in place, and the proper fix.

---

## 1. `prefetch_related("<field>")` returns empty buckets for a SECOND reverse-FK field on one model

**Status:** open — workaround in place, proper fix pending.

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
