# ORM Relation Traversal (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every umbral model symmetric, chainable object→relation accessors so a developer can traverse the relation graph from any loaded object, deep-chaining 5+ hops across FK/O2O/M2M, and run the full query surface (filter/order/aggregate) on the result.

**Architecture:** A to-one accessor returns a lazy `Relation<T>` that implements `IntoFuture` (so `.await` yields the object) and carries an in-memory `RelPath`; chaining another accessor extends the path with no mid-chain await. A to-many accessor (M2M forward, reverse-FK) returns a chainable `QuerySet<T>`. The first to-many hop widens the chain from `Relation` to `QuerySet`. At the terminal, an all-to-one path resolves to one JOIN `SELECT`; a path crossing a to-many resolves to a JOIN/`IN`-subquery to the leaf with `SELECT DISTINCT` on the leaf PK (opt out via `.with_duplicates()`). The derive macro generates a per-model `…Relations` trait carrying the accessors, implemented for `M`, `&M`, `Relation<M>`, and `QuerySet<M>`, and re-exported through the prelude.

**Tech Stack:** Rust, `sqlx` (SQLite + Postgres), `sea-query`/`sea-query-binder` (all SQL — no raw strings), `syn`/`quote` (the derive in `crates/umbral-macros`), `tokio`.

**Spec:** `docs/specs/orm-relation-traversal.md` (read it first; it carries the locked decisions and the current-surface map).

## Global Constraints

- **All SQL goes through the ORM / sea-query builders** — never `sqlx::query("…")` in non-test code (CLAUDE.md ORM rule). The resolver emits sea-query JOINs/subqueries so SQLite and Postgres are both covered by one path.
- **Ambient pool:** every accessor builds a fresh `Model::objects()` (`explicit_pool = None`) so chains resolve the ambient pool with no threading. Tests may pass an explicit pool via the existing `.on(&pool)` / `QuerySet::on` seam.
- **Behavioral tests only** (CLAUDE.md / memory `feedback_behavioral_tests_not_asserts`): real rows through the actual public accessor, read the object graph back. SQL-string / query-count assertions are allowed only *alongside* a round-trip, never as the sole assertion.
- **Never wipe a DB or delete migrations.** Tests use in-memory / temp SQLite pools they create themselves.
- **Branch:** `feat/orm-relation-traversal` (already created off `origin/main` = commit `dc52a46b`; the spec commit is already there). Do NOT branch off local `main` (it is 91 commits stale).
- **Commit attribution** on every commit:
  ```
  Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_0182eTEe1unSyz6PkB3DXvwC
  ```
- **Facade rule:** any new public type a plugin/user needs lives in `umbral-core` and is re-exported from `umbral` (and, if commonly used, the prelude). Macro output lives in `umbral-macros`.
- **Verify before each commit:** `cargo fmt`, `cargo clippy -p <crate> --all-targets`, `cargo build --workspace`, and the task's tests. Retry a single test once on `SQLITE_BUSY` (known flake).

---

## File Structure

- **Create `crates/umbral-core/src/orm/relation.rs`** — the `Relation<T>` handle, `RelPath` / `RelHop` / `HopKind` / `JunctionSpec`, the `RelationSource` trait, the public `to_one_hop` / `to_many_hop` helpers the macro calls, and the `IntoFuture` impl + `.get()` / `.exists()` terminals. One responsibility: the lazy to-one handle and hop model.
- **Create `crates/umbral-core/src/orm/queryset/relation_resolve.rs`** — the path→SQL resolver: `resolve_to_one_path` (all-to-one → one JOIN SELECT) and `resolve_leaf_queryset` (a `RelPath` → a `QuerySet<Leaf>` carrying the accumulated JOIN/`IN`-subquery predicate, DISTINCT-by-PK default). Extends the existing hop-walking logic in `queryset/mod.rs:1218/1265`; keep the walker generic and shared.
- **Modify `crates/umbral-core/src/orm/m2m.rs`** — add `M2M::query(&self) -> QuerySet<T>` returning the junction-scoped chainable set (the mechanism behind the generated M2M accessor).
- **Modify `crates/umbral-core/src/orm/queryset/mod.rs`** — add `QuerySet::with_duplicates()` (drops the leaf DISTINCT) and a `pub(crate)` constructor that seeds a `QuerySet` from a resolved leaf predicate; wire `mod relation_resolve;`.
- **Modify `crates/umbral-core/src/orm/mod.rs`** — `pub mod relation;` and re-exports.
- **Modify `crates/umbral-macros/src/lib.rs`** — in `expand_model`, generate the `<Model>Relations` trait (accessor per FK/O2O/M2M relation) and its impls for `Model` / `&Model` / `Relation<Model>` / `QuerySet<Model>`; keep reverse-FK `<child>_set()` exactly as-is.
- **Modify `crates/umbral/src/lib.rs`** — re-export `Relation` and glob the generated relation traits into the prelude.
- **Create tests** under `crates/umbral-core/tests/` (engine + integration) — see tasks.
- **Create `documentation/docs/v0.0.1/orm/relation-traversal.mdx`** — the user-facing page.

