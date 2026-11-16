# review_4 · umbral-signals

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 1477 LOC (`plugins/umbral-signals`; src 424 + tests 1053) |
| **Reviewer** | review_4 agent |
| **Verdict** | healthy |
| **Findings** | 0 critical · 0 high · 0 medium · 1 low · 3 info — 4 new, 1 known |

## Summary

- umbral-signals is a thin typed layer over umbral-core's in-process signal registry: `on_model::<M>()` returns a `ModelSignals<M>` builder whose methods (`pre_save`, `post_save`, `post_create_txn`, `pre_update`, `post_update`, `pre_delete`, `post_delete`) decode signal payloads into `&M` and register async handlers under the `<event>:<table>` names; `SignalsPlugin` is a marker that declares the registry ready for dependent plugins.
- Overall state is healthy: the crate owns no locks, no `unsafe`, no raw SQL, no lifecycle state — the two historic defects (registry-lock poisoning, swallowed decode errors) are fixed at the right layer and verified held, and the `gaps6 #15` after-commit contract is matched on the plugin side and behaviourally tested both ways.
- The findings are polish, not risk: one unused dependency (SIG-1) and three INFO observations (no typed builders for the bulk/m2m signals the module doc advertises; actor not surfaced by the typed API; `post_create_txn` fails closed on decode failure — fail-closed is the right shape, but the failure mode is undocumented).
- The 1053-line test suite is a genuine strength: it asserts the public path end-to-end (real rows via the ORM, read back, handlers observed), covers ordering, panic isolation, bulk/m2m payloads, actor propagation, and the txn create-rollback.
- Fix first: SIG-1 (one Cargo.toml line). Everything else is a documentation or ergonomics pass a maintainer can schedule freely.

## Findings

### SIG-1 · LOW · hygiene · self-verified
**The `tokio` dependency in `[dependencies]` is unused by the library — the only `tokio` mention in `src/` is a doc comment.**

- **Where**: `plugins/umbral-signals/Cargo.toml:14`, against `src/lib.rs` (424 lines, read in full).
- **Evidence**:
  ```toml
  [dependencies]
  umbral = { path = "../../crates/umbral", version = "0.0.13" }
  tokio = { version = "1", features = ["sync", "rt", "macros"] }
  serde = { version = "1", features = ["derive"] }
  serde_json = "1"
  tracing = "0.1"
  ```
  `grep -n "tokio" plugins/umbral-signals/src/lib.rs` matches exactly one line — `lib.rs:135`: "`tokio::task::spawn` inside the handler for fire-and-forget work." — inside the `ModelSignals` rustdoc. The crate compiles against nothing else: no `use tokio`, no `#[tokio::test]` in `src/`, no `examples/` or `benches/` directory, and every doc fence that references tokio (lib.rs:9, :67, :107, :240) is `ignore`d, so no doctest compiles it.
- **Impact**: consumers that build with `--no-default-features` heuristics or `cargo udeps`-style dependency hygiene see a phantom direct dependency on tokio's `sync`/`rt`/`macros` features; more importantly the entry misleads an integrator into thinking the plugin's public API needs a tokio runtime in scope (it doesn't — the async handlers are awaited by core's `emit` inline on the write path, which is the actual runtime coupling). The cost today is graph noise, not a build failure — hence LOW.
- **Fix**: delete the `[dependencies]` entry; the `[dev-dependencies]` tokio at line 20 (with the richer feature set) already covers the tests. If a future example needs tokio, re-add it where the example lives.

### SIG-2 · INFO · ergonomics
**The module doc advertises the bulk signals (`bulk_post_save`, `bulk_post_delete`, `m2m_changed`) but the typed `ModelSignals<M>` surface has no builders for them — subscribing means dropping to the raw string namespace and manual JSON access, exactly what the typed API claims to spare users.**

