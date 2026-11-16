# review_4 · umbral-core — slice 2: models / field-types / relations

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 19,102 LOC slice (`crates/umbral-core` — `src/orm` 23 files + `src/signals.rs` = 11,763; crate tests skimmed for this slice = 7,339; crate total = 130,814) |
| **Reviewer** | core slice 2 — models / field-types / relations (`orm/{model,validation,validators,cleaners,m2m,relation,reverse_accessor,reverse_set,one_to_one,foreign_key,nested,file_field,masked,multichoice,choices,gis,forms_runtime,exposure,materialized,audit,secrets,soft_delete_cascade}.rs`, `signals.rs`) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 6 medium · 4 low · 1 info — 13 new, 7 known |

## Summary

- This slice is the ORM's declarative surface: the `Model` trait and `FieldSpec`/`FkAction` contracts, the five pre-DB validators, the relation machinery (`ForeignKey`, `M2M`, reverse accessors, `one_to_one`), `Masked`/secrets redaction, soft-delete cascade, materialized fields, and the signal registry. Its behavioral test suite (7,339 LOC across ~50 integration files) is genuinely strong — real rows, public APIs, read-back object graphs — and the two historic incidents in this area (signal-registry mutex poisoning, `Masked` re-seal) are verifiably closed with regression tests.
- The highest-risk areas have the most defect mass: **soft-delete cascade silently skips self-referential trees** (MD-1), i.e. the one relationship shape the module's own doc says it exists to cover, and **`FileField`'s key guard never runs on the dynamic REST/admin write path its docstring names** (MD-2) — the same seam-drift class that `Masked` already had fixed (`gaps4 #2`).
- The signature drift lens pays off repeatedly here: FK-existence validation disagrees on soft-delete scope between its ambient and in-tx implementations (MD-3); SQLite's duplicate-PK code is missing from `classify_sql_error` so the same failure is a 500 there and a 400 on Postgres (MD-4); the `DatabaseRouter` is honored by every typed terminal and the dynamic free-fns (`gaps2 #88`) but bypassed by the M2M instance methods and deep relation chains (MD-6); `M2M::fetch` shows soft-deleted children that `M2M::query` hides (MD-7).
- One silent-swallow finding sits squarely under CLAUDE.md's "fix, don't patch" litmus: a transient DB error during a materialized-field recompute is mapped to "row gone" and the staleness is never logged (MD-8).
- Fix first: MD-1 (data-integrity, user-visible tree corruption-in-proxy), MD-2 (missing second guard on the path the docs promise it), then MD-3/MD-4 as the cheap drift corrections.

## Findings

### MD-1 · HIGH · correctness/data-integrity · self-verified
**Soft-delete cascade silently skips every self-referential `on_delete = "cascade"` tree — the parent table is pre-seeded into the `seen` set, so the whole subtree is left live.**

- **Where**: `crates/umbral-core/src/orm/soft_delete_cascade.rs:147` and `:169-176` (with `cascade_children` at `:84-96`); suite `crates/umbral-core/tests/soft_delete_cascade.rs` has no self-referential model.
- **Evidence**:
  ```rust
  // cascade_soft_delete, :147
  let mut seen = HashSet::from([parent.table.clone()]);
  // cascade_delete_level, :169-175
  for (child, fk) in cascade_children(&parent.table) {
      // A cascade child that isn't itself `soft_delete` has nowhere to record
      // the deletion. ... `check_cascade_targets` reports this at boot.
      if !child.soft_delete || !seen.insert(child.table.clone()) {
          continue;
      }
  ```