---

## Task 1: `Relation<T>` handle + single forward-FK resolution

**Files:**
- Create: `crates/umbral-core/src/orm/relation.rs`
- Modify: `crates/umbral-core/src/orm/mod.rs` (add `pub mod relation;`)
- Modify: `crates/umbral/src/lib.rs` (re-export `Relation`)
- Test: `crates/umbral-core/tests/relation_handle.rs`

**Interfaces:**
- Produces:
  - `pub struct Relation<T: Model> { /* path: RelPath, root_pk: PkValue, explicit_pool: Option<DbPool>, _t: PhantomData<T> */ }`
  - `pub(crate) enum HopKind { Fk, O2OForward, O2OReverse, M2M, ReverseFk }` — `Fk`/`O2OForward`/`O2OReverse` are to-one; `M2M`/`ReverseFk` are to-many.
  - `pub struct HopSpec { pub kind: HopKind, pub from_table: &'static str, pub to_table: &'static str, pub fk_column: &'static str, pub fk_on_from: bool, pub junction: Option<JunctionSpec> }` (the static descriptor the macro passes).
  - `pub struct JunctionSpec { pub table: &'static str, pub parent_column: &'static str, pub target_column: &'static str }`
  - `pub trait RelationSource<From: Model> { fn into_path_base(self) -> PathBase; }` impl'd in this task for `&From` (→ `PathBase::SinglePk { table: From::TABLE, pk }`) and `Relation<From>` / `&Relation<From>` (→ carry the existing path).
  - `pub fn to_one_hop<From: Model, To: Model>(src: impl RelationSource<From>, hop: HopSpec) -> Relation<To>`
  - On `Relation<T>`: `pub async fn get(self) -> Result<RelTarget<T>>`, `pub async fn exists(self) -> Result<bool>`, and `impl IntoFuture for Relation<T>` delegating to `get()`. `RelTarget<T>` is `T` for a required hop, `Option<T>` for a nullable/reverse hop — encode via a `const REQUIRED: bool` on the terminal hop; for Task 1 implement the required-FK case returning `Result<T>` and a `get_opt() -> Result<Option<T>>`; unify in Task 2.
- Consumes: existing `ForeignKey::resolve`/`resolve_pg` (`orm/foreign_key.rs:253/281`), the ambient pool resolver `resolve_pool` (`queryset/mod.rs:1362`), `Model::TABLE`/`Model::PrimaryKey`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/umbral-core/tests/relation_handle.rs
// Two models: Post has a required ForeignKey<Author>. Build a Relation for the
// forward FK by hand (codegen comes in Task 5) and prove it awaits to the row.
use umbral::prelude::*;
use umbral::orm::relation::{to_one_hop, HopSpec, HopKind};

#[derive(Model, sqlx::FromRow, serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Author { #[umbral(primary_key)] id: i64, name: String }

#[derive(Model, sqlx::FromRow, serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Post { #[umbral(primary_key)] id: i64, title: String, author: ForeignKey<Author> }

