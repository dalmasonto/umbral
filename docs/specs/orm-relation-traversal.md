# ORM Relation Traversal — Phase 1 design

Status: **design approved 2026-09-02**, awaiting spec review before implementation. Phase 1 of a phased ORM Django-parity epic (see "Phasing" below). Source of the request: a downstream consumer wanting Django-style traversal — `user.developer.software_groups.all()` — from any node of the relationship graph, with deep nesting (5+ hops), mixed relation kinds, and the ability to filter/aggregate on the traversed result. Related historical doc: `relationships.md` (records the v1 FK cut decisions; its deferred items — reverse accessors, M2M, `select_related` — have since shipped and this spec builds on them). See also `03-orm-querysets.md`.

## The goal, in the user's words

Django lets you build every relationship from one point: `user.profile.name`, `profile.user.first_name`, and the deeper `user.developer.software_groups.all()`. The ask is the same reach in umbral: traverse the relationship graph from a loaded object, chain arbitrarily deep across foreign keys, one-to-ones, and many-to-manys, and then run the full query surface (filters, ordering, aggregates, flat-map-style result operations) on whatever the traversal lands on — with the ORM generating the underlying SQL to hit the target in as few round-trips as correctness allows.

## Why this can't be literal Django in Rust

Django's `user.profile.name` works because attribute access on a lazy descriptor secretly runs a query. Rust has no such hook: every DB hop is `async` and must be `.await`ed, and there is no ambient `self.pool` on a model instance. So the umbral equivalent is method-based and awaited. The central design tension: if a to-one accessor returns the *bare object* (`user.developer().await? -> Developer`), the caller is forced to `.await` before the next hop, so a deep chain becomes N separate awaited queries rather than one fluent traversal. "Return the object" and "zero-await deep chaining" cannot both hold for the same call — unless the accessor returns a handle that is *simultaneously awaitable and chainable*. That handle is the core of this design.

## Current surface (what Phase 1 builds on)

Confirmed against the code (`orm/foreign_key.rs`, `one_to_one.rs`, `m2m.rs`, `reverse_set.rs`, `reverse_accessor.rs`, `queryset/mod.rs`, `macros/src/lib.rs`):

- **Reverse FK** already has a chainable, ambient-pooled accessor: the derive emits a per-relation trait with `<child>_set(&self) -> QuerySet<Child>` (`macros/src/lib.rs:2382+`), so `.filter()/.order_by()/.aggregate()/.count()/.fetch()` already compose on it. This is the pattern to generalise.
- **Forward FK / O2O** have NO accessor. You get `.resolved()` (cache after `select_related`) or `.resolve(&pool)` (eager, requires an explicit pool). No ambient-pooled forward accessor exists.
- **M2M forward** is eager-only: `M2M::fetch() -> Vec<T>` or the CRUD (`add/remove/set/clear`). No `QuerySet` is produced, so you cannot `.filter()` or `.aggregate()` a M2M relation.
- **`__` relation traversal in `filter()`/WHERE is absent.** The `__` hop-walkers (`resolve_join_hops`, `resolve_m2m_chain` at `queryset/mod.rs:1218/1265`) exist but are wired only to `select_related`/`join_related`/`values` (reads/projection), never to the WHERE builder. `Predicate<T>` (`orm/mod.rs:164`) wraps an opaque `SimpleExpr` and its keys are always literal columns.
- **Aggregates** (`aggregate`, `annotate`, `annotate_count`, `count`, `exists`, `values`, `distinct`) run on any `QuerySet`, so they already work on the reverse-FK accessor result — and will work on every new `QuerySet`-returning accessor for free.
- **Ambient pool**: every relation accessor builds a fresh `Model::objects()` (`explicit_pool = None`), so chains resolve the ambient pool with no threading. This is preserved.

The net asymmetry Phase 1 removes: forward FK/O2O and M2M become first-class, chainable, ambient-pooled relations, symmetric with reverse FK; and the path→SQL resolver that today only serves reads is extended to drive deep traversal terminals (and, in Phase 2, WHERE filters).

## Locked decisions

1. **Phased.** Ship Phase 1 (this spec) fully tested and mergeable, then iterate. Phasing table below.
2. **To-one returns the object, to-many returns a QuerySet** — semantically. Realised via the awaitable-and-chainable handle so deep chaining is not sacrificed (next section).
3. **Completely chainable, arbitrary depth (5+), mixed kinds.** Accessors never force a mid-chain `.await`.
4. **A pure to-one deep chain resolves to ONE JOIN query at the terminal** (not lazy per-hop). Reuses the `resolve_join_hops` machinery.
5. **Deep-chain leaf terminals dedupe by leaf PK by default** (`SELECT DISTINCT` on the leaf PK), with `.with_duplicates()` to opt out and get raw JOIN multiplicity.
6. **Cross-relation filters (Phase 2) get BOTH a typed builder (`col.to(...).to(...)`) and a Django `__` string** — one resolver under both. The resolution engine lands in Phase 1; the WHERE wiring is Phase 2.