- **Impact**: Concrete trace — `Category { parent: ForeignKey<Category, on_delete = "cascade"> }` (the canonical tree), rows `C1(root) ← C2,C3 ← C4`. `Category::objects().filter(id.eq(C1)).delete()` → typed delete calls `cascade_soft_delete` (queryset/mod.rs:4244-4271) → `seen = {"category"}` → `cascade_children("category")` returns `("category", "parent_id")` → `seen.insert("category")` is already present → `continue` → `total = 0`, nothing cascaded. `C1` is stamped; `C2/C3/C4` keep `deleted_at IS NULL`. This reproduces exactly the failure the module header says it closes ("a soft-deleted parent used to leave its children behind as live rows… the children were invisible via the parent while still answering their own queries, still holding unique constraints", `:5-11`). The depth-limit warning at `:159-166` ("is there an FK cycle?") shows cycles were considered; the `seen` set makes *any* back-edge skip the subtree instead of using the depth limiter for schema cycles, and a self-FK is a normal (acyclic-in-data) tree.
- **Fix**: distinguish schema cycles from data trees — only skip a child table already on the current descent *path* (per-branch `seen`, or key it by `(table, depth)` so re-entry descends under the depth cap), or drop the table-level `seen` and rely on the `deleted_at IS NULL` child predicate + `MAX_CASCADE_DEPTH`. Add one self-referential tree case to `tests/soft_delete_cascade.rs`; the fix lives entirely in `soft_delete_cascade.rs`.