#[tokio::test]
async fn forward_fk_relation_awaits_to_the_row() {
    let pool = umbral::test_support::sqlite_pool().await; // create schema for author+post, insert 1 author + 1 post
    // ... insert author id=1 "ada", post id=1 author=1 (use the crate's existing test harness helpers) ...
    let post = Post::objects().on(&pool).get(1).await.unwrap();

    let hop = HopSpec { kind: HopKind::Fk, from_table: "post", to_table: "author",
                        fk_column: "author", fk_on_from: true, junction: None };
    let author: Author = to_one_hop(&post, hop).on(&pool).get().await.unwrap();
    assert_eq!(author.name, "ada");         // real row read back through the handle
}
```
(Use whatever schema-setup / in-memory-pool helper the existing `crates/umbral-core/tests/*` use — grep a neighbouring test like `relationships`/`select_related` for the exact harness; mirror it. Add `.on(&pool)` support on `Relation` in this task if the ambient pool isn't set in tests.)

- [ ] **Step 2: Run test to verify it fails** — `cargo test -p umbral-core --test relation_handle -- forward_fk_relation_awaits_to_the_row` → FAIL (`to_one_hop` / `Relation` undefined).

- [ ] **Step 3: Write minimal implementation** — create `relation.rs` with the types above. `to_one_hop` builds a `Relation<To>` whose `RelPath` is `src.into_path_base()` extended by `hop`. Add `Relation::on(pool)` storing `explicit_pool`. `get()` for a single-hop `Fk` path: read the root PK, run `To::objects().filter(<to.pk> = <fk value read from src>)`… — but for Task 1 the simplest correct impl is: resolve the FK value from the source object's `author.id()` carried in the path base and call `To::objects().filter(<To pk>.eq(fk_value)).first()` using the resolver. Return `Result<To>` erroring if `None`.

- [ ] **Step 4: Run test to verify it passes** — same command → PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/umbral-core/src/orm/relation.rs crates/umbral-core/src/orm/mod.rs crates/umbral/src/lib.rs crates/umbral-core/tests/relation_handle.rs
git commit -m "feat(orm): Relation<T> handle with single forward-FK resolution" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0182eTEe1unSyz6PkB3DXvwC"
```

---

## Task 2: All-to-one multi-hop JOIN resolution + nullable→Option

**Files:**
- Create: `crates/umbral-core/src/orm/queryset/relation_resolve.rs`
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`mod relation_resolve;`, extend the shared hop walker to O2O directions)
- Modify: `crates/umbral-core/src/orm/relation.rs` (`get()` delegates multi-hop paths to `resolve_to_one_path`; unify `RelTarget` / `get_opt`)
- Test: `crates/umbral-core/tests/relation_deep_to_one.rs`

**Interfaces:**
- Produces: `pub(crate) async fn resolve_to_one_path<Leaf: Model>(path: &RelPath, root_pk: &PkValue, pool: &DbPool) -> Result<Option<Leaf>>` — emits ONE `SELECT <leaf.*> FROM <root> JOIN … JOIN <leaf> WHERE <root.pk> = ?` via sea-query, decoding the leaf row. Handles `Fk`/`O2OForward` (fk on the near table) and `O2OReverse` (fk on the far table) join directions.
- Consumes: `crate::migrate::registered_models()` for per-hop `fk_target` lookup (as `resolve_join_hops` at `queryset/mod.rs:1226` already does), the existing sea-query JOIN builders.

- [ ] **Step 1: Write the failing test** — three-hop all-to-one chain (`Post.author` FK → `Author.company` FK → `Company.owner` FK), built by chaining `to_one_hop` three times, awaits to the leaf `User`, and a nullable middle FK yields `Ok(None)`. Include a query-count probe via `to_sql()` asserting a single JOIN statement (alongside the round-trip, not instead of).

```rust
#[tokio::test]
async fn three_hop_to_one_resolves_in_one_join_query() {
    // schema: author.company -> company, company.owner -> user; insert a full chain
    let rel = to_one_hop(&post, hop_author)
        .company()               // NOTE: until Task 5, extend via to_one_hop(rel, hop_company)
        ; // ...
    let owner: User = /* to_one_hop(rel, hop_owner) */ .on(&pool).get().await.unwrap();
    assert_eq!(owner.email, "boss@acme.test");
}
```
(Before Task 5 there is no `.company()` method; build the 3-hop path by nesting `to_one_hop(to_one_hop(&post, hop_author), hop_company)` then `hop_owner`. Show the explicit nested form in the real test.)

- [ ] **Step 2: Run to verify it fails** — FAIL (multi-hop `get()` still does the single-hop path / method missing).
- [ ] **Step 3: Implement** `resolve_to_one_path` in `relation_resolve.rs`; make `Relation::get()` call it for `hops.len() > 1` (and optionally for 1 too, unifying). Unify `RelTarget`: `get()` returns `Result<T>` when the terminal hop `REQUIRED`, else `get_opt()` / `Option`. Encode `required` on `HopSpec` (nullable FK / reverse O2O → not required).
- [ ] **Step 4: Run to verify it passes** — PASS; assert the `to_sql` contains exactly the expected JOIN count.
- [ ] **Step 5: Commit** (`feat(orm): resolve all-to-one relation chains in one JOIN query`).

---

## Task 3: M2M chainable QuerySet accessor

**Files:**
- Modify: `crates/umbral-core/src/orm/m2m.rs` (add `pub fn query(&self) -> QuerySet<T>`)
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (a `pub(crate)` predicate-seed helper if needed)
- Test: `crates/umbral-core/tests/relation_m2m_query.rs`

**Interfaces:**
- Produces: `M2M<T,P>::query(&self) -> QuerySet<T>` — returns `T::objects()` with an injected `Predicate::new(<T.pk> IN (SELECT <junction.target_column> FROM <junction> WHERE <junction.parent_column> = <parent_id>))` (sea-query subquery), ambient-pooled. Also `pub fn to_many_hop<From, To>(src, hop) -> QuerySet<To>` in `relation.rs` for the general path (M2M / reverse-FK hop off a `Relation`/object).
- Consumes: `M2M.junction_table` / `parent_id` / target metadata (`m2m.rs:82`), `Predicate::new` (`orm/mod.rs:194`).

- [ ] **Step 1: Write the failing test** — a `Developer` with `M2M<SoftwareGroup>`; seed 3 groups, attach 2; `dev.software_groups.query().filter(software_group::ACTIVE.eq(true)).order_by(software_group::NAME.asc()).fetch()` returns exactly the attached-and-active rows, and `.count()` returns the attached count. (Field access `dev.software_groups` is the `M2M` value; `.query()` is the new method. The generated bare `dev.software_groups()` comes in Task 5 and will delegate here.)
- [ ] **Step 2: Run to verify it fails** — FAIL (`M2M::query` undefined).
- [ ] **Step 3: Implement** `M2M::query` building the subquery predicate; confirm it composes with existing `.filter/.order_by/.count/.aggregate` (no terminal changes needed).
- [ ] **Step 4: Run to verify it passes** — PASS.
- [ ] **Step 5: Commit** (`feat(orm): chainable QuerySet from an M2M relation via junction subquery`).

---

## Task 4: Crossing to-many + DISTINCT-by-PK default + `.with_duplicates()`

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/relation_resolve.rs` (`resolve_leaf_queryset` for a path crossing a to-many)
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`pub fn with_duplicates(self) -> Self`; the leaf DISTINCT flag)
- Modify: `crates/umbral-core/src/orm/relation.rs` (`to_many_hop` on a multi-hop `Relation`/`QuerySet` source resolves via `resolve_leaf_queryset`)
- Test: `crates/umbral-core/tests/relation_deep_to_many.rs`

