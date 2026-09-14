# ORM Heavy Relations — epic design (Phases 3–4 + backend hardening)

Status: **design approved 2026-09-14**, awaiting spec review before implementation. Continuation of the ORM Django-parity epic whose Phase 1 (chainable `Relation<T>`/`QuerySet<T>` traversal) and Phase 2 (`__` / typed WHERE filters) already shipped to `main`. See `docs/specs/orm-relation-traversal.md` (the Phase 1 spec, the source of the `RelPath`/`HopSpec`/`walk_forward_joins` machinery this epic builds on) and `03-orm-querysets.md`.

## The goal, in the user's words

> "Work in the ORM level so we can build a proper and heavy ORM that works with nested items well i.e. `user.profile.country.name`, and also for M2M, one-to-many and all those relationships. Make the backend heavy enough for that."

Confirmed against the code: nested traversal across every relationship kind (`user.profile().country()...`, M2M, reverse-FK, 5-hop mixed chains, `__` WHERE filters) **already works and is green** (47 traversal tests pass). This epic closes the remaining gap between "works" and "heavy enough to handle everything, cleanly":

1. **Consolidate** the relation→SQL engine so the framework has ONE place that turns a relation path into JOIN SQL (today: three diverging builders). This is the "heavy backend" ask — robustness, not features.
2. **Aggregate across relation paths** (Phase 3): `annotate(Count("posts__comments"))`, Sum/Avg/Min/Max over a path, ordering and filtering on the result.
3. **Deep eager hydration** (the closest thing to Django's transparent `user.profile.country.name`): one JOIN hydrates a whole to-one chain, and access is via a single cache-aware accessor — the error-prone `.resolved()` step is removed.

## Locked decisions

1. **One epic spec, three implementation plans**, executed in order A → B → C.
2. **Full convergence** for the engine unification (sub-project A): one `RelPath`/`HopSpec` representation and one walker; `JoinHop` / `resolve_join_hops` / `resolve_m2m_chain` are retired, not shimmed.
3. **`select_related` error posture converges to LOUD.** An unresolved relation path returns a clear `Err` instead of the current silent-skip (which degrades to a silent N+1 on a typo). Deliberate behavior change; documented.
4. **Aggregates use a correlated subquery per annotation**, never a shared JOIN + GROUP BY — this makes multiple aggregate annotations correct by construction (no cross-multiplication).
5. **Public `.resolved()` is removed** — but only *after* the accessor is made cache-aware (see the correction below). The single access path to a related object becomes the awaited accessor (`user.profile().await?`), which returns `T` (never `Option`), serving a `select_related` hit with zero round-trips and querying otherwise. `select_related` becomes a pure performance hint, never a correctness prerequisite. In-repo consumers migrate; a pre-1.0 breaking change for external consumers.

## Correction verified against the implementation (2026-09-14)

The Phase 1 spec's Caching section *claimed* "the accessor checks the cache first," but the code does **not** do this: the generated forward-FK/O2O accessor calls `to_one_hop(self, hop)` (`crates/umbral-macros/src/lib.rs:2987`, `:3153`), and `to_one_hop` builds the `Relation` from `RelationSource for &From`, which captures **only the object's primary key** (`relation.rs:155`), never the FK field's `resolved` slot. So `post.author().await?` **always issues a query today**, even immediately after `select_related`. Consequence, folded into sub-project C: **the cache-aware accessor is a hard prerequisite for removing `.resolved()`** — remove the redundant `.resolved()` and you would silently turn every previously-cached read into a fresh query. C wires the short-circuit first, then removes.

---

# Sub-project A — Unify the hop→JOIN engine (foundation)

## Problem

Turning a relation path into JOIN SQL lives in **three** places today, with two different hop representations, two alias schemes, and two error postures:

| Builder | Representation | Aliases | Errors | Used by |
|---|---|---|---|---|
| `relation.rs::terminal_queryset` + `queryset/relation_resolve.rs` (`walk_forward_joins`, `build_to_one_select`, `build_leaf_select`) | `RelPath` / `HopSpec` (direction- and M2M-aware) | `__rel_N`, `__ldm_N` | loud `Err` | traversal terminals |
| `queryset/mod.rs::apply_join_related` + `resolve_join_hops` | `JoinHop` (FK-only) | `__j_*`, `__p` | silent-skip (`None`) | `select_related` / `join_related` / `values` |
| `queryset/mod.rs::resolve_m2m_chain` | ad-hoc tuple | — | silent-skip | M2M-first paths in `select_related` |

Both traversal and `select_related` already use `schema_qualified_table` (multi-tenant is *not* actively drifting today), so this is **drift-prevention and consolidation**, not a live-bug fix. It matters *now* because sub-projects B and C would each add another consumer of hop→JOIN logic; converging first keeps B and C thin. `relation_resolve.rs` already carries the `TODO(orm-traversal, deferred)` breadcrumb calling for exactly this.

## Design

- **Canonical representation: `RelPath` / `HopSpec`** (the richer one). It already models FK, O2O (both directions), M2M (via `JunctionSpec`), and reverse-FK, and already carries the `required` flag (currently metadata-only) — sub-project A starts *reading* `required` to drive the null-join policy.

- **One string resolver — `RelPath::from_path::<T>(path: &str) -> Result<RelPath, sqlx::Error>`.** Walks `__` segments off `T::FIELDS` / the migrate registry / `M2M_RELATIONS` and produces a `RelPath`. Replaces both `resolve_join_hops` and `resolve_m2m_chain`. For non-PK-anchored uses (`select_related`, aggregates) the base is a new `PathBase::TableRoot { table }` variant (no `pk_value`); the existing `PathBase::SinglePk` is unchanged.

- **One walker — `walk_joins(select, root_alias, hops, null_policy, alias_prefix) -> Result<Alias>`.** Pure JOIN emission (no WHERE, no projection). Generalises the existing `walk_forward_joins` to every `HopKind` (adds the M2M junction join and reverse-FK join it doesn't yet emit). Parameters:
  - `null_policy`: `Inner` (traversal — a null link legitimately drops the row, so `get_opt()` sees `None`) vs `LeftForNullable` (hydration / `select_related` — a nullable hop LEFT-joins to preserve the parent row). Driven by `HopSpec::required`.
  - `alias_prefix`: namespaces the per-level aliases so nested/multiple uses in one statement never collide (subsumes the deliberately-distinct `__rel_` / `__j_` prefixes).

- **Callers become thin, owning only their anchoring + projection:**
  - traversal to-one: `WHERE root.pk = ? LIMIT 1`, project leaf columns as bare names (unchanged behavior).
  - traversal crossing-to-many: the junction-walk + `IN`-subquery pivot (unchanged behavior), rebuilt on `walk_joins`.
  - `select_related` / `join_related` / `values`: project the joined levels' columns (prefixed, for C).

- **Retire** `JoinHop`, `resolve_join_hops`, `resolve_join_hops_for`, `resolve_m2m_chain`. Delete the `TODO(orm-traversal, deferred)` breadcrumb once the three builders are one.

## Behavior change (locked decision 3)

`select_related`'s current silent-skip on an unresolved hop becomes a loud `Err` (matching `join_related`, which already errors, and traversal). A typo'd `select_related("athor")` now fails fast instead of silently falling back to N+1. `values` traversal already errors loudly (`unknown_relation_errors_loudly`), so it is unaffected.

## Testing (A)

The **existing suite is the regression net** — every traversal test (47) and every `select_related` / `join_related` / `values` / prefetch test must stay green through the migration; refactor strictly under green (TDD: no behavior change except the documented loud-error one). Add: a test that a typo'd `select_related` path now returns `Err` (was silent). SQLite required; PG parity by symmetry, gated behind `UMBRAL_TEST_POSTGRES_URL`.

## Files (A)

- `crates/umbral-core/src/orm/relation.rs` — `PathBase::TableRoot`; `RelPath::from_path`; read `required`.
- `crates/umbral-core/src/orm/queryset/relation_resolve.rs` — generalise `walk_forward_joins` → `walk_joins` (all hop kinds + null policy + prefix); rebuild `build_to_one_select` / `build_leaf_select` on it; drop the deferred TODO.
- `crates/umbral-core/src/orm/queryset/mod.rs` — rebuild `apply_join_related` on `walk_joins`; delete `JoinHop` / `resolve_join_hops` / `resolve_m2m_chain`.
- doc: `documentation/docs/v0.0.1/orm/` note that `select_related` now errors on an unknown relation.

---

# Sub-project B — Multi-hop aggregates (Phase 3)

## Goal

Django-parity aggregation over relation paths:

```rust
User::objects()
    .annotate_count("posts__comments")               // 2-hop reverse count
    .annotate_sum("post_total", "posts__price")      // sum over a path column
    .filter_annotation("posts__comments_count", Op::Gt, 5.into())  // HAVING-style
    .order_by_annotation("post_total", true)          // already exists
    .fetch().await?;
```

## Design (locked decision 4)

**One correlated subquery per annotation.** Each annotation compiles to `expr_as(<subquery>, alias)` on the main SELECT, where the subquery is `SELECT <AGG>(<leaf.col>) FROM <path…> WHERE <innermost link> = <outer row PK>` — its JOIN chain built by A's `walk_joins`, correlated to the outer row via the base PK. Because each aggregate is computed in its own subquery, **multiple annotations never multiply each other** (the classic Django JOIN-inflation bug is impossible by construction), and no GROUP BY on the base table is required.

- Aggregates: `Count` (with implicit `DISTINCT` on the leaf PK to match the deduping traversal semantics), `Sum`, `Avg`, `Min`, `Max`.
- Public surface: extend `annotate_count(path)` to accept a deep `__` path (today single-hop); add `annotate_sum/avg/min/max(alias, path)`. Keep the existing `annotate_count_where` / `annotate_related` / `order_by_annotation`.
- Filtering on an annotation (Django's `foo__gt=5`): wrap the annotated statement in an outer `SELECT * FROM (…) WHERE alias > ?` so the alias is filterable on both SQLite and Postgres (a portable stand-in for `HAVING`, which correlated subqueries can't reference directly). Surface: `filter_annotation(alias, op, value)`.

## Testing (B)

Behavioral: real rows, `annotate_*` via the public API, read the annotated value back. A parent with two different to-many relations, each annotated, asserting **neither count is inflated** by the other (the anti-Django-bug test). Deep path (`posts__comments`), each aggregate kind, `filter_annotation` cutting rows, `order_by_annotation` ordering by an aggregate. SQLite required; PG-gated parity.

## Files (B)

- `crates/umbral-core/src/orm/queryset/mod.rs` — the `annotate_*` builders + `filter_annotation` + the correlated-subquery emitter (on `walk_joins`).
- new test file `crates/umbral-core/tests/annotate_relation_path.rs`.
- doc page `documentation/docs/v0.0.1/orm/aggregates.mdx`.

---

# Sub-project C — Deep eager hydration + remove `.resolved()` (deepest)

## Goal

```rust
let u = User::objects().select_related("profile__country").get(id).await?;
let country_name = u.profile().await?.country().await?.name;   // zero round-trips, no Option
```

One JOIN hydrates `User → Profile → Country`; the awaited accessors serve it from cache with no further queries and no `.resolved()` ceremony.

## Design

- **Deep `select_related`.** `select_related("profile__country")` accepts a `__` path (today single-hop). One SELECT via `walk_joins` (LEFT-for-nullable, so a null mid-chain keeps the parent), projecting **every level's columns prefixed per level** (`__rel_1__<col>`, `__rel_2__<col>`) to avoid collisions between same-named columns across tables.

- **Recursive row splitter.** A splitter slices the flat joined row by column prefix and hydrates each level's object, threading the cache *recursively*: the `Profile` stored in `User`'s FK cache has its own `Country` already cached. The `values`-traversal path already splits a flat joined row into nested objects (`values_traversal_returns_nested_per_relation_object`), so C reuses that column-mapping and adds typed hydration + recursive cache population on top of the existing single-hop `set_resolved` slots.

- **Make the awaited accessor cache-aware (prerequisite — see the Correction above).** Today `post.author().await?` always queries; the FK field's `resolved` slot is ignored. C changes the source path so a hop off an object whose FK field is already hydrated **short-circuits to the cached object with zero round-trips**. Two viable wirings, chosen in C's plan: (a) `RelationSource for &From` captures the FK field's `resolved` slot alongside the PK, and `to_one_hop`/`get` returns it when present; or (b) the generated accessor checks `self.<field>.resolved()` before building the `Relation`. Either way a multi-hop chain still resolves via one JOIN (the cache short-circuit is for single hydrated hops, matching deep `select_related`). This MUST land before the `.resolved()` removal.
- **Then remove public `.resolved()` (locked decision 5).** With the accessor cache-aware, `user.profile().await?` returns the hydrated object with zero round-trips after `select_related`, or queries otherwise — always `T`, never `Option`, no separate step to forget.
  - Demote `resolved()` / `set_resolved()` (and the O2O / M2M equivalents) to `pub(crate)`. Internal hydration and serde/template serialization read the private `resolved` field directly (`{{ post.author.username }}` keeps working), so they are unaffected.
  - Migrate the ~112 in-repo call sites (mostly integration tests that assert hydration) from `.resolved()` to the awaited accessor. **Wrinkle:** integration tests live outside the crate, so `pub(crate)` breaks them — the tests convert to the accessor (they are asserting the same fact: the object is reachable with zero queries; a query-count assertion alongside preserves the "was it a cache hit" check).
  - Pre-1.0 breaking change for external consumers; documented with the migration (`.resolved()` → `.<field>().await?`).

## Alternative recorded (decision 5 fallback)

If the ~112-site migration proves disproportionate, land `#[deprecated(note = "use the awaited accessor, e.g. post.author().await?")]` on public `.resolved()` first (compiler nudges every caller off it, nothing breaks), and remove it a version later. The end state (no public `.resolved()`) is the same; only the cadence differs. Full removal is the target unless review says otherwise.

## Testing (C)

Behavioral: real `User → Profile → Country` rows; `select_related("profile__country")` then assert `u.profile().await?.country().await?.name` returns the right value **in zero additional queries** (query-counter alongside the round-trip). Nullable mid-chain → the parent row survives (LEFT join) and the accessor's `get_opt()` sees `None`. A test that the public `.resolved()` is gone (compile-fail / doc) and the accessor replaces it. SQLite required; PG-gated parity.

## Files (C)

- `crates/umbral-core/src/orm/queryset/hydration.rs` + `mod.rs` — deep `select_related`, prefixed projection, recursive splitter.
- `crates/umbral-core/src/orm/foreign_key.rs`, `one_to_one.rs`, `m2m.rs` — demote `resolved`/`set_resolved` to `pub(crate)`.
- `crates/umbral/src/lib.rs` / prelude — surface unchanged (no new public type beyond deep `select_related`).
- in-repo migration: `examples/shop`, all `crates/umbral-core/tests/*` using `.resolved()`.
- doc page `documentation/docs/v0.0.1/orm/select-related-deep.mdx` incl. the `.resolved()` → accessor migration note.

---

# Cross-cutting

- **ORM-only, no raw SQL.** Every JOIN/subquery is emitted through sea-query via `walk_joins`, so SQLite and Postgres are covered by one path (per the ORM rules in `CLAUDE.md`).
- **Ambient pool preserved** — every accessor / annotation resolves the ambient pool; no `.on(&pool)` threading.
- **Ship-a-feature-ship-its-doc** — each sub-project adds its user-facing MDX page in the same PR.
- **Commit cadence** — one logical change per commit; full-workspace `fmt`/`clippy`/`build`/`test` before each.

# Risks and open questions

- **A touches tested read paths.** Mitigated: refactor strictly under the existing green suite; the loud-error change is the only intended behavior delta.
- **C's recursive splitter is the deepest work.** Prefixed-column hydration + recursive cache population is new; its plan is the largest and lands last.
- **`.resolved()` removal churn** (~112 sites). The deprecate-first fallback (above) de-risks it if needed.
- **Aggregate portability** — the wrap-and-filter approach for `filter_annotation` is the portable HAVING stand-in; verified against SQLite, PG parity gated.

# Phasing recap (the whole epic)

- Phase 1 — chainable traversal engine. **Shipped.**
- Phase 2 — `__` / typed WHERE filters. **Shipped** (gap #76).
- **This epic:** A (engine unification / hardening) → B (Phase 3 multi-hop aggregates) → C (deep hydration + `.resolved()` removal).
- Phase 4 — consumer surfaces (REST `?expand=` #72, prefetch #75, owner-scope #78) mostly shipped; GraphQL parity remains, out of scope here.