### MD-2 · HIGH · security/defense-in-depth · self-verified
**`validate_storage_key` runs only from the serde `Deserialize` impl; the dynamic write path (REST JSON / admin payloads) it names in its own docstring stores unvalidated keys — the same seam `Masked` already had sealed (gaps4 #2).**

- **Where**: `crates/umbral-core/src/orm/file_field.rs:48-60` (doc), `:61` (fn), `:219-231` (sole call site); repo-wide `grep -rn "validate_storage_key" crates/ plugins/ examples/` → definition + doc-link + line 227 only.
- **Evidence**:
  ```rust
  /// ... a client that controls the serialised form of a `FileField` / `ImageField`
  /// (a REST write, a deserialised admin payload) could otherwise point a
  /// record's key at an absolute path or a `..`-traversal string. ...
  // Deserialize impl, :225-228
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
      let key = String::deserialize(d)?;
      crate::orm::file_field::validate_storage_key(&key)
          .map_err(serde::de::Error::custom)?;
  ```
- **Impact**: The dynamic path (`DynQuerySet::insert_json` / `update` / admin writes) never constructs the typed `FileField` — it binds `Map<String, Value>` entries straight through (`write::json_to_sea_value`) and stores whatever string the client sent; a grep of `dynamic.rs` for `storage_key`/`file`/`image` finds no validation. So `POST {"photo": "../../etc/passwd"}` (or an absolute path on Windows-style input the guard rejects: backslash, control chars) is rejected when a handler deserializes into the typed model and silently accepted on the JSON path the guard's docstring explicitly promises to protect — a false security claim, plus a missing second layer. The operative guard is the storage layer (FS `ServeDir` + symlink guard; non-FS proxies stream by key — routed below); this finding does not claim an exploit on shipped backends, it claims the documented defense-in-depth layer is absent on the documented threat path. `Masked` is the exact precedent: its dynamic-path seal was added (`dynamic.rs:161-164`, `gaps4 #2`); `FileField` was left serde-only.
- **Fix**: call `validate_storage_key` at the dynamic write seam (the same place the `Masked` seal hook runs, in the values-to-bind step), or, if serde-only is the deliberate contract, delete the "a REST write, a deserialised admin payload" sentence — per CLAUDE.md the promise is the contract, so the guard is the fix. Lives in `file_field.rs` + the dynamic write path.

### MD-3 · MEDIUM · drift · self-verified
**The two FK-existence validators disagree on soft-delete scope: the ambient check is scoped to live rows (`DynQuerySet`), the in-tx check is not (raw COUNT) — so identical create bodies get opposite answers depending on which entry point runs them.**

- **Where**: `crates/umbral-core/src/orm/validation.rs:700-719` (`check_fk_row_exists`) vs `:726-780` (`check_fk_row_exists_in_tx`); callers `:51` (`validate_on_create` → `dynamic.rs:2323`), `:123` (`validate_on_typed_create` → `queryset/mod.rs:4895,5150`), `:91`/`:173` (in-tx variants).
- **Evidence**:
  ```rust
  // check_fk_row_exists, :710-713 — DynQuerySet adds `deleted_at IS NULL` (dynamic.rs:614-625)
  let count = crate::orm::dynamic::DynQuerySet::for_meta(target)
      .filter_eq_string(&pk.name, &pk_repr)
      .count()
  // check_fk_row_exists_in_tx, :747-750 — no such predicate
  let mut q = Query::select();
  q.from(crate::db::router::schema_qualified_table(&target.table))
      .expr(Func::count(Expr::col(Alias::new(&pk.name))))
      .and_where(Expr::col(Alias::new(&pk.name)).eq(pk_value));
  ```
- **Impact**: Soft-delete a `Category`; then `POST /posts {"category": <trashed id>}` — the ambient path returns `ForeignKeyNotFound` ("category: no matching record") although the row exists and the DB's FK constraint (and the in-tx sibling) accept it. Same body via `insert_json_in_tx` / the typed-with-tx path succeeds. The ambient check is stricter than the schema it guards, so valid references are rejected on the REST/admin surface and accepted elsewhere. (Note the tx variant cannot simply copy the DynQuerySet predicate without losing uncommitted-parent visibility — the fix must pick one contract.)
- **Fix**: one shared existence helper with an explicit `include_trashed` parameter (the DB's semantics — row exists — is the right default, matching Django), applied in both. Lives in `validation.rs`; call-site sites are unchanged.

### MD-4 · MEDIUM · drift/backend-parity · self-verified
**SQLite's duplicate-primary-key extended code (`1555`, `SQLITE_CONSTRAINT_PRIMARYKEY`) is missing from `classify_sql_error`, so a duplicate PK is a structured 400 on Postgres and an unclassified 500 on SQLite — for the PK shapes this tree now ships most (user-supplied String/Uuid keys).**

- **Where**: `crates/umbral-core/src/orm/validation.rs:206-256` (no `"1555"` arm); the doc at `:191-196` concedes unmatched codes fall to the 500 path.
- **Evidence**:
  ```rust
  // :194-196
  /// ... Returns `None`
  /// when the error doesn't match a known constraint code; the
  /// caller's `WriteError::Sqlx(e)` fallback handles that case
  /// (and surfaces as a 500 at the REST boundary).
  // Postgres arm, :244
  "23505" => { let value = pg_column.as_ref().and_then(|c| body.get(c)).cloned(); ...
  ```
  (SQLite arms present: `"787"`, `"2067"`, `"1299"`, `"275"`.)
- **Impact**: A slug-keyed model (`String` PK, user-supplied) double-POSTed with the same slug: Postgres returns a structured `UniqueViolation` (400, field named); SQLite reports extended code 1555 with the same `"UNIQUE constraint failed: t.slug"` message, hits `_ => None`, and the client gets a 500 with no field. SQLite is the test backend, so this divergence also means the suite passes on the backend where the error shape differs from the one production uses for the structured path.
- **Fix**: add `"1555"` sharing the `"2067"` body (the message format is identical — reuse `sqlite_columns_from_message`); ideally fold both codes into one arm. Lives in `validation.rs`.

### MD-5 · MEDIUM · correctness · self-verified
**`parse_pg_column_from_constraint` splits at the *first* underscore, so every Postgres auto-named constraint on a multi-word table (`blog_post_slug_key`) yields a garbled column and the REST error names a field that does not exist.**

- **Where**: `crates/umbral-core/src/orm/validation.rs:804-811`, fed at `:202-204`.
- **Evidence**:
  ```rust
  fn parse_pg_column_from_constraint(constraint: &str) -> Option<String> {
      for suffix in ["_fkey", "_key", "_check"] {
          if let Some(rest) = constraint.strip_suffix(suffix) {
              return rest.split_once('_').map(|(_, col)| col.to_string());
          }
      }
      None
  }
  ```
- **Impact**: PG auto-names an inline single-column UNIQUE `<table>_<col>_key` and an inline FK `<table>_<col>_fkey`. For table `blog_post` + column `slug`, the constraint is `blog_post_slug_key` → `rest = "blog_post_slug"` → `split_once('_')` → `("blog", "post_slug")` → the error carries `field: "post_slug"`, the body lookup misses, and the client is told a nonexistent field conflicts (the same for `blog_post_category_id_fkey` → `"post_category_id"`). Correct only by luck when the table name has no underscore. Multi-word table names are the norm (`snake_case`), so this is the common case, not the edge.
- **Fix**: derive the column from PG's `DETAIL` ("Key (slug)=(…) already exists") or match the constraint against the model's known column names (longest suffix); a `ModelMeta`-aware classifier is the robust shape. Lives in `validation.rs`.

### MD-6 · MEDIUM · drift/architecture · self-verified
**The `DatabaseRouter` is honored by every typed terminal, by `M2M::query()`, and by the dynamic junction free-fns (gaps2 #88) — but bypassed by the M2M instance methods (`add`/`remove`/`clear`/`set`) and by multi-hop `Relation` chains, which fall back to the ambient default pool.**

- **Where**: `crates/umbral-core/src/orm/m2m.rs:223` (`fetch`), `:265` (`add` → `execute_sql` `:892`), `:296` (`remove` → `execute_delete` `:1014`), `:324` (`clear` → `execute_delete_returning_ids` `:989`), `:358` (`set`); routed sibling `junction_pool_for_write/read` `:609-632`; `relation.rs:649-662` (`resolve_pool`).
- **Evidence**:
  ```rust
  // m2m.rs module doc, :42-43
  //! Every CRUD method routes through [`crate::db::pool_dispatched`]
  //! and uses sea-query — no per-backend SQL strings...
  // execute_delete, :1014-1015
  async fn execute_delete(q: &sea_query::DeleteStatement) -> Result<u64, sqlx::Error> {
      let pool = crate::db::pool_dispatched();
  // the routed sibling that exists for the free-fns, :609-614
  fn junction_pool_for_write(parent_model: Option<&str>) -> crate::db::DbPool {
      if let Some(name) = parent_model {
          if let Some(meta) = crate::migrate::model_meta_ref(name) {
              let ctx = crate::db::route_context::current();
              let alias = crate::db::router::router().db_for_write(meta, &ctx);
  ```
- **Impact**: In a `DatabaseRouter` app (`db_for_write` per model/tenant), `group.members.add(&child)` writes the junction in the *default* database while `Group::objects()` reads the routed one — the membership silently does not exist where the app looks, or the junction insert fails its FK against the unrouted pool. `M2M::query()` (same file, `:482-494`) routes correctly via `Manager::<T>`, so read and write of the same relation can hit different databases. `relation.rs::resolve_pool` (`:656-658`) does the same for chains >1 hop (`try_pool_dispatched` → ambient default; `.on()` is the documented workaround, and its docstring still frames the default as "the ambient default set at `App::build()`" — pre-router wording). gaps2 #88 shipped exactly this fix for `set_junction_dynamic`/`load_junction_selection` (test `tests/router_m2m_junction.rs` drives those free-fns only); the instance methods are its un-fixed sibling.
- **Fix**: have the M2M instance methods resolve through `junction_pool_for_write/read` — they need the parent model name, which the macro that wires `set_parent_id` already knows (carry it on the handle, or thread it through `M2M::set_parent_id`); for `relation.rs`, resolve the terminal model's pool via the router. The helpers exist; the call sites don't use them.

### MD-7 · MEDIUM · correctness/drift · self-verified
**`M2M::fetch` returns soft-deleted children while `M2M::query()` (same file, same relation) hides them — the raw join lacks the child-side `deleted_at IS NULL` predicate that every join/prefetch path and the soft-delete module doc promise.**

- **Where**: `crates/umbral-core/src/orm/m2m.rs:204-221` (raw join, no `deleted_at`) vs `:482-494` (`Manager::<T>` — soft-delete scoped via `queryset/mod.rs:4508-4513`); `grep -n "deleted_at" m2m.rs` → zero hits.
- **Evidence**:
  ```rust
  // fetch, :217-221
  .join_as(JoinType::InnerJoin, ..., Expr::col((Alias::new("j"), Alias::new("child_id")))
      .equals((Alias::new("c"), Alias::new(child_pk))))
  .and_where(Expr::col((Alias::new("j"), Alias::new("parent_id"))).eq(parent_id));
  // query, :494
  super::queryset::Manager::<T>::new().filter(predicate)
  ```
  The soft-delete module header states the contract: "reads *through* the parent hide them (the join and prefetch paths fold `AND child.deleted_at IS NULL` into their subqueries)" (`soft_delete_cascade.rs:8-9`).
- **Impact**: Soft-delete a child row; `group.members.fetch()` still lists it (counts include it, admin lists built from `fetch` show it) while `group.members.query().fetch()` correctly omits it — two answers from one relation object, and the visible one violates the module's documented invariant. Same drift family as MD-1/MD-3 (the soft-delete rule implemented per-call-site, missed at one).
- **Fix**: implement `fetch` via `self.query().fetch()` (one scoping implementation), or add the child-side `deleted_at IS NULL` predicate when `T::SOFT_DELETE`. Lives in `m2m.rs`.

### MD-8 · MEDIUM · correctness · self-verified
**A transient DB error during a materialized-field recompute is collapsed into "row gone" by `.unwrap_or_default()` — the target's field stays stale, silently, with no log.**

- **Where**: `crates/umbral-core/src/orm/materialized.rs:249-256`.
- **Evidence**:
  ```rust
  let rows = crate::orm::dynamic::DynQuerySet::for_meta(&source_meta)
      .filter_pk_eq(&id)
      .fetch_as_json()
      .await
      .unwrap_or_default();
  let Some(row) = rows.into_iter().next() else {
      continue; // row gone (e.g. deleted after the signal) — skip
  };
  ```
- **Impact**: Deciding scenario — a source row update fires `bulk_post_update`; the after-commit recompute re-fetches the source row and the query fails (pool timeout, SQLITE_BUSY under load, connection drop). `unwrap_or_default()` turns the `Err` into `[]`, the `let-else` treats it exactly like a legitimately deleted row, and the recompute loop moves on. The target row keeps its previous materialized value indefinitely; nothing recovers it until the next write to a source row, and nothing logs that this one was skipped. This is CLAUDE.md's litmus exactly ("an `.ok()` that silently discards a secondary error… the fix is to log the secondary error AND make the recovery path correct"). The sibling `continue` below it (key_fn → None) is fine and documented; only the swallowed `Err` is the bug.
- **Fix**: match the `Result` — on `Err`, `tracing::warn!` with the error + source pk (+ table/target) and skip (or surface to the recompute task's error path); never map `Err` to an empty row-set. Lives in `materialized.rs`.

### MD-9 · LOW · drift · self-verified
**`forms_runtime::validate_fk_exists` reports a DB failure as `"<field>: no matching record"` — the precise lie gaps2 #48 fixed in its multi-id sibling, here without even a log.**

- **Where**: `crates/umbral-core/src/orm/forms_runtime.rs:52-53` (doc) and `:77-85`; contrast `:193-253` (`validate_multi_fk_exists`, gaps2 #48).
- **Evidence**:
  ```rust
  // :77-85
  let exists = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
      .filter_eq_string(&pk_col, id)
      .count()
      .await
      .map(|n| n > 0)
      .unwrap_or(false);
  if !exists {
      errs.add(field, format!("{field}: no matching record"));
  }
  ```
  The doc at `:52-53` ("Registry / pool failures are swallowed as a miss") documents the swallow; the sibling's doc (`:199-207`) documents why that lie is unacceptable and its code logs + `add_non_field`s honestly.
- **Impact**: DB outage during form validation → every FK field shows "no matching record", blaming the user for an infrastructure failure and leaving no trace in the logs. Fails closed (the write is blocked), so the cost is diagnosability, not data. LOW because the only damage is the message — but it is the same defect #48 closed one function below.
- **Fix**: mirror `validate_multi_fk_exists`: `tracing::error` + `add_non_field("could not validate {field} (database error)")`, return as-is. Lives in `forms_runtime.rs`.

### MD-10 · LOW · docs/drift · self-verified
**`ModelMeta::public_fields` / `serializable_fields` ignore the canonical `is_secret_column` deny (so they would ship `password_hash` on a model that never annotates it) and the module doc claims consumers that do not exist.**

- **Where**: `crates/umbral-core/src/orm/exposure.rs:1-4` (doc), `:33-35` and `:40-44`; the real predicate `crates/umbral-core/src/orm/secrets.rs:59-61` used at `dynamic.rs:475`, `aggregate_path.rs:160`.
- **Evidence**:
  ```rust
  /// ... the derived views over `ModelMeta` that every
  /// serializer plugin (REST, GraphQL, admin, a future gRPC) reads ...
  pub fn public_fields(&self) -> impl Iterator<Item = &Column> {
      self.fields.iter().filter(|c| !c.secret && !c.private)
  }
  ```
  vs `secrets.rs:60`: `col.secret || is_hard_denied_field(&col.name)` — whose own doc says the name check exists precisely "for models that predate the annotation, or whose author never thought to add it" (a case an inspectdb-recovered model hits). `grep -rn "public_fields\|serializable_fields"` across `crates/` + `plugins/` → no consumers outside this module's own test.
- **Impact**: today, no serializer reads these predicates, so nothing leaks; the drift hazard is that the module bills itself as *the* exposure contract while omitting the one predicate every actual redaction site uses — the first plugin to adopt it inherits a hole (`password_hash` is `secret == false` until the derive/macro marks it). LOW: unreachable today, cheap to close.
- **Fix**: implement both predicates via `is_secret_column`, and either wire a real consumer or soften the module doc to "intended contract". Lives in `exposure.rs`.

### MD-11 · LOW · docs
**A cluster of docstrings in the model/relation layer state that shipped features are out of scope, and one error variant's name is a lie about its condition.**

- **Where**: `crates/umbral-core/src/orm/model.rs:195`, `:1150-1160`; `crates/umbral-core/src/orm/reverse_accessor.rs:106-110`.
- **Evidence**:
  ```rust
  // model.rs:195
  /// The primary-key type. M2 supports `i64` only; UUID lands later.
  // model.rs:1158-1160
  /// Out of scope at v1: non-`i64` FK targets, `ON DELETE` behaviours
  /// beyond the default RESTRICT, reverse accessors (`User::posts`),
  /// and many-to-many join tables. See `docs/specs/relationships.md`.
  // reverse_accessor.rs:106-108 — fires for any failed PK bind, not just non-i64
  ReverseError::NonI64Pk { parent } => write!(f,
      "umbral::orm::reverse: `{parent}` primary key could not be bound into the \
       reverse relation predicate"),
  ```
- **Impact**: Every claim is contradicted by the tree: i64/String/Uuid PKs work end-to-end (`pk_string_*`/`pk_uuid_*` suites), reverse accessors and M2M are shipped, `FkAction`'s own doc (`:1082-1087`) says the default is `NoAction` — not RESTRICT — while `:1150` still says FKs are "Stored as `i64`… Renders as `BIGINT REFERENCES`". `NonI64Pk` is still produced at `:172` (a `pk_as_json() == None` case) and `:182` (any `json_to_sea_value` failure) for String/Uuid parents that now work, so the name misdescribes its condition even though the message text is fine. A reader (or an LLM assistant) auditing capability from these docs under-reports the ORM's surface. gaps6 #21's stale-doc batch does not list `model.rs` (its scope: app.rs / realtime / signals-plugin / storage / tasks-plugin), so this stays numbered.
- **Fix**: refresh the four doc blocks to describe the shipped surface; rename `NonI64Pk` → e.g. `PkBindFailed`. Pure docs, `model.rs` + `reverse_accessor.rs`.

### MD-12 · LOW · correctness
**`MultiChoice`'s CSV storage has no escaping: a choice value containing `,` or differing from another only by surrounding whitespace cannot round-trip.**

- **Where**: `crates/umbral-core/src/orm/multichoice.rs:98-107` (`to_csv`), `:113-129` (`from_csv`).
- **Evidence**:
  ```rust
  // to_csv, :100-104 — raw join
  for (i, v) in self.values.iter().enumerate() {
      if i > 0 { out.push(','); }
      out.push_str(v.as_str());
  // from_csv, :118-119 — split + trim
  for part in s.split(',') {
      let part = part.trim();
  ```
- **Impact**: Unknown segments fail loudly (good — `Err(part)`), but when both `" a"` and `"a"` are registered choice values, storing `" a"` reads back as `"a"` — a silent wrong-value parse, not an error. Any choice containing a comma is unrepresentable. LOW today (choice values are short enum-ish tokens, and the loud-Err path covers the common mistake), but the quiet case is the dangerous one.
- **Fix**: reject at registration (the choice-definition check) any value containing `,` or with leading/trailing whitespace — cheap and keeps the storage format dumb; alternatively escape. Registration check lives with the choices validation.

### MD-13 · INFO · hygiene
**`cleaners::any_registered` fails open on a poisoned registry while every sibling `expect`s — on poison, all cleaners silently stop running.**
- **Where**: `crates/umbral-core/src/orm/cleaners.rs:106-108` vs `:87`, `:99`, `:123`.
  `registry().read().map(|r| !r.is_empty()).unwrap_or(false)` → a poisoned lock makes `apply` return `Ok(None)` (cleaner skipped, value written raw), whereas `register`/`clear_for_tests`/`apply`'s own body `expect("cleaner registry poisoned")`. Either is defensible; the mix is not — the signals registry's chosen posture is `into_inner()` recovery (`signals.rs` `lock_registry`). No action required; align the posture when the file is next touched.

## Strengths

- **`signals.rs` registry — the historic mutex-poisoning incident is genuinely closed, and the design is better than the minimum fix.** `lock_registry` recovers from poison (`into_inner`), sync handlers run under `catch_unwind` (`:310-313`), async handlers under `AssertUnwindSafe(...).catch_unwind()` *and* a documented per-subscriber `ASYNC_SUBSCRIBER_TIMEOUT` of 30s (`:55-61`, `:334-348`), the registry lock is dropped before any handler is awaited, and `emit` returns the total count. The regression test `panicking_sync_handler_does_not_brick_the_registry` asserts the contract directly ("both handlers are counted even though one panicked"). Transactional `emit_txn` deliberately does *not* catch panics and documents why.
- **`soft_delete_cascade.rs`'s core design (MD-1 aside) is exemplary**: subquery-driven (`child.fk IN (SELECT pk FROM parent …)`) so PK types never marshal and a million children cost one statement; one shared timestamp giving restore a precise identity ("a child that was *independently* trashed last week stays trashed"); children-first delete / deepest-first restore with the ordering constraint documented as the reason; a boot check for non-soft-delete cascade targets; explicit refusal to hard-delete a child rather than making the parent's soft delete irreversible.
- **`secrets.rs`'s hard-deny rationale is written down and load-bearing**: `HARD_DENIED_FIELDS` with the asymmetry spelled out ("the failure mode of matching too broadly … is survivable in a way that the failure mode of matching too narrowly … is not"), consumed by `is_secret_column` at every actual serializer redaction site (`dynamic.rs:475`, `aggregate_path.rs:160`), and `Masked`'s seal is enforced on serde, `Encode`, *and* the dynamic path with a dedicated no-reseal regression suite.
- **`validation.rs`'s structured-error ambition is real**: `classify_sql_error` lifts the offending value into the response, SQLite message parsing recovers the column (`sqlite_columns_from_message`), and the in-tx FK check exists specifically so a nested create can see an uncommitted parent — with the visibility rationale documented on both the function and its callers.
- **The test suite is behavioral, not decorative**: `soft_delete_cascade.rs` asserts live-row counts and independent-delete survival through the public ORM; `router_m2m_junction.rs` drives the free-fn entry and counts router hits; `masked_no_reseal.rs` exists as an explicit regression for the review_3 cluster; signal tests pin panic-during-emit semantics.

## Clean areas checked

- **`signals.rs` registry**: poison recovery, lock-not-held-across-await, sync/async panic isolation, timeout, regression test — read in full; sound (the one dropped candidate — "timeout counts as delivered" — is the tested, documented contract).
- **`audit.rs`**: savepoint-isolated `record_in_tx` (gaps6 #18 shipped), best-effort posture documented per-context rather than blanket-swallowed — sound.
- **`masked.rs` + `secrets.rs` trust boundaries**: seal on serde/`Encode`/dynamic path, key-parse errors typed, no re-seal on restore (`presealed`), denylist rationale — sound.
- **GIS gating**: postgis is feature-gated (compile) + boot-checked + per-backend arms with SQLite skip-with-warn per CLAUDE.md's allowed-exception rule — sound.
- **`nested.rs`**: two-pass validation (FK injection + hidden-field strip + depth/node caps) — no defect found beyond the shared validators reported above.
- **`choices.rs` + `forms_runtime::validate_choice_member`**: compile-time VALUES/LABELS pairing, nullable handling explicit — sound; the CSV storage gap is MD-12.
- **`reverse_set.rs` / `one_to_one.rs` / `foreign_key.rs` accessors**: unset-state returns empty/`None` (no silent widening — documented on `M2M::query`'s impossible predicate too); `ForeignKey::resolve`/`resolve_pg` take an explicit pool, so no router promise is broken — sound (naming nit is MD-11).
- **`validators.rs`**: the pure validators run identically from the dynamic and nested paths (pass-1); write-path wiring gaps are tracker items (#22), not new defects.
- **`relation.rs` single-hop paths**: `terminal_queryset()` → `Manager` (router-routed, soft-delete scoped) — sound; only the multi-hop pool fallback is MD-6.
- **`model.rs` trait machinery**: `set_m2m_parent_ids` default + macro-shadow design, `FkAction` boot-check posture (SetNull's lack of a boot check is honestly documented at `:1098-1101`) — sound beyond the stale docs in MD-11.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #22` — `FieldSpec::max_length` emits `varchar(N)`(validation.rs area) with no write-path length validation (open)
- `gaps6 #23` — cleaners' `apply` not run by the typed `update_values` path (open; queryset/mod.rs)
- `gaps6 #17` (residual) — soft-deleting a materialized source row fires only `bulk_post_delete`; that path doesn't recompute (open)
- `gaps6 #21` — stale code doc-comments batch; its scope list (app.rs / realtime / signals-plugin / storage / tasks-plugin) omits `model.rs`, so MD-11 stays numbered (open)
- `gaps2 #88` — M2M junction routing through `DatabaseRouter`, shipped for the free-fns only (archived/shipped) — the frame for MD-6
- `gaps2 #48` — `validate_multi_fk_exists` DB-error honesty, shipped (archived/shipped) — the frame for MD-9
- `review_3` file_field LOW (IDOR hardening) — "FIXED: FileField Deserialize rejects absolute/../control-char keys" (fixed) — MD-2 is the residual seam on the path that fix did not cover

## Out-of-scope notes

- `plugins/umbral-storage/src/lib.rs:1127-1175` (non-FS proxy `retrieve_stream(&key)`) and `plugins/umbral-storage/src/media.rs:233` (`path_for = dir.join(key)`) — verify traversal containment: this is MD-2's operative guard on non-FS backends (FS backends use ServeDir + the SymlinkGuard in `static_serve.rs`).
- `plugins/umbral-rest/*`, `plugins/umbral-admin/*` — whether any write path deserializes into the typed model (serde guard fires) or uses `DynQuerySet` JSON (guard absent): MD-2's reachability per surface.
- `crates/umbral-core/src/orm/queryset/mod.rs:4956-4960` — typed create flushes pending M2M post-commit, outside the atomic hook window (the code's own "#92 case" comment says so) — for the query/write slice reviewer.
- `crates/umbral-core/src/migrate.rs:1376-1405` — the deterministic `uniq_`/`idx_` naming contract and inline single-column UNIQUE (which PG auto-names `<table>_<col>_key`) that MD-5's parser assumes — for the migrations slice reviewer.
- `crates/umbral-core/src/orm/dynamic.rs:2323,2536` — the `insert_json` vs `insert_json_in_tx` entry points that select MD-3's divergent checks — query/write slice, for cross-checking.
