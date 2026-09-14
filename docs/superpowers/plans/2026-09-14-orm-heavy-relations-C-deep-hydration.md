# ORM Heavy Relations — Plan C: Deep eager hydration + remove `.resolved()`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `select_related("profile__country")` hydrates a whole to-one chain in one JOIN, and the awaited accessor (`user.profile().await?.country().await?.name`) serves it from cache with zero round-trips — so the error-prone public `.resolved()` step can be removed entirely.

**Architecture:** Three ordered moves. (1) Make the awaited to-one accessor **cache-aware** — today it always queries because it is built from the PK only; it must short-circuit to the FK field's `resolved` cache when populated. (2) Extend `select_related` to accept a deep `__` path, hydrating every level in one JOIN (prefixed columns + a recursive splitter that threads the cache down the chain). (3) Only then, **remove public `.resolved()`** (demote to `pub(crate)`), migrating in-repo callers to the accessor. The order is mandatory: removing `.resolved()` before the accessor is cache-aware would silently turn cached reads into fresh queries.

**Tech Stack:** Rust, `sea-query` / `sea-query-binder`, `sqlx` (SQLite + Postgres), `#[derive(Model)]` proc-macro, serde (for the FK serialize-when-resolved path).

**Spec:** `docs/specs/orm-heavy-relations-epic.md` (sub-project C + the "Correction verified against the implementation" section).

## Global Constraints

- ORM-only: JOIN/subquery through sea-query; no string SQL.
- Multi-tenant: tables via `schema_qualified_table(...)`.
- Backends: SQLite required for tests; PG parity by symmetry, gated behind `UMBRAL_TEST_POSTGRES_URL`.
- Ambient pool preserved.
- Before each commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` (whole workspace).
- Behavioral tests: real rows, public accessor, read the graph back; a query-counter check runs *alongside* the round-trip to prove a cache hit — never as the sole assertion.
- Depends on Plan A being merged (`walk_joins` with `NullJoinPolicy::LeftForNullable`, `RelPath::from_path` incl. reverse-FK).
- **Security & Performance (binds every task — see the spec's "Security & Performance" section):**
  - The cache-aware accessor is THE latency win: a `select_related` hit is ZERO queries (Task 1 test). Deep `select_related` is ONE JOIN for the whole to-one chain, never N per-hop queries (Task 2 test). Both are query-counter-asserted.
  - Do not make callers pay for depth they didn't ask for: `select_related("a")` hydrates one hop; `select_related("a__b__c")` hydrates three. A bare accessor with no `select_related` issues one lightweight query per hop on demand (Django semantics) — the framework never forces a deep JOIN on a single-hop need.
  - Deep JOINs schema-qualify every level (via `walk_joins`) and apply row scoping (soft-delete/tenancy) to joined levels consistently with a direct read; a deep hydrate must not surface a soft-deleted or cross-tenant intermediate/leaf row.
  - The prefixed deep-projection must not project a `Masked`/hidden column raw (bypassing decryption / the read policy); hydrated nested objects decrypt `Masked` fields exactly as a direct fetch does.
  - Review lens: flag any re-query of a cached relation, any per-hop N+1 in deep hydration, or any hidden/masked column projected raw across a level.

## Interfaces produced by Plan A (consumed here)

- `RelPath::from_path::<T>(path: &str) -> Result<RelPath, sqlx::Error>` — forward FK/O2O, M2M, reverse-FK.
- `walk_joins(select, root_alias, hops, NullJoinPolicy::LeftForNullable, prefix, registered) -> Result<Alias>` — a nullable hop LEFT-joins to keep the parent row.
- `PathBase::TableRoot { table }`, `NullJoinPolicy`.

## File Structure

- `crates/umbral-core/src/orm/relation.rs` — a resolved-value slot on `Relation<T>` + `Relation::from_resolved(obj)`; `get()`/`get_opt()` short-circuit it; `RelationSource for Relation<From>` yields a `SinglePk` base from a resolved value so a chain continues.
- `crates/umbral-macros/src/lib.rs` — the generated forward-FK / O2O accessor checks the field's `resolved()` cache before building the `Relation`.
- `crates/umbral-core/src/orm/queryset/mod.rs` + `queryset/hydration.rs` — deep `select_related`; prefixed projection; recursive cache-populating splitter.
- `crates/umbral-core/src/orm/foreign_key.rs`, `one_to_one.rs`, `m2m.rs` — demote `resolved()`/`set_resolved()`/`set_resolved_opt()` to `pub(crate)`.
- `crates/umbral-core/tests/select_related_deep.rs` (new) — deep-hydration behavioral tests.
- in-repo migration of `.resolved()` call sites (tests + `examples/shop`).
- `documentation/docs/v0.0.1/orm/select-related-deep.mdx` (new) — incl. the `.resolved()` → accessor migration note.

---

### Task 1: Cache-aware to-one accessor (prerequisite)

**Files:**
- Modify: `crates/umbral-core/src/orm/relation.rs`
- Modify: `crates/umbral-macros/src/lib.rs` (forward-FK accessor ~2987, generic accessor emit ~3145-3156; O2O child-side + reverse-O2O `reverse_o2o_impls`)
- Test: `crates/umbral-core/tests/select_related_deep.rs` (new)

**Interfaces:**
- Produces: `Relation<T>` gains `resolved: Option<T>`; `pub fn Relation::from_resolved(obj: T) -> Relation<T>`; `get()` returns the resolved value when present (zero queries), else resolves via `path`; `get_opt()` returns `Ok(Some(obj))` when resolved. `RelationSource for Relation<From>` produces a `SinglePk` base from the resolved value's PK when present (so a further hop chains off it), else the carried `path`.
- Consumes: the FK field's existing `resolved()` accessor (still public at this point — removed in Task 4).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn accessor_serves_select_related_cache_with_zero_queries() {
    // App setup: User { id, name }, Post { id, author: ForeignKey<User> } + 1 user, 1 post.
    let counter = umbral::test_support::QueryCounter::install(); // or the repo's existing counter — see note
    let post = Post::objects().select_related("author").get(post_id).await.unwrap();
    let before = counter.count();
    let author = post.author().await.unwrap();      // MUST NOT query — served from cache
    assert_eq!(author.name, "Ada");
    assert_eq!(counter.count(), before, "cached accessor must issue zero queries");
}
```