**Interfaces:**
- Produces: `resolve_leaf_queryset<Leaf: Model>(path: &RelPath, root_pk: &PkValue) -> QuerySet<Leaf>` — builds `Leaf::objects()` with the traversal expressed as JOINs / `IN`-subqueries rooted at `root_pk`, `SELECT DISTINCT` on `Leaf` PK by default. `QuerySet::with_duplicates()` clears the DISTINCT flag.
- Consumes: `resolve_m2m_chain` (`queryset/mod.rs:1265`), the DISTINCT machinery (`queryset/mod.rs:1126`).

- [ ] **Step 1: Write the failing test** — `dev.software_groups() → .software()` (M2M→M2M) where one `Software` is reachable via two groups: default `.all()` returns it once; `.with_duplicates().all()` returns it twice. (Build the two-hop to-many path via nested `to_many_hop` until Task 5.)
- [ ] **Step 2: Run to verify it fails** — FAIL.
- [ ] **Step 3: Implement** `resolve_leaf_queryset` + `with_duplicates`.
- [ ] **Step 4: Run to verify it passes** — PASS (both dedupe and multiplicity assertions).
- [ ] **Step 5: Commit** (`feat(orm): resolve to-many relation chains to the leaf, DISTINCT by PK`).

---

## Task 5: Codegen — `<Model>Relations` trait + accessors

