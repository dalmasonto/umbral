# umbra-signals review

## Verdict

The signals system is genuinely solid for an in-process pub/sub: the registry, ORM wiring, actor envelope, panic isolation for sync handlers, and m2m signals are all fully implemented. The biggest gap is a stale plugin-facing user doc (`plugins/signals.mdx`) that tells users `bulk_create`, `update_values`, and `QuerySet::delete` are "signal-free" when the code and ORM docs confirm they all fire `bulk_post_save`/`bulk_post_delete`. Async handler panics are also unguarded — a panicking async subscriber propagates the panic to the emitter (and therefore to the ORM write that triggered it), whereas sync handlers are already protected with `catch_unwind`.

---

## Completeness (vs Django signals)

### Implemented

- **`pre_save` / `post_save`** — emitted from `Manager::save` (typed, `queryset/mod.rs:4433,4458,4513`) and `DynQuerySet::insert_json` (dynamic, `dynamic.rs:1056,1114,1141`). `post_save` uses the DB read-back row (correct — post-INSERT autoincrement PK is present).
- **`pre_delete` / `post_delete`** — emitted from `Manager::delete_instance` (`queryset/mod.rs:4560,4631`), both before and after the DELETE (or soft-delete UPDATE). The instance value passed is the caller-supplied struct, not a re-fetch — documented correctly.
- **`bulk_post_save`** — emitted from `bulk_create` (ids, created=true), `update_values` (ids, created=false), `update_expr` (ids, created=false) on both typed and dynamic paths.
- **`bulk_post_delete`** — emitted from `QuerySet::delete` and `DynQuerySet::delete` with the affected PK list.
- **`m2m_changed`** — emitted from `M2M::add`, `remove`, `set`, `clear` (`m2m.rs:251,282,312,416`). `clear` guards on `!removed_ids.is_empty()` (silent on empty relation). `set` always fires (see finding #3 below).
- **Custom / generic signals** — `subscribe` / `subscribe_async` / `emit` are available for app-defined events with no restriction on name (other than the documented `<event>:<table>` namespace warning).
- **Actor envelope** — `with_actor` / `current_actor` task-locals wired into `emit` via `with_payload_actor`; all ORM signals carry the actor automatically.
- **Sync handler panic isolation** — `catch_unwind(AssertUnwindSafe(...))` around each sync handler; panicking handler is logged and skipped, remaining handlers run, mutex poison recovered via `into_inner`.
- **Handler ordering** — registration order preserved (Vec, not HashSet).
- **Typed per-model API** — `on_model::<M>().pre_save(...)` / `.post_save(...)` / `.pre_delete(...)` / `.post_delete(...)` with deserialized `&M` rather than raw `serde_json::Value`.
- **Lock discipline** — lock held only to collect futures, dropped before any `.await` (already confirmed safe in race-conditions.md).
- **Dynamic write path (gaps #77)** — `DynQuerySet` (REST + admin) fires the same signal names and payload shapes as the typed Manager paths. Already fully resolved.

### Stubs / Partial

- **Typed API for bulk and m2m signals** — `on_model::<M>()` exposes only `pre_save`, `post_save`, `pre_delete`, `post_delete`. There is no `on_model::<M>().bulk_post_save(...)` or `on_model::<M>().m2m_changed(...)` typed builder. Subscribers to bulk or m2m signals must use the raw `subscribe` / `subscribe_async` functions with the string name `bulk_post_save:<table>` or `m2m_changed:<junction>` and parse the JSON payload manually. Noted as deferred in `plugins/umbra-signals/src/lib.rs:76`.

### Missing

- **Signal `disconnect` / per-handler unsubscribe** — no way to unsubscribe a specific handler. `clear_for_tests()` wipes all handlers; that's the only mechanism. Noted as deferred in `lib.rs:75`.
- **Async handler panic isolation** — see Finding #1 (Important, NEW).
- **Signals not fired from `Manager::create`** — `Manager::create` (`queryset/mod.rs:3629`) issues no signal at all (not even `bulk_post_save`). It is the only write path that is completely signal-invisible. The plugin user doc lists it alongside `bulk_create` as intentionally signal-free, but that claim is now incorrect for `bulk_create` (which does fire `bulk_post_save`). `create` itself is genuinely silent. See Finding #2.
- **`insert_json_in_tx` has no post-commit signal hook** — the transactional insert path fires no signals by design (documented in `dynamic.rs:1167-1174`). The caller owns the commit and therefore owns post-commit signalling. There is no framework mechanism to register a "fire after tx commit" callback. This is a documented gap, not a bug, but it means signals+transactions is a manual coordination pattern today.
- **Cross-process broadcast** — deferred; documented.
- **Typed event enums** — deferred; documented.

---

## Findings

### [Important] Async handler panics are not isolated — `signals.rs:239`

**Severity:** Important
**Status:** NEW

**Description:**
Sync handlers are wrapped in `catch_unwind` (`signals.rs:222`), which keeps a panicking sync handler from propagating the panic to the emitter and from poisoning the registry mutex. The async handler dispatch loop at lines 239–241 has no equivalent protection:

```rust
for fut in futures {
    fut.await;  // if fut panics, the panic unwinds through emit()
}
```

An async handler that panics will unwind through `emit()`, through whichever ORM write called it (`Manager::save`, `DynQuerySet::insert_json`, etc.), and will unwind the entire request. Because the panic crosses an `.await` point, it will also abort the tokio task. In practice this means a subscriber bug (e.g. an index-out-of-bounds on `payload["ids"]`) can kill a POST endpoint entirely rather than being logged and skipped.

The fix is the same pattern used for sync handlers: `tokio::task::spawn` each async future, join it, and catch the `JoinError` (which surfaces panics). Or, more simply, wrap each future in `std::panic::AssertUnwindSafe` and use `futures::FutureExt::catch_unwind`. Either way the error path should log via `tracing::error!` with the signal name and skip to the next handler.

**Fix:** In `signals.rs`, wrap each async future dispatch with panic catching before awaiting, mirroring the sync handler treatment.

---

### [Important] `plugins/signals.mdx` tells users `bulk_create`, `update_values`, and `QuerySet::delete` are "signal-free" — they are not — `documentation/docs/v0.0.1/plugins/signals.mdx:115-118`

**Severity:** Important
**Status:** NEW

**Description:**
The `<Callout type="warning">` at `plugins/signals.mdx:112-133` states:

> The following write paths bypass signals entirely — matching Django's own behaviour:
> - `Manager::bulk_create(vec)` — never fires signals …
> - `QuerySet::update_values(map)` — bulk UPDATE, no signals.
> - `QuerySet::delete()` — bulk DELETE, no signals.

This is false. All three paths fire bulk signals:

- `bulk_create` → `bulk_post_save:<table>` with `created=true` and the inserted PKs (`queryset/mod.rs:3870`).
- `update_values` → `bulk_post_save:<table>` with `created=false` and matched PKs (`queryset/mod.rs:3087`).
- `QuerySet::delete` → `bulk_post_delete:<table>` with deleted PKs (`queryset/mod.rs:2887`).

The ORM-facing doc (`documentation/docs/v0.0.1/orm/signals.mdx`) describes all three correctly. The plugin-facing doc was written before the bulk signals were wired and was never updated.

Impact: a developer reading `plugins/signals.mdx` to decide whether they need signals on a bulk import will conclude "no signals, fine" and miss the fact that their audit-log or cache-invalidation subscriber IS firing on every `bulk_create` call. Conversely, someone who wants audit coverage via signals on bulk writes is told to implement a slow per-row loop when they don't need to.

Note: `Manager::create(instance)` (the single-row, non-signal create path) genuinely fires no signals. That claim in the doc is correct.

**Fix:** Rewrite the `<Callout>` in `plugins/signals.mdx` to distinguish: per-row signals (`pre_save`, `post_save`, `pre_delete`, `post_delete`) do not fire from bulk paths; bulk signals (`bulk_post_save`, `bulk_post_delete`) do. Link to `orm/signals.mdx` for the full table. The only genuinely signal-free typed write path is `Manager::create(instance)`.

---

### [Optional] `m2m::set(&[])` fires `m2m_changed` with `added=[]` and `removed=[prior_children]` — callers may not expect a signal when "setting to empty" — `m2m.rs:416`

**Severity:** Optional
**Status:** NEW

**Description:**
`M2M::clear()` guards on `!removed_ids.is_empty()` before firing the signal (`m2m.rs:310`) — so a `clear()` on an already-empty relation is silent. The ORM doc (`orm/signals.mdx:119`) also states "clear on an empty relation skips emission."

`M2M::set(&[])` does not guard the same way. After the transactional DELETE+INSERT (which on an already-empty relation does nothing because the DELETE returns no rows), `emit_m2m_changed` is called unconditionally at `m2m.rs:416` with `added=[]` and `removed=[]`. A subscriber that checks `payload["added"].len() + payload["removed"].len() == 0` to skip no-ops will skip it, but a subscriber that just logs every `m2m_changed` event will see a spurious emission when `set(&[])` is called on an already-empty relation.

This is inconsistent with `clear()`'s silence-on-empty behaviour. The ORM doc makes no mention of `set`'s unconditional fire.

**Fix:** Add `if added_json.is_empty() && removed_json.is_empty() { return Ok(()); }` before the `emit_m2m_changed` call in `m2m.rs`, matching `clear()`'s guard. Update `orm/signals.mdx` to note this. Alternatively, document the difference explicitly.

---

### [FYI] `Manager::create` is the only write path with zero signal coverage — not documented in the typed-signals table — `queryset/mod.rs:3629`

**Severity:** FYI
**Status:** NEW

**Description:**
`Manager::create(instance)` inserts one row and fires no signal at all — not `pre_save`, not `post_save`, not `bulk_post_save`. This makes it the sole write path that is completely invisible to signal subscribers. The plugin doc does list it in the "signal-free" callout, but the ORM doc's signal table (`orm/signals.mdx:19-27`) does not mention `Manager::create` anywhere, which could mislead a developer reading that doc into thinking all typed inserts produce signals.

This is not a bug — `Manager::create` is documented as the signal-free fast-path in the plugin doc. But the absence from the ORM doc's write-path table is a readability gap. Subscribers that want coverage must use `Manager::save` instead, which fires `pre_save` + `post_save`.

**Fix:** Add a row or footnote to the `orm/signals.mdx` table noting that `Manager::create` fires no signal, with a pointer to `Manager::save` for signal-covered single-row inserts.

---

### [FYI] `plugins/signals.mdx` "What's not shipped" section still lists `m2m_changed` as missing — it is fully implemented — `documentation/docs/v0.0.1/plugins/signals.mdx:188`

**Severity:** FYI
**Status:** NEW

**Description:**
Line 188 of `plugins/signals.mdx` says:

> **`m2m_changed` signals.** Many-to-many relationships aren't in scope until a later milestone.

`m2m_changed` is fully wired: `M2M::add`, `remove`, `set`, and `clear` all fire `m2m_changed:<junction_table>` (`m2m.rs:251,282,312,416`), with documented payload shapes in `orm/signals.mdx:107-118`. The plugin doc's "What's not shipped" section was not updated when m2m signals landed.

**Fix:** Remove the `m2m_changed` bullet from the "What's not shipped" section of `plugins/signals.mdx` and add a brief note under "Generic pub/sub" or in a new "M2M signals" section pointing to `orm/signals.mdx` for the full `m2m_changed` contract.

---

### [FYI] `actor` task-local does not propagate across `tokio::spawn` — documented only in `orm/signals.mdx`, not in `plugins/signals.mdx` — `signals.rs:70-76`

**Severity:** FYI
**Status:** NEW (doc gap only; behaviour is correct)

**Description:**
The `ACTOR` task-local (`signals.rs:70-76`) is a `tokio::task_local!`, which means it resets to the default (access fails, `current_actor()` returns `Value::Null`) when a new tokio task starts. If a handler uses `tokio::task::spawn` for fire-and-forget work inside a `with_actor(...)` scope, the spawned task does not inherit the actor.

The ORM-facing doc (`orm/signals.mdx:146`) calls this out clearly in the Pitfalls section. The plugin-facing doc (`plugins/signals.mdx`) has no equivalent warning, even though the signal handler examples in that doc use `tokio::task::spawn`-like patterns (e.g. calling `umbra_tasks::enqueue`, which internally spawns). A developer whose audit log handler spawns a task inside `with_actor(...)` will silently get `actor: null` in the audit record without any warning.

**Fix:** Add a pitfall callout to `plugins/signals.mdx` matching the one in `orm/signals.mdx:146`, specifically noting that `tokio::task::spawn` inside a signal handler does not inherit the actor and showing the `let actor = current_actor(); with_actor(actor, ...)` capture pattern.

---

## Tests

### Covered

- `integration.rs`: sync handler fires and receives payload; multiple subscribers run in registration order; emit on unknown signal returns 0; async handler awaited to completion; sync + async handlers coexist on one signal.
- `model_signals.rs`: `post_save` fires with `created=true` on INSERT; `pre_save` fires before `post_save` (sequencer test); `post_save` fires with `created=false` on UPDATE; `pre_delete` + `post_delete` fire from `delete_instance`; bulk `update_values` does NOT fire save signals; bulk `QuerySet::delete` does NOT fire delete signals.

### Missing

- **Async handler panic isolation** — no test that a panicking async handler is caught, skipped, and the remaining handlers still run. This would fail today (the panic propagates).
- **`bulk_post_save` and `bulk_post_delete` via typed API** — no test that `bulk_create`, `update_values`, or `QuerySet::delete` fire `bulk_post_save` / `bulk_post_delete`. The `model_signals.rs` tests verify these DON'T fire per-row signals but do not assert the bulk signals DO fire.
- **`m2m_changed` signal** — no test in `umbra-signals` that M2M operations fire the signal. (Tests may exist elsewhere in `umbra-core` but none in the signals plugin's own test suite.)
- **Actor envelope** — no test that the `actor` key is present in the payload a handler receives after a `with_actor(...)` scope.
- **`on_model` typed API deserialization failure path** — no test that a payload whose `"instance"` doesn't deserialize into `M` logs a warning and skips the handler (the `decode_instance` warn path at `lib.rs:140-148`).
- **`m2m::set(&[])` emit behavior** — no test covering the case where `set` is called with an empty slice on an already-empty relation (the inconsistency with `clear` noted in Finding #3).
- **Multiple async handlers in series** — `integration.rs` tests one async handler. No test that two async handlers both run in order when registered on the same signal.