Note: use the query-counting mechanism the existing `query_counts.rs` test already relies on (find it in that file and reuse the exact API; do not invent `QueryCounter` if a different one exists).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test select_related_deep accessor_serves_select_related_cache_with_zero_queries`
Expected: FAIL — today `post.author().await?` issues a fresh query (the count increases), because `to_one_hop` builds from the PK only.

- [ ] **Step 3: Implement**

In `relation.rs`, add the slot + constructor + short-circuit:
```rust
pub struct Relation<T: Model> {
    path: RelPath,
    explicit_pool: Option<...>,
    resolved: Option<T>,   // NEW: a pre-hydrated value (from select_related cache)
    _t: PhantomData<T>,
}
impl<T: Model> Relation<T> {
    pub fn from_resolved(obj: T) -> Self {
        // base still carries the object's identity so a further hop can chain.
        let path = RelPath { base: PathBase::SinglePk {
            table: T::TABLE, pk_column: pk_column_name::<T>(), pk_value: obj.primary_key().into(),
        }, hops: Vec::new() };
        Relation { path, explicit_pool: None, resolved: Some(obj), _t: PhantomData }
    }
}
```
`get()` / `get_opt()`: `if let Some(obj) = self.resolved.take_or_clone() { return Ok(obj / Some(obj)); }` before touching the DB. Update `to_one_hop` to set `resolved: None`. Update `RelationSource for Relation<From>` so a resolved handle yields a `SinglePk` base (it already does via `self.path`, which `from_resolved` set correctly — verify).

In `macros/src/lib.rs`, the generated forward-FK / O2O accessor becomes cache-aware:
```rust
fn #accessor_name(&self) -> ::umbral::orm::Relation<#target> {
    if let ::core::option::Option::Some(__c) = self.#field_ident.resolved() {
        return ::umbral::orm::Relation::from_resolved(::core::clone::Clone::clone(__c));
    }
    ::umbral::orm::relation::to_one_hop(self, /* HopSpec literal */)
}
```
Apply to: the forward-FK accessor (~2987 region is O2OReverse; the forward-FK/O2O emit is the `.map(|(name, ret, is_to_one, hop)|` block ~3145 — only the `is_to_one` FK/O2O-forward arm gets the cache check; M2M/to-many do not) and the O2O child-side. For reverse-O2O parent-side there is no local FK field to read, so it keeps `to_one_hop` (no cache slot) — unchanged. `#field_ident` is the model field backing the accessor; the macro already has it in scope where it builds the `HopSpec` (`fk_column`).

- [ ] **Step 4: Run test to verify it passes** + no regression

Run: `cargo test -p umbral-core --test select_related_deep accessor_serves_select_related_cache_with_zero_queries` then the traversal suite (`relation_traversal_integration`, `relation_deep_to_one`, `relation_codegen`).
Expected: new test PASS (zero queries); traversal tests unchanged (an un-hydrated accessor still resolves via `path`).

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/relation.rs crates/umbral-macros/src/lib.rs crates/umbral-core/tests/select_related_deep.rs
git commit -m "feat(orm): cache-aware to-one accessor — serves select_related with zero queries"
```

---

### Task 2: Deep `select_related` (multi-hop, one JOIN, recursive cache)

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`select_related` ~985, `&self` mirror ~4159)
- Modify: `crates/umbral-core/src/orm/queryset/hydration.rs` (`hydrate_select_related`)
- Test: `crates/umbral-core/tests/select_related_deep.rs`

**Interfaces:**
- Consumes: `RelPath::from_path`, `walk_joins(LeftForNullable)`, Task 1's `set_resolved` chain (still `pub(crate)`-visible internally).
- Produces: `select_related("a__b")` accepts a `__` path; one SELECT joins every level (LEFT for nullable), projects each level's columns prefixed (`__rel_1__<col>`, `__rel_2__<col>`); a splitter reconstructs each level's object and populates the nested `resolved` caches so `root.a().await?.b().await?` is zero-query.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn deep_select_related_hydrates_whole_chain_zero_queries() {
    // User { id, name, profile? } is wrong shape — model it as:
    // Country { id, name }, Profile { id, country: ForeignKey<Country> },
    // User { id, profile: ForeignKey<Profile> }. Seed one of each: country "KE".
    let counter = /* reuse query counter */;
    let u = User::objects().select_related("profile__country").get(user_id).await.unwrap();
    let before = counter.count();
    let name = u.profile().await.unwrap().country().await.unwrap().name;
    assert_eq!(name, "KE");
    assert_eq!(counter.count(), before, "deep chain fully hydrated — zero further queries");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p umbral-core --test select_related_deep deep_select_related_hydrates_whole_chain_zero_queries`
Expected: FAIL — today `select_related` is single-hop (a `__` path errors after Plan A's loud-error change, or only hydrates the first hop).

- [ ] **Step 3: Implement**

In `select_related`, resolve the field arg with `RelPath::from_path::<T>(path)`; if it has >1 hop, route to the deep builder. Deep builder: `walk_joins(&mut select, root_alias, &rel.hops, NullJoinPolicy::LeftForNullable, "__rel_", &registered)`; for each level `k` (1..=n) project every column of that level's table as `expr_as(Expr::col((alias_k, col)), Alias::new(format!("__rel_{k}__{col}")))`. In `hydration.rs`, split each fetched row: for the deepest level build the object via a prefixed `FromRow`, then walk *inward* calling the parent level's `set_resolved(child)` (Task-1 chain) so the cache nests. Reuse the flat-row→nested-object column mapping the values-traversal path already uses (`values_traversal_returns_nested_per_relation_object`; impl in `hydration.rs`) — extend it from JSON extraction to typed hydration.

Resolve during implementation (do not defer): a prefixed row can't feed the derived `FromRow` (which reads bare column names). Two options — pick one: (a) build a per-level sea-query sub-`SelectStatement` decoded separately by PK (simpler, one extra tiny query per level — but that defeats "one JOIN"); or **(b)** codegen a `hydrate_prefixed(row, prefix) -> Result<Self>` on `#[derive(Model)]` that reads `<prefix><col>` (preferred: keeps it one JOIN). Prefer (b). If (b)'s macro work is large, land (a) first behind the same public API and open a follow-up gap for (b) — but note (a) is NOT zero-query, so the Step-1 assertion would need per-level counts; prefer (b) to satisfy the zero-query contract.

- [ ] **Step 4: Run to verify it passes** + regression

Run: `cargo test -p umbral-core --test select_related_deep` then `cargo test -p umbral-core --test select_related --test select_related_nested` (single-hop still works — deep is a superset).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/src/orm/queryset/hydration.rs crates/umbral-macros/src/lib.rs crates/umbral-core/tests/select_related_deep.rs
git commit -m "feat(orm): deep select_related — one JOIN hydrates a whole to-one chain"
```

---

### Task 3: Nullable mid-chain keeps the parent (LEFT-join correctness)

**Files:**
- Test: `crates/umbral-core/tests/select_related_deep.rs`
- Modify (if needed): `crates/umbral-core/src/orm/queryset/hydration.rs` (skip `set_resolved` when a level's PK column came back NULL)

**Interfaces:**
- Consumes: `walk_joins(LeftForNullable)` (Plan A) + Task 2's splitter.
- Produces: a nullable FK that is NULL mid-chain leaves the parent row present and that relation un-hydrated; the accessor's `.get_opt().await?` on that hop returns `Ok(None)`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn nullable_mid_chain_keeps_parent_and_yields_none() {
    // Profile { id, country: Option<ForeignKey<Country>> }; seed a Profile with country = NULL,
    // and a User pointing at it.
    let u = User::objects().select_related("profile__country").get(user_id).await.unwrap();
    let profile = u.profile().await.unwrap();               // parent survives (LEFT join)
    assert!(profile.country().get_opt().await.unwrap().is_none()); // absent country -> None
}
```

- [ ] **Step 2: Run to verify it fails/passes**

Run: `cargo test -p umbral-core --test select_related_deep nullable_mid_chain_keeps_parent_and_yields_none`
Expected: FAIL if the splitter blindly hydrates a NULL level (constructs a garbage Country) or if an INNER join dropped the user row.

- [ ] **Step 3: Implement** — in the splitter, before hydrating level `k`, check the level's PK prefixed column; if NULL, skip `set_resolved` for that hop (leave it un-hydrated) and stop descending. Confirm `walk_joins` used `LeftForNullable` so the parent row is not dropped.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p umbral-core --test select_related_deep`
Expected: PASS all three deep tests.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/hydration.rs crates/umbral-core/tests/select_related_deep.rs
git commit -m "feat(orm): deep select_related LEFT-joins nullable hops — parent survives, missing hop is None"
```

---

### Task 4: Remove public `.resolved()` — single access path

**Files:**
- Modify: `crates/umbral-core/src/orm/foreign_key.rs` (`resolved` ~215, `set_resolved` ~225), `one_to_one.rs` (~177/210/219), `m2m.rs` (the M2M `resolved`)
- Migrate: every `.resolved()` call site under `crates/umbral-core/tests/*` and `examples/shop/`

**Interfaces:**
- Produces: `resolved()` / `set_resolved()` / `set_resolved_opt()` are `pub(crate)`. The public read path for a related object is the awaited accessor (Task 1). serde/template serialization is unaffected (it reads the private `resolved` field directly — `foreign_key.rs` serde ~305-330).

- [ ] **Step 1: Confirm the surface + baseline** — `grep -rn '\.resolved()' crates/umbral-core/tests examples plugins --include='*.rs' | grep -v 'fn resolved' | wc -l` (≈112). Run the whole suite green first: `cargo test` — baseline.

- [ ] **Step 2: Demote to `pub(crate)`** — change `pub fn resolved` → `pub(crate) fn resolved` (and `set_resolved`, `set_resolved_opt`) in `foreign_key.rs`, `one_to_one.rs`, `m2m.rs`. Build: `cargo build -p umbral-core` — this compiles (internal callers are in-crate). Integration tests + `examples/shop` will now FAIL to compile (they are external) — that is expected and is the migration surface.

- [ ] **Step 3: Migrate call sites to the accessor** — for each failing site, replace `let a = x.field.resolved().unwrap();` with `let a = x.field().await.unwrap();` (or `.get_opt().await?` for a nullable/optional relation). Where a test's *point* was "select_related hydrated it," keep the assertion meaningful by wrapping with a query-counter check (zero further queries) so it still tests the cache, not just reachability. Work file-by-file; re-run each test binary after its file compiles: `cargo test -p umbral-core --test <name>`. Migrate `examples/shop` similarly (`cd examples/shop && cargo build`).

- [ ] **Step 4: Full workspace green**

Run: `cargo build && cargo test` (whole workspace) and `cd examples/shop && cargo build`.
Expected: clean, no remaining public `.resolved()` references outside the crate. Final check: `grep -rn '\.resolved()' crates plugins examples --include='*.rs' | grep -v 'fn resolved' | grep -v 'crates/umbral-core/src/'` returns nothing.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(orm)!: remove public .resolved() — the awaited accessor is the single access path"
```

**Fallback (spec-recorded):** if the migration proves disproportionate, instead land `#[deprecated(note = "use the awaited accessor, e.g. post.author().await?")]` on the public `resolved()` (nothing breaks, the compiler nudges callers) and open a follow-up gap to remove it a version later. Same end state; different cadence. Full removal is the target unless review says otherwise.

---

### Task 5: Docs + full-workspace verification

**Files:**
- Create: `documentation/docs/v0.0.1/orm/select-related-deep.mdx`

- [ ] **Step 1: Write the doc** — purpose (deep `select_related("a__b__c")` hydrates a whole to-one chain in one JOIN; access via the awaited accessor which is now the single, cache-aware path), one example, and a `<Callout>` documenting the `.resolved()` removal + the `.resolved()` → `.<field>().await?` migration. Link to the spec. Frontmatter `title`/`description`/`sidebar_position`. Prose not hard-wrapped.

- [ ] **Step 2: Full workspace gate**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add documentation/docs/v0.0.1/orm/select-related-deep.mdx
git commit -m "docs(orm): deep select_related + .resolved() removal migration note"
```

---

## Self-Review

- **Spec coverage:** cache-aware accessor prerequisite (Task 1), deep `select_related` one-JOIN + recursive cache (Task 2), nullable-mid-chain LEFT correctness (Task 3), public `.resolved()` removal + migration (Task 4), doc (Task 5). All of sub-project C's "Design" bullets + the "Correction" ordering map to tasks. ✓
- **Ordering guard:** Task 1 (cache-aware) precedes Task 4 (removal), so the removal never regresses cached reads — the spec's hard prerequisite is encoded in task order. ✓
- **Placeholder scan:** the `hydrate_prefixed` (Task 2) and query-counter (Task 1) markers are in-task decisions with a chosen default (option (b); reuse `query_counts.rs`'s counter), not deferrable TODOs. ✓
- **Type consistency:** `Relation::from_resolved` / the `resolved` slot from Task 1 are consumed by the Task-2 splitter's `set_resolved` chain and the Task-4 accessor migration; signatures stated once and reused. ✓
- **Mandatory tests present:** zero-query cache hit (Task 1), zero-query deep chain (Task 2), nullable→None (Task 3). ✓