**Files:**
- Modify: `crates/umbral-macros/src/lib.rs` (in `expand_model`)
- Test: `crates/umbral-core/tests/relation_codegen.rs`

**Interfaces:**
- Produces, per model `M`, a trait `MRelations` with one method per forward relation:
  - forward FK / O2O child-side / reverse O2O parent-side → `fn <field>(&self) -> Relation<Target>` (body: `to_one_hop(self, HopSpec { … static … })`).
  - M2M forward → `fn <field>(&self) -> QuerySet<Target>` (body: `to_many_hop(self, HopSpec { … })`; when the receiver is the owning object it may delegate to the `M2M::query` value directly).
  - implemented for `M`, `&M`, `Relation<M>`, `&Relation<M>`, `QuerySet<M>` (orphan-legal: the trait is local; the type parameter is local).
  - reverse-FK `<child>_set()` is UNCHANGED (already generated at `macros/src/lib.rs:2382`).
- Consumes: the `FieldSpec` FK metadata + `M2M_RELATIONS` the derive already reads; `HopSpec`/`to_one_hop`/`to_many_hop` from Task 1/3.

- [ ] **Step 1: Write the failing test** — real models with each relation kind; assert the object-rooted single hop (`post.author().await?`) and a deep mixed chain (`user.developer().software_groups().software().all().await?`) both compile and round-trip, using ONLY the generated accessors (no manual `to_one_hop`). Import via `umbral::prelude::*`.
- [ ] **Step 2: Run to verify it fails** — FAIL (methods don't exist / trait not in scope).
- [ ] **Step 3: Implement** the codegen. Emit the trait + impls next to the existing reverse-accessor emission. Keep each generated method a thin `HopSpec` literal + helper call. Ensure the M2M accessor name = field name and the to-one accessor name = field name; guard against collision with an existing inherent method (documented limitation if a field is named like a terminal).
- [ ] **Step 4: Run to verify it passes** — PASS. Also run the full `cargo test -p umbral-core` + `cargo build --workspace` (codegen touches every model — the shop's 32-model app is the compile canary; if compile-time balloons, note it).
- [ ] **Step 5: Commit** (`feat(macros): generate chainable relation accessors per model`).

---

## Task 6: Prelude glob + facade re-exports

**Files:**
- Modify: `crates/umbral/src/lib.rs` (prelude re-exports `Relation`; glob the generated relation traits)
- Modify: `crates/umbral-macros/src/lib.rs` if the trait needs a discoverable path for the glob
- Test: `crates/umbral-core/tests/relation_prelude.rs`

- [ ] **Step 1: Write the failing test** — a file that imports ONLY `use umbral::prelude::*;` and runs a deep chain; it must compile and pass without any explicit `use …Relations;`.
- [ ] **Step 2: Run to verify it fails** — FAIL (trait not in scope from prelude).
- [ ] **Step 3: Implement** — decide the glob mechanism: either the derive emits the trait into a well-known module the prelude re-exports, or the prelude re-exports a marker that pulls them. If a blanket glob isn't feasible for user-crate-defined traits, document that user models need `use <crate>::*` for their own relation traits and that built-in/plugin model traits are preluded; adjust the test to match the honest surface.
- [ ] **Step 4: Run to verify it passes** — PASS.
- [ ] **Step 5: Commit** (`feat(orm): re-export Relation and relation traits through the prelude`).

---

## Task 7: Full behavioral suite

**Files:**
- Test: `crates/umbral-core/tests/relation_traversal_integration.rs`

- [ ] **Step 1: Write the tests** — one behavioral test per row:
  - forward FK → object; nullable forward FK → `Option`; missing required FK target → `Err`.
  - forward O2O child-side → object; reverse O2O parent-side → `Option`.
  - M2M forward → QuerySet; surface `.fetch()` AND chained `.filter().count()`.
  - reverse-FK `<child>_set()` still works + `.aggregate()` on it.
  - a **5-hop mixed chain** (to-one → to-one → to-many → to-one → to-many) returning the deduped leaf set, with a `to_sql`/count probe alongside asserting the small statement count.
  - the whole suite runs with the ambient pool (no `.on(&pool)` on the chain) to prove pool-free chaining — set the ambient pool via the crate's test harness.
- [ ] **Step 2: Run to verify** the new-behavior tests initially fail where they exercise a not-yet-covered combination; fix any gap in Tasks 1–5 (do not weaken a test to make it pass).
- [ ] **Step 3–4:** Green the whole file; then `cargo test -p umbral-core` + `cargo build --workspace` clean.
- [ ] **Step 5: Commit** (`test(orm): behavioral suite for relation traversal — per-kind + 5-hop mixed chain`).

---

## Task 8: User-facing doc page

**Files:**
- Create: `documentation/docs/v0.0.1/orm/relation-traversal.mdx`

- [ ] **Step 1:** Write the page: one paragraph on purpose (traverse from any object, deep-chain, filter/aggregate the result), the smallest example (`user.developer().await?` and the deep `dev.software_groups().software().filter(...).fetch().await?`), the to-one→object / to-many→QuerySet rule, the DISTINCT-by-PK default + `.with_duplicates()`, and a link to `docs/specs/orm-relation-traversal.md`. Frontmatter: `title`, `description`, `sidebar_position`. Add `orm/_category_.json` only if the folder lacks one.
- [ ] **Step 2:** No test; confirm the MDX has valid frontmatter and links resolve.
- [ ] **Step 3: Commit** (`docs(orm): relation traversal page`).

---

## Self-Review

**Spec coverage:** symmetric accessors (Tasks 3/5), to-one→object via `Relation: IntoFuture` (Tasks 1/2), to-many→QuerySet (Tasks 3/4), deep all-to-one → one JOIN (Task 2), crossing-to-many → leaf DISTINCT-by-PK + `.with_duplicates()` (Task 4), reverse-FK unchanged (Task 5), prelude (Task 6), behavioral 5-hop + per-kind tests (Task 7), doc page (Task 8). Phases 2–4 explicitly out of scope (no `__` WHERE filters, no relation-path owner scope, no REST embed here). Covered.

**Placeholder scan:** every task names exact files and gives real test code + concrete impl direction against named existing functions/line anchors. The two honest open decisions (Task 2 `RelTarget` unification point; Task 6 prelude-glob feasibility for user-crate traits) are called out with a fallback, not left as "TODO".

**Type consistency:** `HopSpec` / `HopKind` / `JunctionSpec` / `RelPath` (Task 1) are reused verbatim by Tasks 2–5; `to_one_hop` / `to_many_hop` / `M2M::query` / `with_duplicates` names are stable across the tasks that consume them. `Relation<T>` and the `<Model>Relations` trait naming is consistent Task 1→6.

**Risk to watch during execution:** Task 5 codegen compiles against every model — build-time and any accessor-name collisions surface there; Task 6 may reduce to "prelude covers built-in/plugin traits; user models `use` their own" if a blanket glob proves infeasible (adjust the Task 6 test to the honest surface rather than forcing it).