- **Where**: `lib.rs:40-49` (module doc) against `lib.rs:174-403` (the typedef surface); the "Deferred past v1" list at `lib.rs:85-89` does not mention them.
- **Evidence**:
  ```rust
  //! **Bulk methods ALSO fire a bulk signal alongside the per-row ones.**
  //! `Manager::bulk_create` and `QuerySet::update_values` fire ONLY
  //! `bulk_post_save:<table>` (payload carries the affected `ids`), never
  //! per-row `post_save`. `QuerySet::delete()` / `DynQuerySet::delete()`
  //! fire BOTH: `bulk_post_delete:<table>` (`ids`) AND a per-row
  //! `post_delete:<table>` for each deleted row …
  ```
  The builder's methods are per-row only (verified by full read: no `bulk_`/`m2m` identifier exists in `src/` outside these doc lines), while `lib.rs:129-130` promises "The handler receives a deserialised `&M` (not a raw `serde_json::Value`), so the caller never writes manual JSON field access" — true for per-row hooks, false for the bulk/m2m paths the same doc advertises. `tests/panic_bulk_m2m.rs` exercises the bulk paths via the raw `subscribe` API (`payload["ids"]`, `payload["action"]`), showing the workaround shape users will hand-roll. Opportunity: add `on_model::<M>().bulk_post_save(...)` / `bulk_post_delete(...)` builders (decoding `ids` into `Vec<M::Pk>`), or state in the doc that bulk/m2m are intentionally raw-level for v1.

### SIG-3 · INFO · ergonomics
**Typed handlers don't receive the actor — the payload carries an `"actor"` key and core exposes `current_actor()`, but the typed closures take only the model (plus `created`/`previous`), and neither actor helper is re-exported by this crate.**