## Design

### The two handle types

- **`Relation<T>`** — a lazy, single-target traversal handle returned by a to-one accessor (forward FK, forward O2O child-side, reverse O2O parent-side). It:
  - implements `IntoFuture<Output = Result<T>>` so `let x = a.b().await?` yields the object directly (honoring decision 2). Awaiting is a REQUIRED resolution: it calls `get()`, which errors (`sqlx::Error::RowNotFound`) if the target row is absent — Django-faithful, where following a reverse-O2O to a missing row raises `DoesNotExist`. The "absence is legitimate" case (nullable FK / reverse-O2O) is served by the explicit `.get_opt().await?` terminal, which returns `Result<Option<T>>` — awaiting the handle directly is always `Result<T>`, never `Result<Option<T>>` (there is no `RelationTarget<T>` type-switch; the shape is chosen by the terminal you call, not by the relation's nullability);
  - carries the accumulated hop path so the next accessor extends it instead of forcing a query;
  - exposes the target model's relation accessors (via the generated trait, below), each returning the *next* `Relation<Next>` or `QuerySet<Next>`;
  - offers explicit terminals `.get().await?` (alias of awaiting → `Result<T>`), `.get_opt().await?` (→ `Result<Option<T>>`), and `.exists().await?`.
- **`QuerySet<T>`** — the existing multi-row builder, returned by a to-many accessor (M2M forward, reverse FK). It gains the same generated relation accessors so a chain can continue past a to-many. All its current terminals (`fetch/first/count/exists/values/aggregate/filter/order_by/...`) apply unchanged.

Crossing semantics: a chain stays `Relation<_>` while every hop is to-one; the first to-many hop widens it to `QuerySet<_>`; subsequent to-one hops become per-row JOINs and subsequent to-many hops fan out — the whole thing still resolves at one terminal. This mirrors Django's `a__b__c` traversal returning leaf rows.

### Generated relation accessors (codegen)

For each model `M`, `#[derive(Model)]` emits a trait `MRelations` with one method per relation on `M`:

- forward FK / O2O child-side / reverse O2O parent-side → `fn <field>(&self) -> Relation<Target>`;
- M2M forward → `fn <field>(&self) -> QuerySet<Target>`;
- reverse FK → keeps the existing `fn <child>_set(&self) -> QuerySet<Child>` (unchanged name; see "Naming").

`MRelations` is implemented for `M`, `&M`, `Relation<M>`, and `QuerySet<M>`, so the chain composes whether it starts from an object or from another relation handle. (Impl'ing a locally-defined trait for the foreign `Relation<LocalM>` / `QuerySet<LocalM>` is orphan-rule-legal because the type parameter is local.) The reverse-accessor trait pattern already in the codebase (`macros/src/lib.rs:2382`) is the precedent; this generalises it to all kinds and to the two handle receivers. All generated relation traits are re-exported through the prelude (glob) so callers do not hand-`use` each one — the single friction point of the existing reverse accessors.

### Path model and SQL resolution

Each handle carries a `RelPath`: an ordered list of hops, each hop recording `{ kind: Fk | O2OChild | O2OParent | M2M | ReverseFk, from_table, to_table, join_on }` — for FK/O2O the FK column and its direction, for M2M the junction table + both junction columns, for reverse-FK the child FK column. The path is built purely in memory as the chain is written; nothing touches the DB until a terminal.

At the terminal:

- **All-to-one path** → one `SELECT <leaf.*> FROM <root> JOIN … JOIN <leaf> WHERE <root.pk> = ?`, built by extending `resolve_join_hops` (which already walks FK chains to arbitrary depth via `registered_models()`) to also handle the O2O directions. One round-trip (decision 4).
- **Path crossing a to-many** → the leaf query becomes a JOIN (or `IN`-subquery through junction tables for M2M hops, reusing `resolve_m2m_chain`) rooted at the starting PK, selecting the leaf rows. `SELECT DISTINCT` on the leaf PK by default (decision 5); `.with_duplicates()` drops the DISTINCT.
- **Further `.filter()/.order_by()/.aggregate()/.count()`** compose onto the leaf `QuerySet` exactly as today — they operate on the leaf table, which the resolver has already established as the query root with the traversal expressed as JOIN/EXISTS conditions.

No raw SQL is introduced: the resolver emits sea-query JOINs/subqueries through the existing builders, so both SQLite and Postgres are covered by one path (per the ORM rules).

### Caching

A single forward FK/O2O hop that was pre-populated by `select_related` is served from `.resolved()` with zero round-trips (the accessor checks the cache first). A multi-hop chain always resolves via the single JOIN query — it does not attempt to stitch per-hop caches (that would reintroduce N+1 and cache-coherence questions); `select_related` remains the tool for pre-hydrating single hops you will read fields off.

### Return shapes and errors

- Required forward FK (`ForeignKey<T>`, NOT NULL) → `Relation<T>`. Await it (`.await?`, i.e. `.get().await?`) → `Result<T>`. A missing target row (referential integrity already broken, since the default DDL is RESTRICT) surfaces as an `Err`, never a silent `None`.
- Nullable forward FK (`Option<ForeignKey<T>>`) and reverse O2O parent-side → `Relation<T>`. Absence is legitimate here, so read them with the `.get_opt().await?` terminal → `Result<Option<T>>` (an absent row is `Ok(None)`, never an error). Awaiting the handle directly is still `Result<T>` and treats an absent row as `Err(RowNotFound)` — same `Relation<T>` type either way, the terminal picks the shape. This is the Django-faithful default: a `.get()`-style await raises on a missing reverse-O2O; `.get_opt()` is the "maybe" read.
- To-many (M2M, reverse FK) → `QuerySet<T>`; empty is an empty `Vec`, never an error.
- All DB errors flow as the framework `Result` error type so `?` composes, consistent with the rest of the ORM.

### Naming

- Accessor name = the field name for forward FK / O2O / M2M (`post.author()`, `developer.user()`, `developer.software_groups()`).
- Reverse FK keeps the existing `<child>_set()` (e.g. `developer.project_set()`) unchanged — no rename, no churn to the shipped surface. A bare-plural reverse alias (`developer.projects()`) is explicitly out of scope for Phase 1 (can be added later without breaking anything).
- Multiple FKs from a child to the same parent keep the existing `<child>_via_<field>_set()` disambiguation.

## Semantics table (mixed chains)

| Chain | Kinds | Resolves to | Returns |
|---|---|---|---|
| `user.developer()` | reverse-O2O | 1 query | `Option<Developer>` |
| `post.author().await?` | fwd FK | cache or 1 query | `User` |
| `user.developer().company().owner().await?` | O2O→FK→FK (all to-one) | **1 JOIN query** | leaf `User` (or `Option` if any hop nullable) |
| `dev.software_groups()` | M2M | lazy `QuerySet` | `QuerySet<SoftwareGroup>` |
| `dev.software_groups().software().all().await?` | M2M→M2M | JOIN/IN to leaf, DISTINCT by PK | `Vec<Software>` (deduped) |
| `dev.software_groups().software().with_duplicates().all().await?` | M2M→M2M | JOIN, no DISTINCT | `Vec<Software>` (raw multiplicity) |
| `dev.project_set().filter(project::ACTIVE.eq(true)).count().await?` | reverse-FK + aggregate | 1 aggregate query | `i64` |

## Testing plan

Behavioral, per the project's testing rule (real rows, the actual public accessor, read the object graph back — not SQL-string assertions as a proxy). New test file(s) under `crates/umbral-core/tests/`:

- one round-trip test per relation kind: forward FK, forward O2O, reverse O2O, M2M (surface `.fetch()` AND chained `.filter().count()`), reverse FK.
- a **5-hop mixed chain** test (to-one → to-one → to-many → to-one → to-many) asserting the leaf rows and that it issues the expected small number of queries (one for the all-to-one prefix pattern; assert via a query counter or `explain`/`to_sql` alongside the round-trip, never instead of it).
- dedupe: a M2M→M2M chain where a leaf is reachable via multiple paths returns each leaf once by default and the full multiplicity under `.with_duplicates()`.
- nullable forward FK → `Option`; missing required FK target → `Err`.
- ambient-pool: the whole chain runs with no `.on(&pool)`.
- SQLite is the required backend; assert Postgres parity by symmetry where a live PG is unavailable, and gate any PG-only assertions behind the existing `UMBRAL_TEST_POSTGRES_URL` harness.

## Files touched

- `crates/umbral-macros/src/lib.rs` — generate `MRelations` trait + impls for `M`/`&M`/`Relation<M>`/`QuerySet<M>`; forward FK/O2O/M2M accessors; keep reverse-FK as-is.
- `crates/umbral-core/src/orm/relation.rs` (new) — the `Relation<T>` handle, `IntoFuture`, `RelPath`, terminals.
- `crates/umbral-core/src/orm/queryset/mod.rs` + a resolver module — extend `resolve_join_hops` to O2O directions and add the terminal path→SQL resolution (JOIN for to-one, JOIN/IN-subquery for to-many, DISTINCT-by-PK default).
- `crates/umbral-core/src/orm/m2m.rs` — the junction-scoped `QuerySet` builder feeding the M2M accessor.
- `crates/umbral-core/src/orm/model.rs` — any relation metadata the resolver needs beyond what `FIELDS`/`M2M_RELATIONS` already carry.
- `crates/umbral/src/lib.rs` + prelude — re-export `Relation`, and glob the generated relation traits into the prelude.
- new test files under `crates/umbral-core/tests/`.
- one user-facing doc page under `documentation/docs/v0.0.1/orm/` (purpose + one example + link here), per ship-a-feature-ship-its-doc.

Work lands on a fresh feature branch off `main` (current working branch is `fix/cargo-audit-rkyv`; the traversal epic is independent of it).

## Phasing (the epic)

- **Phase 1 (this spec):** symmetric chainable accessors + the lazy `Relation`/`QuerySet` traversal engine that resolves deep mixed paths to SQL.
- **Phase 2:** `__` / typed `.to()` cross-relation filters in `filter()`/WHERE (thin reuse of the Phase 1 resolver; wires it to the predicate builder). Closes the WHERE-side of the Django-parity cluster (gap #76) and enables relation-path owner scope (gap #78).
- **Phase 3:** aggregates/annotate over multi-hop relation paths and result flat-map helpers.
- **Phase 4:** the consumer-facing surfaces built on the primitives — REST read-side embed / `?expand=` (gap #72), undeclared reverse prefetch (gap #75), relation-path `owned_via` (gap #78), GraphQL parity.

## Risks and open questions

- **Codegen volume.** One trait + several impls per model. Precedented by the reverse-accessor traits; watch compile-time on large model sets (the shop's 32-model app is the canary). Mitigation: the generated methods are thin (build a `RelPath` and return a handle).
- **Trait import ergonomics.** Prelude glob solves the common case; a chain touching a model whose crate isn't glob-imported still needs a `use`. Acceptable and documented.
- **Mixed-chain terminal semantics.** The "first to-many widens to `QuerySet`" rule must be unambiguous in the types; the generated method return types encode it (to-one on a `Relation` stays `Relation`; any accessor on a `QuerySet` returns a `QuerySet`). Verified as orphan-rule-legal and type-coherent above.
- **Query-count assertions in tests** are inherently a little brittle; they run *alongside* the behavioral round-trip, never as the sole assertion.
- **Three parallel hop→JOIN SQL builders (deferred unification).** As of Task 2, hop→JOIN SQL is built in three places: the Task-1 single-hop subquery (`Relation::terminal_queryset` in `relation.rs`), the all-to-one JOIN builder (`queryset/relation_resolve.rs`), and `apply_join_related` (`queryset/mod.rs`, which serves `select_related` / `join_related`). The brief called for extending the shared `resolve_join_hops` walker to a third consumer, but that was **deferred by controller ruling**: reworking the walker that select_related/join_related depend on, mid-Phase-1, risks regressing tested read paths, and Phase 1's scope is the traversal surface, not a query-builder refactor. **Divergence risk:** any change to how a hop becomes SQL — schema-qualification for multi-tenant PG, column-level encryption (`Masked<T>`), FK-column resolution — must be applied in **all three** builders or they silently drift. Unifying them onto one walker is tracked by the `TODO(orm-traversal, deferred)` breadcrumb at the top of `relation_resolve.rs`.
- **Breaking change (Task 5) — reverse-O2O accessor return type.** The pre-existing auto reverse-O2O accessor (`parent.child()`, emitted for a child's unique/OneToOne FK) previously returned `impl Future<Output = Result<Option<Child>, sqlx::Error>>` — i.e. `parent.child().await?` yielded `Option<Child>`. Task 5 **unified** it with the new parent-side-`OneToOne` accessor into a single chainable one returning `Relation<Child>` (this removed a genuine name collision when a parent declared BOTH a back-link field and the child a unique FK). Because `Relation<T>` awaits to `Result<T>` (required), `parent.child().await?` now yields `Child` and errors on an absent row; the old `Option` shape is read with `.get_opt().await?`. In-repo consumers were migrated (the shop's `examples/shop/src/views/account.rs`, and the admin O2O tests `cross_crate_o2o` / `o2o_child_sugar`); **external consumers of a reverse-O2O accessor must change `.await?` → `.get_opt().await?`** to preserve the `Option` behavior. A v0.0.x pre-1.0 refinement; the gain is that `parent.child()` now composes into a deeper traversal.