- **Where**: `lib.rs:185-189` (handler signature) and `lib.rs:100` (re-export list); the actor facilities live in `crates/umbral-core/src/signals.rs:104-106` (`pub fn current_actor()`) and `:110-118` (payload enrichment).
- **Evidence**:
  ```rust
  pub fn pre_save<F, Fut>(&self, handler: F)
  where
      F: Fn(&M, bool) -> Fut + Send + Sync + 'static,
  ```
  ```rust
  pub use umbral::signals::{clear_for_tests, emit, subscribe, subscribe_async};
  ```
  A handler that needs "who did this" can call `umbral::signals::current_actor()` inline (async subscribers run in series on the ORM write path, inside the caller's `with_actor` scope), but it must import from `umbral::signals` directly, and the doc-recommended fire-and-forget pattern — `tokio::task::spawn` inside the handler, `lib.rs:135` — loses the task-local, so a spawned continuation silently sees `Value::Null`. Opportunity: pass the actor to typed closures (or document the `current_actor()`/spawn caveat next to the `tokio::task::spawn` advice).

### SIG-4 · INFO · semantics
**`post_create_txn` fails closed on a decode failure — it returns a `WriteError` that rolls the create back — while every other typed hook warns and skips; the `signal_skip`-induced trigger is documented only on `post_delete`.**

- **Where**: `lib.rs:265-279` (the decode branch), against `lib.rs:162-170` (`decode_key` warns and returns `None`) and `lib.rs:380-384` (the note that a required `#[umbral(signal_skip)]` field "will never decode here").
- **Evidence**:
  ```rust
  None => Err(umbral::orm::WriteError::Sqlx(
      umbral::sqlx::Error::Protocol(format!(
          "post_create_txn:{}: could not decode instance from the signal payload",
          M::TABLE
      )),
  )),
  ```
  The fail-closed choice is defensible — silently skipping a handler whose contract is "the related row exists atomically with the create" would break the invariant the method promotes (`lib.rs:228-235`), and the rollback path is behaviourally tested (`tests/model_signals.rs:466`, `failing_post_create_txn_rolls_the_create_back`). The gap is documentation: the doc note about required `signal_skip` fields and decode failures appears only on `post_delete`, yet under `post_create_txn` that same combination turns **every** create of the model into a rolled-back transaction with an opaque `Sqlx(Protocol(...))` error — a loud failure, but one whose cause ("my signal_skip field, or a serde drift") is not discoverable from the error or the docs. Opportunity: one sentence on `post_create_txn` about the failure mode, plus a behavioural test that pins the decode-failure rollback (the suite currently tests handler-`Err` rollback, not decode-`Err` rollback).

## Strengths

- **The decode-failure fix (BROKEN-5) is implemented as a shared choke point, not per-method patches**: every typed hook funnels through `decode_key` (`lib.rs:155-172`), which logs `model`, `key`, and the serde error before returning `None`, and the doc comment records the prior `.ok()` swallow so the regression is self-documenting. One fix, one place.
- **The transactional contract is tested as behaviour, both directions**: `post_create_txn_runs_in_transaction_with_the_decoded_instance` (`tests/model_signals.rs:431`) proves the handler writes in the same tx, and `failing_post_create_txn_rolls_the_create_back` (`:466`) proves an `Err` rolls the create back — real rows, public API, read back after.
- **Panic containment is covered at the plugin layer** even though the machinery is core's: `async_handler_panic_is_isolated_and_other_subscribers_still_fire` and its sync twin (`tests/panic_bulk_m2m.rs:136`, `:174`) pin the "one bad subscriber doesn't kill the write or its siblings" property, alongside core's own suite.
- **Bulk, m2m and actor payloads get observable assertions**: `bulk_create_fires_bulk_post_save_with_ids`, `bulk_delete_fires_bulk_post_delete_with_ids`, `m2m_mutations_fire_m2m_changed_with_action`, `emit_payload_carries_actor_from_with_actor_scope` (`tests/panic_bulk_m2m.rs:205`, `:277`, `:342`, `:405`) — the raw contract is verified, not assumed.
- **Thin-wrapper discipline**: no state, no locks, no `unsafe`, no raw SQL; the plugin marker (`SignalsPlugin`, `lib.rs:405-424`) carries no models/routes/checks, which is exactly the "no special-casing" plugin shape the architecture wants.

## Clean areas checked

- ORM-only rule: `grep -rn "sqlx::query\|query_as\|query_scalar" plugins/umbral-signals/src/` → zero hits; the single `sqlx::` token is the facade re-export `umbral::sqlx::Error::Protocol` (`lib.rs:272`), a type construction, not SQL.
- Facade-only imports: only `umbral::prelude::*`, `umbral::signals::*`, `umbral::db::Transaction`, `umbral::orm::WriteError` — no `umbral_core` path anywhere.
- Panic reachability: no `unsafe`; no `unwrap`/`expect` in `src/` except `payload["created"].as_bool().unwrap_or(false)` (`lib.rs:193`, `:218`) — a deliberate default for a flag core always sets, not a masked error.
- Lock poisoning (review_1 BROKEN-3): held — the plugin registers into core's registry and owns no lock of its own, so the poison-recovery path in `umbral-core` is the single owner of that concern.
- Decode-failure logging (review_1 BROKEN-5): held — see `decode_key` above; a genuinely-absent key stays a quiet `None`, a present-but-undecodable one warns.
- After-commit semantics (gaps6 #15, shipped 2026-09-20): held — the plugin side matches the contract; `post_save`/`post_delete` are async and post-commit by construction, `post_create_txn` is explicitly in-tx with read-back-the-object-graph tests on both outcomes.
- Bulk-vs-per-row contract: module doc (`lib.rs:40-49`) matches core's emit sites spot-checked this sweep; `tests/model_signals.rs:287` asserts per-row silence on `update_values` (its name says "any signal" but the asserted set is the per-row save signals — the assertion itself is correct and the bulk signal belongs to a different subscriber name).
- Feature flags: none declared in `Cargo.toml` — no `default-features` drift surface (contrast: see `umbral-tasks` TASKS-2 in this sweep).
- SQL injection / authorization: not applicable — no SQL, no request path, no auth surface in this crate.

## Known issues already tracked elsewhere (not counted in the totals)

- `planning/gaps6.md` #21(c) — `on_model` doc-comment example calls `umbral_tasks::enqueue` with 2 args; the real signature is 3-arg (`name, payload, EnqueueOptions`) `[open]` — this is the stale-doc-example class entry; kept as known rather than re-numbered.
- `planning/gaps6.md` #15 — the `on_tx()` write path is no longer signal-free: SHIPPED (buffered post-commit flush, never on rollback); the `post_create_txn` contract here matches it and is tested `[fixed/verified held]`
- `review_1` BROKEN-3 — signals registry mutex-poisoning `[fixed/verified held]`
- `review_1` BROKEN-5 — deserialize-failure logging on signal decode `[fixed/verified held]`

## Out-of-scope notes

- `crates/umbral-core/src/signals.rs` — the registry has no `unsubscribe` (only `clear_for_tests` at `:743`), which is why this crate can't offer per-subscription disconnect (`lib.rs:85-89` defers it). That is a core-surface change → routed to the umbral-core review.
