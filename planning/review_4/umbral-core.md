# review_4 · umbral-core

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 130,814 LOC (69,759 src + 61,055 tests) — all `.rs` under `crates/umbral-core`, excluding `target/` |
| **Reviewer** | core slices ×5, assembler-merged |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 6 high · 24 medium · 26 low · 4 info — 60 new, 22 known |

Size = all `.rs` under the crate (src + tests, excluding `target/`). N new = every numbered finding, INFO included; M known = entries in the Known section (never numbered).

## Summary

- `umbral-core` is the framework's centre: the ORM (the QuerySet/Manager builder→SQL→decode engine, models/field types/relations, dynamic rows), the migration engine plus inspectdb/backup/transferdata, the web layer (App builder, middleware, templates/forms/static/CORS/hosts), and the runtime and plugin contract (Plugin trait, ambient pool registry, settings, system-check catalogue, codegen). 130,814 LOC — 69,759 src plus a genuinely behavioral 61,055-LOC test suite.
- Overall state: the query/build half, the migration refusal surface, the web layer, and the runtime/plugin contract are healthy — every review_3/audit_2 security fix this crate owns was re-verified present. The defect mass is concentrated in typed write-terminal parity (create got the full lifecycle; save/upsert/bulk_update re-implement the same SQL without its guards), data-movement plumbing, and dev-time generators.
- Six HIGHs: CORE-1 (Postgres SUM/AVG decode assumes the column's declared type, but PG widens integer aggregates — every integer-column aggregate fails at runtime on PG, with no PG test), CORE-2 (save/upsert return rows with unseeded M2M handles, so documented M2M writes are silent no-ops), CORE-3 (soft-delete cascade skips self-referential trees — the exact shape its module doc says it exists to cover), CORE-4 (FileField's storage-key guard never runs on the dynamic write path its own docstring names), CORE-5 (transferdata's documented resume path is dead on a Postgres target), CORE-6 (flipping `db_constraint` produces no diff, so the physical FK is never dropped or added). (CORE-7 was downgraded to MEDIUM after the post-assembly cross-check — its REST-wire claim was refuted; see its coordinator note.)
- A recurring "one rule implemented N times, fixed in N−1 places" drift runs through the Mediums: the typed UPDATE path exists three times with three normalization rules (CORE-11), the read-only-VIEW + create-validation guards cover 5 of 11 write terminals (CORE-10), the DatabaseRouter is bypassed by the M2M instance methods (CORE-18) and by `Search::across` (CORE-14), the two FK-existence checks disagree on soft-delete scope (CORE-15), and the four transaction runners hand-copy one protocol (CORE-26).
- Test-suite gaps that hide exactly the above: no PG-gated aggregate test, no PG-target transfer *rerun* test, no `db_constraint`-flip or FK→BigInt diff test, no typegen test over a `#[sqlx(rename)]` field, no self-referential cascade model, and no slash-redirect integration test through `App::builder()`.
- Fix first: CORE-1…CORE-6, then the write-terminal parity batch (CORE-9…CORE-13), then the cheap drift corrections (CORE-15, CORE-16).

## Findings

Ordered by severity (CRITICAL → INFO). Every numbered finding is **new** — anything already tracked goes only in the Known section below, never numbered. Each carries: exact location, quoted evidence, a concrete impact scenario, a fix sketch.

### CORE-1 · HIGH · correctness · self-verified · spot-verified
**Postgres aggregate decoding assumes SUM/AVG return the source column's declared type, but Postgres widens integer aggregates — every `aggregate()`/`annotate_*` SUM or AVG over an integer column on PG fails at runtime with a `ColumnDecode` type mismatch.**
- **Where**: `crates/umbral-core/src/orm/queryset/backend_pg.rs:224-263` (decoder), `crates/umbral-core/src/orm/aggregate.rs:97-106` (SQL builder), `crates/umbral-core/src/orm/queryset/mod.rs:2985-3030` (`aggregate` terminal; same call pattern at `:3149,:3176` grouped-agg and `:3731,:3749` `fetch_annotated`).
- **Evidence**: the SQL is emitted with no cast —
  ```rust
  Aggregate::Sum(col) => Func::sum(Expr::col(Alias::new(col.as_str()))).into(),
  Aggregate::Avg(col) => Func::avg(Expr::col(Alias::new(col.as_str()))).into(),
  Aggregate::Max(col) => Func::max(Expr::col(Alias::new(col.as_str()))).into(),
  ```
  (`aggregate.rs:101-103`; sea-query 0.32.7 emits bare `SUM(...)`/`AVG(...)` — verified in the vendored source, `sea-query-0.32.7/src/backend/query_builder.rs:733-734`), and the PG decoder picks the Rust type from the model's declared `SqlType` —
  ```rust
  /// Postgres aggregate result decoder. … The only
  /// difference: Postgres preserves integer widths, so SUM/MAX/MIN on a
  /// SmallInt comes back as `i16` (not coerced to `i64` like SQLite).
  …
  AggregateKind::Sum | AggregateKind::Max | AggregateKind::Min => match source_ty {
      Some(SmallInt) => row.try_get::<Option<i16>, _>(name)?…
      Some(Integer)  => row.try_get::<Option<i32>, _>(name)?…
      Some(BigInt | ForeignKey) => row.try_get::<Option<i64>, _>(name)?…
  ```
  (`backend_pg.rs:224-251`), with `source_ty` read from the model metadata in the terminal: `meta.fields.iter().find(|f| f.name == c).map(|f| f.ty)` (`mod.rs:3003-3005`, `:3020-3022`).
- **Impact**: traced scenario — model `Post { view_count: i32 }`; `Post::objects().aggregate(&[("total", Aggregate::sum("view_count"))])` on Postgres emits `SELECT SUM("view_count") AS "total" FROM post`; PG's return type for `sum(integer)` is `bigint`; `sqlx`'s PG `try_get` compat check is exact type equality (INT4 ≠ INT8), so `try_get::<Option<i32>>` returns `Err(ColumnDecode { … "mismatched types" })` and the whole call fails. Same for `sum(int2)` (`int→bigint`), `sum(int8)` (`bigint→numeric`), `sum(FK)` (FK columns are int8-backed), `avg(any int)` (`→numeric`) and `annotate_sum`/`fetch_annotated` (identical decode path). `MAX`/`MIN` are correct (PG preserves the type) and SQLite is unaffected (its decoder coerces ints to `i64`). Because `avg(real/double)` really is `double precision`, the f64 AVG arm is right for float columns and wrong only for integer columns — a mix that makes the bug look intermittent. `crates/umbral-core/tests/aggregates.rs:15` is `use sqlx::SqlitePool;` only; no aggregate test anywhere drives `PgPool` (the crate's `tests/postgres_queryset.rs` has zero `Aggregate`/`annotate_sum` hits), so CI cannot see this.
- **Fix**: cast in `Aggregate::to_simple_expr` is the single-point fix — render `CAST(SUM(col) AS …)` (or `::float8`/`::numeric`) to a type the decoder agrees with — and simultaneously correct `backend_pg::decode_agg`'s doc (PG does not "preserve integer widths" for SUM; it widens) next to its SQLite twin. Then add a PG-gated aggregate test (the pattern `crates/umbral-core/tests/postgres_queryset.rs` already uses for other PG paths). Note `orm/search.rs:173` shows the codebase knows the pattern — it casts `::float8` on `ts_rank` precisely so the f64 decode is safe.

### CORE-2 · HIGH · correctness · self-verified · spot-verified
**Rows returned by `Manager::save()` (INSERT branch) and `Manager::upsert()` never get their M2M slots seeded, so `row.tags.add(...)`/`remove(...)`/`clear()` on them are documented silent no-ops — `Manager::create()` does this seeding and calls it load-bearing (BUG-16).**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:6097-6161` (`save` INSERT), `:5627-5683` (`upsert`); missing vs `:5015-5025` and `:5079-5081` (`create`, both backend arms), `:5996`/`:6005` (`create_in_tx`).
- **Evidence** — `create()`'s SQLite arm:
  ```rust
  // BUG-16 step 2: every materialised row, including the
  // post-INSERT readback, needs `parent_id` +
  // `junction_table` seeded on its M2M slots — otherwise
  // `row.tags.add(...)` is a silent no-op.
  row.set_m2m_parent_ids();
  // Carry form-staged M2M pending ids from the caller's
  // instance onto the readback row, then flush them to
  // junction rows now that parent_id + junction_table are
  // seeded on the readback row.
  instance.take_pending_m2m_into(&mut row);
  row.write_pending_m2m().await?;
  ```
  (`mod.rs:5015-5025`; the PG arm repeats the three calls at `:5079-5081`). `save()`'s INSERT branch does none of them:
  ```rust
  let stmt = build_insert_one_for::<T>(backend, &map).map_err(SaveError::Write)?;
  let row = match pool { … fetch_one … };
  // Fire post_save with the DB-populated row.
  crate::orm::audit::record( … ).await;
  crate::signals::emit_post_save::<T>(&row, true).await;
  Ok(row)
  ```
  (`mod.rs:6124,6141-6161` — no `set_m2m_parent_ids`/`take_pending_m2m_into`/`write_pending_m2m` anywhere in the method; `upsert` likewise returns the raw readback at `:5667-5683`). The M2M side then degrades silently by design:
  ```rust
  /// Insert one junction row linking this parent to `child`. … Returns `Ok(())`
  /// when the M2M slot is unattached.
  pub async fn add(&self, child: &T) -> Result<(), sqlx::Error> {
      let Some((parent_id, junction)) = self.junction_handle() else {
          return Ok(());
      };
  ```
  (`crates/umbral-core/src/orm/m2m.rs:240-252`; `junction_handle` is `Some((self.parent_id.clone()?, self.junction_table?))`, `m2m.rs:438-440`), and the trait doc names the exact gap this leaves: "Closes the second BUG-16 gap: without this, `m2m.add(&t)` silently writes a junction row with `parent_id = 0`" (`crates/umbral-core/src/orm/model.rs:66-68`).
- **Impact**: traced scenario — `let p = Post { title, ..Default::default() }; let p = Post::objects().save(p).await?; p.tags.add(&tag).await?;` returns `Ok(())` from both calls, but no `post_tags` row exists; the app's "tagged" state is lost with no error. Same for a row handed back by `upsert`, and for a REST/form path that staged `pending` M2M ids on an instance and persisted it through `save()` (the stage is dropped, not flushed — `take_pending_m2m_into` is never called). `save()` is advertised as the peer of `create()` ("Use `save` / `delete_instance` when per-row signal semantics are needed", `mod.rs:6089-6096`), so a caller switching between them for signal reasons silently loses M2M writes on one of them.
- **Fix**: call `row.set_m2m_parent_ids(); instance.take_pending_m2m_into(&mut row); row.write_pending_m2m().await?;` in `save`'s INSERT arm (both backends share the code) and the same seeding in `upsert`'s two arms — this needs the `T: HydrateRelated` bound the other terminals already carry. Add a save/upsert M2M round-trip test (create / `save` / `upsert`, then `add` + re-fetch, asserting the junction row).

### CORE-3 · HIGH · correctness/data-integrity · self-verified · spot-verified
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

### CORE-4 · HIGH · security/defense-in-depth · self-verified · spot-verified
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
- **Impact**: The dynamic path (`DynQuerySet::insert_json` / `update` / admin writes) never constructs the typed `FileField` — it binds `Map<String, Value>` entries straight through (`write::json_to_sea_value`) and stores whatever string the client sent; a grep of `dynamic.rs` for `storage_key`/`file`/`image` finds no validation. So `POST {"photo": "../../etc/passwd"}` (or an absolute path on Windows-style input the guard rejects: backslash, control chars) is rejected when a handler deserializes into the typed model and silently accepted on the JSON path the guard's docstring explicitly promises to protect — a false security claim, plus a missing second layer. The operative guard is the storage layer (FS `ServeDir` + symlink guard; non-FS proxies stream by key — see the Out-of-scope notes); this finding does not claim an exploit on shipped backends, it claims the documented defense-in-depth layer is absent on the documented threat path. `Masked` is the exact precedent: its dynamic-path seal was added (`dynamic.rs:161-164`, `gaps4 #2`); `FileField` was left serde-only.
- **Fix**: call `validate_storage_key` at the dynamic write seam (the same place the `Masked` seal hook runs, in the values-to-bind step), or, if serde-only is the deliberate contract, delete the "a REST write, a deserialised admin payload" sentence — per CLAUDE.md the promise is the contract, so the guard is the fix. Lives in `file_field.rs` + the dynamic write path.

### CORE-5 · HIGH · correctness · self-verified · spot-verified
**On a Postgres target, `transferdata`'s resume path is dead: the state table declares `done INTEGER`, `read_state` decodes it as `i64`, and sqlx decodes PG `INT4` into `i64` strictly — so the second run fails before copying anything.**

- **Where**: `crates/umbral-core/src/transfer.rs:898-923` (table + read), called at `transfer.rs:783-784`.
- **Evidence**:
  ```rust
  // transfer.rs:898-902 — done is INTEGER on the Postgres arm too
  async fn ensure_state_table(target: &DbPool) -> Result<(), TransferError> {
      let sql = format!(
          "CREATE TABLE IF NOT EXISTS {STATE_TABLE} \
           (table_name TEXT PRIMARY KEY, last_pk TEXT, done INTEGER NOT NULL DEFAULT 0)"
      );
  ```
  ```rust
  // transfer.rs:918-922 — decoded as i64 on both arms
  let sql = format!("SELECT table_name, last_pk, done FROM {STATE_TABLE}");
  ...
  let rows: Vec<(String, Option<String>, i64)> = match target {
      DbPool::Sqlite(p) => sqlx::query_as(&sql).fetch_all(p).await?,
      DbPool::Postgres(p) => sqlx::query_as(&sql).fetch_all(p).await?,
  };
  ```
- **Impact**: `transferdata --target postgres://…` run 1 completes and writes state rows (`upsert_state_in_tx`, `transfer.rs:929-966`). Run 2 — the documented resume/rerun path — calls `ensure_state_table` (no-op via `IF NOT EXISTS`) then `read_state`, and decodes the row's `INT4` `done` as `i64`. sqlx 0.8.6 has no int4 widening: in the pinned `sqlx-postgres` source `Type<Postgres> for i64` resolves to `INT8` only and `PgTypeInfo::type_compatible` is strict equality; the tuple `FromRow` decodes via `try_get`, which raises `ColumnDecode` on the mismatch. So the resume aborts before visiting any table. First run is fine (empty state → zero rows to decode), which is why this was not caught: the only rerun test is SQLite→SQLite (`tests/transfer.rs:760`, `transfer_is_idempotent_on_rerun`), and the two Postgres-target tests take the first-run path. The identical decode shape exists at `transfer.rs:520` (`read_id_pg`, `row.try_get::<i64, _>(idx)?`) — secondary, since it needs an int4 key column on a PG source; same fix applies.
- **Fix**: decode the flag as `i32` (a 0/1 value always fits; also works against already-created state tables — `read_state`'s tuple becomes `(String, Option<String>, i32)`); alternatively declare `done BIGINT` **plus** an `ALTER COLUMN` for existing state tables. For `read_id_pg`, decode `IdKind::Int` via the column's type info instead of hard-coding `i64`.
- **Status**: new. Self-verified: re-read `ensure_state_table`/`read_state`/the call site and both callers' paths; checked the pinned sqlx-postgres `Type<i64>` impl and `type_compatible` (strict equality, no int4 widening); traced run 1 → state row → run 2 decode failure.

### CORE-6 · HIGH · correctness/drift · self-verified · spot-verified
**Flipping `db_constraint` on an FK column changes nothing in the diff — the AlterColumn trigger list and `fk_changed` both ignore it — so the physical FK is never dropped (true→false) or added (false→true), and the model silently disagrees with the live schema in both directions.**

- **Where**: `crates/umbral-core/src/migrate.rs:5022-5031` (trigger list) and `migrate.rs:6218-6238` (the FK block and its comment).
- **Evidence**:
  ```rust
  // migrate.rs:5022-5031 — no db_constraint term (prev/curr both carry it)
  if type_changed
      || length_changed
      || prev_col.nullable != curr_col.nullable
      || prev_col.fk_target != curr_col.fk_target
      || prev_col.unique != curr_col.unique
      || prev_col.default != curr_col.default
      || prev_col.choices != curr_col.choices
      || prev_col.choice_labels != curr_col.choice_labels
      || prev_col.on_delete != curr_col.on_delete
      || prev_col.on_update != curr_col.on_update
  {
  ```
  ```rust
  // migrate.rs:6226-6237 — the comment claims the flip tears down the constraint; nothing implements that
  let fk_changed = prev_col.fk_target != new.fk_target
      || prev_col.on_delete != new.on_delete
      || prev_col.on_update != new.on_update;
  if fk_changed && matches!(new.ty, SqlType::ForeignKey) {
      ...
      // gaps2 #22: only re-add the physical constraint when the FK
      // still wants one. A `db_constraint = false` FK keeps the
      // DROP (so flipping the flag tears down any prior constraint)
      // but emits no ADD CONSTRAINT.
  ```
- **Impact**: two concrete directions. (1) `Post { author: ForeignKey<User> }` is deployed with the physical constraint; the user adds `#[umbral(db_constraint = false)]` — the documented cross-DB opt-out (`documentation/docs/v0.0.1/orm/database-routing.mdx:114-120`: "keeps the FK logical … the migration emits no `REFERENCES`") — and `makemigrations` reports **no changes**: `diff_columns`' trigger list has no `db_constraint` term, so no `AlterColumn` is emitted, and the `REFERENCES` survives on **both** backends. The documented opt-out never takes effect on an existing DB. (2) Reverse: a `db_constraint = false` FK is deployed, then flipped to `true` → still no diff → the DB keeps enforcing nothing while a fresh DB built from the same models has the constraint; writes that should be rejected pass in prod and diverge from a fresh dev DB. The DROP only fires today when another FK marker (`fk_target`/`on_delete`/`on_update`) changes in the same migration — accidentally. Note that `Column::db_constraint` **is** carried in the snapshot (`orm/model.rs:668`), so the values the comparison should use already exist; only the comparison is missing.
- **Fix**: add `|| prev_col.db_constraint != curr_col.db_constraint` to the trigger list (5022-5031) **and** to `fk_changed` (6226-6228). Both directions then render correctly with the existing gate: true→false = DROP, no re-add (the `new.db_constraint` guard on the ADD already exists, 6235-6241); false→true = `DROP … IF EXISTS` (no-op) + ADD; SQLite's dance rebuilds from `new_columns`, reflecting the new flag. Also correct `planning/archive/gaps2-done.md:553` and the MDX page once fixed.
- **Status**: new. Self-verified: grepped every `db_constraint` read in `migrate.rs` (495/510, 1080-1087, 1491-1516, 5256-5275, 6235/6239, 6426-6431, 6624) — none compares prev vs curr; re-read `diff_columns` and `render_alter_column_postgres`; traced both flip directions to rendered SQL.

### CORE-7 · MEDIUM · docs / codegen contract · self-verified · spot-verified → downgraded (see note)
**`typegen` writes the SQL column name as the TypeScript property while its module doc describes the wire as "the model struct serialised by serde" with field names as declared — a claim that is false for `#[sqlx(rename)]` columns, where the real dynamic-REST wire is column-keyed (matching the emitted key) and typed serde serialization uses the field name.**

> **Coordinator note (post-assembly cross-check, 2026-10-10).** Originally filed HIGH on the claim that the emitted keys "don't exist on the wire"; that claim is refuted. The dynamic JSON terminals key every row by `col.name` (`dynamic.rs` readbacks at `:1936-2622`, `:4553`), and the umbral-openapi review independently traced the REST wire (responses, filters, ordering, `?fields=`, write bodies) as column-keyed — so the emitted `author_id` **does** match the REST wire. What remains is the false doc claim above and the two-surface key divergence it hides (dynamic REST = column names; typed serde = field names). Severity: MEDIUM.
- **Where**: `crates/umbral-core/src/typegen.rs:266` (emission), `crates/umbral-core/src/typegen.rs:16-19` (the claim it breaks), `crates/umbral-macros/src/lib.rs:1804-1808`, `crates/umbral-macros/src/lib.rs:2411`, `crates/umbral-core/src/orm/model.rs:565-568`, `crates/umbral-core/src/migrate.rs:1504-1507`
- **Evidence**:
  ```rust
  // typegen.rs:266 — the property name comes from `Column.name` …
  let _ = writeln!(out, "  {}: {ty};", column.name);

  // typegen.rs:16-19 — … while the module doc promises the serde wire shape:
  //! The JSON umbral-rest puts on the wire, which is the model struct serialised
  //! by serde. Concretely:
  //!
  //! - Field names stay exactly as declared. No camelCase rewrite: the wire is
  //!   snake_case, and a generated type that quietly renames fields is a lie.

  // umbral-macros/src/lib.rs:1805-1808 — `#[sqlx(rename)]` makes the two diverge:
  // remaps it (which `FromRow` honours, so the migration + column const
  // must match). `field_name_str` stays the Rust-facing identity (const
  // name, serde/relation keys); `column_name_str` is the SQL identity.
  let column_name_str = sqlx_rename(&field.attrs).unwrap_or_else(|| field_name_str.clone());

  // umbral-macros/src/lib.rs:2411 — that SQL identity is what FieldSpec carries:
  name: #column_name_str,
  ```
  The chain: `FieldSpec.name` is the column name (its own doc, `model.rs:565-567`, is stale — see the Out-of-scope notes) → `Column::from(&FieldSpec)` copies it verbatim (`migrate.rs:1505` `name: f.name.to_string()`) → `ModelMeta.fields` → typegen's `push_field(…, column: &Column, …)` writes `column.name`. `FieldSpec` carries **no** Rust-field-name member, and the `Model` derive emits no `#[serde(rename)]` for models (the 5 `serde(rename` sites in the macro are the Choices and Dto derives only), so serde serialises the Rust field name while typegen emits the column name.
- **Impact** (coordinator-reframed): for a dynamic-REST frontend the emitted `author_id: number` matches the wire (`"author_id": 5`) — no failure. The trap is real only on the typed-serde surface: a custom handler returning `Json<Post>` emits `"author": 5` while the shared `.ts` types say `author_id`, and the module doc's "model struct serialised by serde" wording tells readers the opposite of what the emitter does. Renamed-column shapes come from `documentation/docs/v0.0.1/orm/column-types.mdx:192-211` and `inspectdb` for keyword-colliding columns (`inspect.rs:1816`, e.g. `#[sqlx(rename = "type")] pub type_: String`). `crates/umbral-core/tests/sqlx_rename_column.rs` pins the metadata divergence, and no typegen test (`tests/typegen.rs`, `typegen_registry.rs`, `dto_typegen.rs`) covers a renamed field — so whichever wire contract is intended stays unpinned.
- **Fix**: correct the module doc (`typegen.rs:16-26`) to state the real contract — REST wire keys are SQL column names; typed serde differs for renamed columns — and add a typegen test over a `#[sqlx(rename)]` model pinning the emitted key to the column name. Adopting serde/field-name keys on the REST wire instead is a REST-layer design change (carry a wire name in `FieldSpec`/`Column`), not a typegen fix.

### CORE-8 · MEDIUM · correctness (data loss) · self-verified
**`build_insert_many_for` fixes the INSERT column set from row 0 and never performs the assertion its doc promises, so a heterogeneous `bulk_create` batch silently drops later rows' values (or their explicit PKs), or binds a literal `0`/NULL where the DB should auto-assign.**
- **Where**: `crates/umbral-core/src/orm/queryset/write_helpers.rs:202-205` (doc), `:227-246` (column selection), `:253-287` (per-row bind); callers `mod.rs:5158` (`bulk_create`), `:5937` (`bulk_create_pg`), `:6027` (`bulk_create_in_tx`).
- **Evidence**:
  ```rust
  /// Build a multi-row INSERT. Reuses the per-row column-selection logic
  /// from `build_insert_one_for` for the first map, then asserts every
  /// subsequent map exposes the same column set (heterogeneous row shapes
  /// would change the column list mid-INSERT, which SQL forbids).
  pub(super) fn build_insert_many_for<T: Model>(…)
  ```
  — no assertion exists anywhere in the body. The column set is frozen from `let first = &maps[0];` and
  ```rust
  if field.primary_key && is_default_pk(field.ty, &val) { return false; }
  if val.is_null() && field.nullable && !first.contains_key(field.name) { return false; }
  ```
  (`write_helpers.rs:230-246`), then every later row is bound through the same `included_fields`, with
  ```rust
  let val = map.get(field.name).cloned().unwrap_or(serde_json::Value::Null);
  ```
  (`write_helpers.rs:264-267`).
- **Impact**: two concrete shapes, both silent. (a) `Post::objects().bulk_create(vec![existing_with_id_5, new_default_pk])` where `existing_with_id_5` is first: the PK is **excluded** because row 0's PK is non-default — row 2's explicit `id = 5` is dropped and the DB auto-assigns, i.e. a caller-requested PK is silently replaced (imports/restores produce wrong ids). Reversed order (`new_default_pk` first) the PK column **is** included and the later row binds `id = 0` explicitly. (b) A model using `#[serde(skip_serializing_if = "Option::is_none")]`/partial `<Model>New` shapes: row 0 omits a nullable column (column excluded), a later row that *sets* it has its value silently dropped — the DB writes its default. Both paths pass validation (`bulk_create` validates each map at `mod.rs:5150`) and report success.
- **Fix**: either implement the documented assertion — walk every map and return `WriteError` naming the row index and column when `included_fields`' presence pattern differs from row 0's — or union the column set across all maps and bind NULL for absent fields (per-row `auto_now` handling at `write_helpers.rs:261-263` already shows the interleaving is workable). The assertion is the safer first step; the union is what Django/SeaORM-style bulk inserts do.

### CORE-9 · MEDIUM · security/correctness (audit + signal accounting) · self-verified
**`#[umbral(audited)]` writes made through `Manager::save`'s UPDATE branch, `delete_instance`, and `upsert` record no audit row, and `delete_instance` fires per-row delete signals when the write matched zero rows.**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:6305-6315` (`save` UPDATE tail), `:6345-6435` (`delete_instance`), `:5667-5683` (`upsert`); contrasted with `:3906-3908` (`delete`, gated on `!ids.is_empty()`), `:4054`, `:4176`, `:4288` (update/delete audits), `:6142` (save INSERT audit), `:5033`/`:5089`/`:5251` (create/bulk audits).
- **Evidence**: the audited contract (`crates/umbral-core/src/orm/audit.rs:3`, `:29-30`) — "Every write to an audited model records a row in `umbral_audit` … Models without `#[umbral(audited)]` pay nothing" — and the user page's own example list ("produces **no row at all** for a write from REST, a task, or `objects().save()`", `documentation/docs/v0.0.1/orm/audit-trail.mdx:54`, pointing audited users at this feature). The UPDATE arm of `save` ends:
  ```rust
  // Fire post_save with created=false.
  crate::signals::emit_post_save::<T>(&row, false).await;
  if want_post { … emit_post_update::<T>(prev, &row).await; }
  Ok(row)
  ```
  (`mod.rs:6305-6315`) — no `audit_pre`/`audit_post`/`audit::record`. `delete_instance` ends `crate::signals::emit_post_delete::<T>(instance).await; Ok(affected)` (`:6431-6434`) with no audit call, whereas its set-based twin audits and gates:
  ```rust
  let count = ids.len() as u64;
  if !ids.is_empty() {
      Self::audit_post(audit_before, &ids, crate::orm::audit::DELETE).await;
  ```
  (`mod.rs:3906-3908`) — while `delete_instance` fires `emit_pre_delete` before the write (`:6361`, before `affected` is known) and `emit_post_delete` unconditionally (`:6432`), e.g. the second soft-delete of a row matches 0 thanks to the idempotency guard `deleted_at IS NULL` (`:6386`).
- **Impact**: an app declares `#[umbral(audited)] Invoice`; `Invoice::objects().save(invoice)` on an existing row (the canonical per-row-signal update path) commits the change and leaves the audit table with no record of the actor/field diff — the same update through `update_values` or the admin/REST dynamic path IS recorded, so the trail is quietly incomplete exactly where the app used the typed API. `delete_instance` likewise deletes with no audit row while `QuerySet::delete` on the same model audits. The no-op case additionally emits `post_delete` for a row that was already deleted, double-counting for per-row subscribers (`RealtimePlugin`, `media_invalidate_on`, materialized-field handlers) while the set-based path stays silent because it gates on matched ids.
- **Fix**: in `save`'s UPDATE branch, take `audit_pre` before the UPDATE and `audit_post(audit_before, &[pk_val], UPDATE)` after (the helpers at `mod.rs:711-759` are already backend-generic and flag-gated); in `delete_instance`, audit DELETE when `affected > 0` and gate the post signasl on the same condition; do the same in `upsert` (which currently fires neither signal nor audit — if it is meant to be a low-level primitive, it needs `create_pg`'s escape-hatch doc, `mod.rs:5690-5699`, which it lacks). Extend `crates/umbral-core/tests/audit_trail.rs` (today `the_typed_path_is_audited` covers only `create` + `update_values`, `tests/audit_trail.rs:100-148`).

### CORE-10 · MEDIUM · drift · self-verified
**The read-only-VIEW refusal and the create-time validation are enforced on `create`/`bulk_create`/`update_values`/`update_expr`/`delete` but skipped by their sibling typed terminals `save`, `upsert`, `bulk_update`, `delete_instance`, `create_in_tx`, `bulk_create_in_tx`.**
- **Where**: guards present at `crates/umbral-core/src/orm/queryset/mod.rs:3783` (delete), `:3983` (update_expr), `:4084` (update_values), `:4874` (create), `:5133` (bulk_create); absent from `:5627` (upsert), `:5736` (bulk_update), `:5976` (create_in_tx), `:6014` (bulk_create_in_tx), `:6097` (save), `:6345` (delete_instance). Validation: `crate::orm::validation::validate_on_typed_create` is called only at `:4895` (create) and `:5150` (bulk_create).
- **Evidence** — the guard and its stated purpose:
  ```rust
  // features #73: a view is read-only. Refuse before any SQL is built, so the
  // error names the model instead of surfacing as the driver's opaque
  // "cannot modify a view".
  if T::VIEW.is_some() {
      return Err(crate::orm::write::WriteError::ReadOnlyView { table: T::TABLE.to_string() });
  }
  ```
  (`mod.rs:4082-4086`); `WriteError::ReadOnlyView`'s own doc says the DB "would reject this too, but only after the ORM had built and sent the statement" (`crates/umbral-core/src/orm/write.rs:132-138`). `save()` — the method whose doc block immediately precedes it and is titled "Manager::save" — contains no `T::VIEW` check and no `validate_on_typed_create` call anywhere between its signature (`:6097`) and either return (`:6161`, `:6315`).
- **Impact**: a model declared `#[umbral(view = "…")]` is refused with the structured `ReadOnlyView` through `create`/`bulk_create`/`update_values`/`delete`, but `Model::objects().save(instance)` and `delete_instance` reach the database. On SQLite that yields a driver-level "cannot modify view"/"cannot delete from view" with no model name; on Postgres a simple view is **auto-updatable**, so the INSERT/UPDATE/DELETE *succeeds* through the view and mutates the base table — a write the same model refuses on its sibling terminal, i.e. a read-only model that is silently writable depending on which method the app called. Separately, `save()`'s INSERT skips `validate_on_typed_create` (no `choices`/required/FK check), so an out-of-choices value or dangling FK reaches the DB and surfaces as a raw driver error instead of the structured `WriteError` that `create` returns for the identical payload (and `SaveError` can already carry a `WriteError`, so the plumbing exists).
- **Fix**: hoist the `T::VIEW` check + `validate_on_typed_create` into a shared pre-write step that `save`, `upsert`, `bulk_update`, `delete_instance`, and the two `_in_tx` writers call the same way the five guarded terminals do.

### CORE-11 · MEDIUM · drift · self-verified
**The typed UPDATE path exists three times and the three copies disagree on every update-side semantic: `save`'s inline loop skips `normalize_json`/cleaners, and `bulk_update` skips those plus `auto_now`/`auto_now_add`/`auto_user`/`auto_user_add`.**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:4304-4346` (`build_update_for`, used by `update_values`/`update_expr`), `:6218-6275` (`save` UPDATE), `:5775-5807` (`bulk_update`); insert-side seams for contrast: `crates/umbral-core/src/orm/queryset/write_helpers.rs:164-176` (single) and `:273-278` (many).
- **Evidence**: `build_update_for` normalizes —
  ```rust
  // features #83: an UPDATE must normalize too — otherwise a field
  // arrives clean on create and dirty on edit.
  let normalized = crate::orm::write::normalize_json(field.trim, field.lowercase, val);
  ```
  (`mod.rs:4340-4343`) and separately refreshes `auto_now` (`:4352-4356`) and `auto_user` (`:4362-4366`) after the caller's columns. `save`'s loop handles the auto stamps (`:6231-6261`) but binds the raw map value —
  ```rust
  let val = map.get(field.name).cloned().unwrap_or(serde_json::Value::Null);
  let sea_val = crate::orm::write::json_to_sea_value(field.ty, &val, field.nullable, field.name, fk_pk_hint(field))…
  ```
  (`mod.rs:6262-6274`) — with no `normalize_json` and no `cleaners::apply`. `bulk_update` builds one `CaseStatement` per non-PK column from `T::FIELDS` with no filtering at all —
  ```rust
  let update_cols: Vec<&crate::orm::FieldSpec> = T::FIELDS.iter().filter(|f| !f.primary_key).collect();
  ```
  (`mod.rs:5775-5776`) — and assigns `map.get(field.name)…` for each (`:5791-5802`), so `auto_now_add`/`auto_user_add` columns are overwritten with the struct's carried value (the exact "epoch-sentinel bug" that `save`'s comment at `:6224-6230` says gap68 closes) and `auto_now`/`auto_user` are never refreshed.
- **Impact**: a field declared `#[umbral(trim, lowercase)]` arrives trimmed through `create`/`bulk_create`/`update_values` and untrimmed through `objects().save(row)` / `bulk_update(rows)` — the same "clean on create, dirty on edit" failure `features #83` fixed for the other writers. An `auto_now` `updated_at` stops advancing for rows touched by `bulk_update` (the field's declared contract is refresh-on-every-update, and the dynamic `update_json` path does refresh it), and an `auto_user` audit column silently keeps the wrong actor. (`cleaners::apply` on `update_values` is already tracked as gaps6 #23; the two sites here are additional members of that hole, and the `normalize_json` gap is not tracked anywhere.)
- **Fix**: make `build_update_for` the only UPDATE-SET builder — give it an optional `instance_map` variant (or a `Vec<(FieldSpec, Value)>`) that `save` and `bulk_update` call per row, so normalization, cleaners, auto-stamps, and the PK skip (already implemented at `:4320-4322`) live in one place; `bulk_update` can then serialize each instance and reuse the same field walk.

### CORE-12 · MEDIUM · drift (correctness) · self-verified
**`QuerySetTx`'s read terminals silently ignore `select_related`, `prefetch_related`, `join_related` and `.only()`, while the identical chain outside a transaction hydrates, validates, and errors — and the tx terminals carry no doc caveat.**
- **Where**: `crates/umbral-core/src/orm/queryset/tx.rs:53-87` (`QuerySetTx::fetch`, the base of `first`/`get`/`exists`); contrast `crates/umbral-core/src/orm/queryset/mod.rs:2062-2130` (generic `fetch`) and the `on_tx` doc at `:1126-1147`.
- **Evidence**: the tx terminal builds and executes the base query, then stops at M2M seeding:
  ```rust
  let q = self.qs.build_query_for(self.tx.backend_name());
  …
  // BUG-16 step 2: wire each row's PK into its M2M slots so
  // junction-table accessors used inside the transaction see
  // the right parent.
  for r in &mut rows { r.set_m2m_parent_ids(); }
  Ok(rows)
  ```
  (`tx.rs:63-84`). The generic path does much more for the same builder chain: it rejects `.only()` (`mod.rs:2071-2073`), validates `join_related` field names (`:2084`, "a typo or an M2M field name doesn't silently no-op"), applies joins (`:2110`), and hydrates both `select_related` and the joined relations (`:2120-2124`). `on_tx`'s doc lists the terminals but states no relation caveat (`mod.rs:1126-1132`).
- **Impact**: `Post::objects().select_related("author").on_tx(tx).fetch()` inside `db::transaction(…)` returns rows whose `author.resolved()` is None — no error, no query, while the same expression outside the transaction loads it (batch-IN) — so code moved into a transaction for atomicity silently loses its eager loads and typically N+1s or renders blanks. `.join_related(...)` is dropped entirely (not even the JOIN), `.only(&["id"])` returns full rows instead of erroring like its twin, and a to-many `join_related` chain that would change the row shape outside a tx is a no-op inside it. Annotated queries are the one survivor (`build_query_for` applies annotations), which makes the divergence harder to attribute.
- **Fix**: either implement relation hydration in `QuerySetTx::fetch` by delegating to a shared hydrate step, or make the tx terminal refuse the unsupported builders loudly (reuse `guard_pg_terminal_unsupported`'s pattern at `mod.rs:1639-1667`, which exists precisely to stop "silently returning un-hydrated rows") and document the restriction on `on_tx`.

### CORE-13 · MEDIUM · perf/alloc · self-verified
**Every typed `create`/`save`-INSERT serializes the row twice (plus a full `Map` clone) to build audit arguments whose first act would be checking the `audited` flag — so unaudited models pay two JSON serializations per write, contradicting `audit.rs`'s "Models without `#[umbral(audited)]` pay nothing".**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:6142-6158` (`save` INSERT), duplicated at `:5033-5050` (create, SQLite arm) and `:5089-5106` (create, PG arm); the false claim at `crates/umbral-core/src/orm/audit.rs:29-30`.
- **Evidence**:
  ```rust
  crate::orm::audit::record(
      &crate::migrate::ModelMeta::for_::<T>(),
      &serde_json::to_value(&row).ok().and_then(|v| { … pk_of(…, o) }).unwrap_or_default(),
      crate::orm::audit::CREATE,
      None,
      serde_json::to_value(&row).ok().and_then(|v| v.as_object().cloned()).as_ref(),
  )
  ```
  — both `to_value(&row)` calls evaluate at the call site, before `record()` runs its first line (`audit::record`'s doc: "A no-op unless the model is `audited`", `audit.rs:121`; the gate is `if !meta.audited { return; }`). The third argument also clones the whole row map for the after-image. The `audit_pre`/`audit_post` wrappers do it right — `if !meta.audited { return Vec::new() }` before any read (`mod.rs:711-717`), which is why `bulk_create` (using `audit_post` at `:5251`) does not pay.
- **Impact**: on the hottest write path (`objects().create()` / `save()` in a loop — the non-bulk insert path), every model — audited or not — pays two `serde_json::to_value` passes over the full row plus one `Map` clone and an extra `ModelMeta::for_::<T>()` lookup per pass, before any INSERT-time audit decision. For a 30-column model at ~1k writes/s that is ~2k avoidable full-row serializations/s on the DB-write critical path, and the three hand-copied blocks are themselves a drift hazard (they have already diverged from the `audit_post` shape used by `bulk_create`).
- **Fix**: match the wrapper pattern — gate on `ModelMeta::for_::<T>().audited` at the call site (or pass the row to a small `audit_record_row(&meta, &row, CREATE, before)` helper that serializes once, computes the pk, and reuses that single `Value` for the after-image), and replace all three copies with that helper.

### CORE-14 · MEDIUM · correctness (multi-database) · self-verified
**`Search::across` reads through `crate::db::pool_dispatched()` — always the "default" alias — bypassing the `DatabaseRouter` every ORM read terminal uses, so search silently reads the wrong database on any app with a read replica or per-model aliases.**
- **Where**: `crates/umbral-core/src/orm/search.rs:260-286`; the routed counterpart `crates/umbral-core/src/orm/queryset/mod.rs:1683-1700` (`resolve_pool`).
- **Evidence**:
  ```rust
  match crate::db::pool_dispatched() {
      DbPool::Postgres(pool) => { … }
      DbPool::Sqlite(pool) => { … }
  }
  ```
  (`search.rs:260-286`), where `pool_dispatched` is hard-wired to one alias:
  ```rust
  pub fn pool_dispatched() -> &'static DbPool {
      POOLS.get().expect("umbral: db pool not initialised — did you call App::build()?")
          .get("default").expect("umbral: no default database registered")
  }
  ```
  (`crates/umbral-core/src/db.rs:208-214`), while every ORM read goes through `resolve_pool` → `router().db_for_read(meta, &ctx)` → `pool_for_dispatched(alias)` (`mod.rs:1683-1693`). This is the identical defect review_3 already fixed for `backup.rs`: "dump() and load() hardcode pool_dispatched() (default pool) … bypassing the per-model alias routing" — `planning/review_3/claude/02_triage.md:59`, marked FIXED ("dump/load route per-model via resolved alias pool"); `search.rs` was the copy that was missed, and `crates/umbral-core/tests/search_across.rs` is SQLite-single-pool only, so nothing covers the aliased case.
- **Impact**: an app with the documented read-replica router (`examples/read-replica`) sends all `Search::across` traffic to the primary while every `fetch()` on the same models goes to the replica — the replica exists precisely to absorb this read load. On a multi-alias/tenant app (`Model::DATABASE` / `Plugin::database()`), search runs `SELECT … FROM "post"` against the **default** pool: either "no such table" or, worse, the same-named table from another tenant's/default database — cross-database data leaking into search results with no error.
- **Fix**: resolve the pool per searched model through the same router used by reads — `SearchSources::branches` already knows the models, so `Search::across` needs a `resolve_pool::<T>(None, RouteOp::Read)` per branch or a small `search_pool::<T>()` helper — and refuse (or group by alias) when the tuple's models resolve to different aliases, since the current design fuses all branches into one `UNION ALL`.

### CORE-15 · MEDIUM · drift · self-verified
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

### CORE-16 · MEDIUM · drift/backend-parity · self-verified
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

### CORE-17 · MEDIUM · correctness · self-verified
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

### CORE-18 · MEDIUM · drift/architecture · self-verified
**The `DatabaseRouter` is honored by every typed terminal, by `M2M::query()`, and by the dynamic junction free-fns (gaps2 #88) — but bypassed by the M2M instance methods (`add`/`remove`/`clear`/`set`) and by multi-hop `Relation` chains, which fall back to the ambient default pool.**

- **Where**: `crates/umbral-core/src/orm/m2m.rs:223` (`fetch`), `:265` (`add` → `execute_sql` `:892`), `:296` (`remove` → `execute_delete` `:1014`), `:324` (`clear` → `execute_delete_returning_ids` `:989`), `:358` (`set`); routed sibling `junction_pool_for_write/read` `:609-632`; `relation.rs:649-662` (`resolve_pool`). Additional direct-`pool_dispatched()` sites in the same file: `:523`, `:581`, `:617`, `:631`, `:893`, `:993`, `:1015`; the registry-less fallbacks are documented at `:607`, `:657`, `:804`.
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

### CORE-19 · MEDIUM · correctness/drift · self-verified
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
- **Impact**: Soft-delete a child row; `group.members.fetch()` still lists it (counts include it, admin lists built from `fetch` show it) while `group.members.query().fetch()` correctly omits it — two answers from one relation object, and the visible one violates the module's documented invariant. Same drift family as CORE-3/CORE-15 (the soft-delete rule implemented per-call-site, missed at one).
- **Fix**: implement `fetch` via `self.query().fetch()` (one scoping implementation), or add the child-side `deleted_at IS NULL` predicate when `T::SOFT_DELETE`. Lives in `m2m.rs`.

### CORE-20 · MEDIUM · correctness · self-verified
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

### CORE-21 · MEDIUM · correctness/drift · self-verified
**A column that stops being an FK (`ForeignKey<T>` → `i64`) keeps its FK on Postgres: the render emits only a type rewrite, never a `DROP CONSTRAINT`, and Postgres does not lose dependent constraints through `ALTER COLUMN … TYPE` — while SQLite's rebuild drops it. The comment saying the type branch "handles it indirectly" is false.**

- **Where**: `crates/umbral-core/src/migrate.rs:6222-6228` (claim + gate), `migrate.rs:6138-6148` (the only statement produced).
- **Evidence**:
  ```rust
  // migrate.rs:6222-6228
  // Only emitted when the new column is still a
  // FK; if the column stopped being a FK (ty changed away
  // from ForeignKey), the type-change branch above handles
  // it indirectly via the column type rewrite.
  let fk_changed = prev_col.fk_target != new.fk_target
      || prev_col.on_delete != new.on_delete
      || prev_col.on_update != new.on_update;
  if fk_changed && matches!(new.ty, SqlType::ForeignKey) {
  ```
  Here `fk_changed` **is** true (`fk_target` Some→None) but `matches!(new.ty, SqlType::ForeignKey)` is false, so the `DROP CONSTRAINT IF EXISTS` inside the block (6230-6233) never runs; the only statement the Postgres renderer produces for this column is the type branch's `ALTER TABLE … ALTER COLUMN … TYPE {new_ty} USING …`.
- **Impact**: realistic model edit — drop the wrapper, keep the id: `author: ForeignKey<User>` → `author: i64` (the macro names the column identically; `tests/foreign_keys.rs:39/196`). After `migrate`: on Postgres the emitted SQL contains no `DROP CONSTRAINT`, and PG drops-and-recreates dependent constraints as part of `ALTER COLUMN … TYPE` (postgres commit `20bef2c3110af295501919bac463b87ac58876de`; if the retype is a no-op the constraints are not touched at all) — **either way the FK remains enforced**. On SQLite the same operation is a table rebuild (`render_alter_column_dance_sqlite`, 6015-6095) from `new_columns`, where the column no longer carries a `fk_target`, so `build_column_def_sqlite` emits no `REFERENCES` — the constraint is gone. Result: `ON DELETE` behaviour (cascade/set-null) fires on prod and not on the test backend, or vice versa; the stale constraint is also invisible to every future diff, because the new snapshot records `BigInt` — only hand-written SQL can remove it.
- **Fix**: key the FK teardown on the **old** column: when `prev_col.fk_target.is_some() && new.fk_target.is_none()`, emit `ALTER TABLE {q_table} DROP CONSTRAINT IF EXISTS "{table}_{column}_fkey"` before the type rewrite (the name convention and IF EXISTS form already exist at 6229-6233).
- **Status**: new. Self-verified: re-read `render_alter_column_postgres` end to end and confirmed no other branch drops the constraint; confirmed `is_safe_cast` allows `(ForeignKey, BigInt)` (4813) so the alter **is** generated; confirmed the SQLite dance rebuilds from the new shape; checked the Postgres behavior of dependent constraints against the upstream commit.

### CORE-22 · MEDIUM · correctness/codegen · self-verified
**`resolve_struct_names`' collision fallback is a no-op on every non-Django framework: it "reverts" the name to `pascal_case_from_table(&t.table)` — which IS the colliding name — so two legal tables that pascal-case alike generate two identical `pub struct` definitions in one non-compiling file.**

- **Where**: `crates/umbral-core/src/inspect.rs:1216-1223` (fallback; the insert is at 1221).
- **Evidence**:
  ```rust
  // inspect.rs:1210-1212 — the non-Django name is already pascal_case_from_table
  } else {
      t.name.clone()
  };
  ```
  ```rust
  // inspect.rs:1216-1223
  // Collision fallback: revert every table whose stripped name is shared to
  // its full pascal name (the external AuthUser is exempt — it's canonical).
  for t in &schema.tables {
      let name = &struct_names[&t.table];
      if counts[name] > 1 && !(django && t.table == DJANGO_USER_TABLE) {
          struct_names.insert(t.table.clone(), pascal_case_from_table(&t.table));
      }
  }
  ```
  and both introspectors set `name: pascal_case_from_table(&table)` (`inspect.rs:405`, `:449`), so on the non-Django path `t.name == pascal_case_from_table(&t.table)` by construction — the fallback re-inserts exactly the value that collided.
- **Impact**: Postgres permits `"blog_post"`, `"BLOG_POST"` and `"blog-post"` side by side (quoted identifiers); all three pascal-case to `BlogPost` (later segments lowercased). `umbral inspectdb` → `counts["BlogPost"] > 1` → the fallback writes `"BlogPost"` again for each → generated `models.rs` contains duplicate `pub struct BlogPost` definitions → `cargo build` fails with E0428. One saving grace already in the tree: because `to_snake_case("BlogPost") != "BLOG_POST"` the `#[umbral(table = …)]` attribute **is** emitted for the odd-cased table (`inspect.rs:1724-1725`), so the table mapping would be right — only the struct name collides. The Django path is unaffected (its names are app-prefix-stripped `django_struct_name(...)` first, so the fallback differs).
- **Fix**: make the fallback actually disambiguate — deterministic discriminator (schema-qualified table segment, or a numeric suffix for later tables) or refuse with both table names; add a test (see CORE-23).
- **Status**: new. Self-verified: read `resolve_struct_names` whole plus both introspector call sites (405/449); traced the two-table scenario; refuted the alternative that counts are framework-scoped (non-Django names get no prefix).

### CORE-23 · MEDIUM · correctness/codegen · self-verified
**inspectdb emits struct names unvalidated and the table attribute unescaped: a leading-digit/empty/keyword table name yields a non-identifier `pub struct`, and a `"` or `\` in a table name breaks or silently mangles the generated `#[umbral(table = "…")]` line.**

- **Where**: `crates/umbral-core/src/inspect.rs:1724-1735` (emission); the guards at `inspect.rs:1847` (`is_rust_keyword`) and `1948-1949` (`safe_field_ident`) reach fields only.
- **Evidence**:
  ```rust
  // inspect.rs:1724-1725
  if to_snake_case(&this_struct) != table.table {
      out.push_str(&format!("#[umbral(table = \"{}\")]\n", table.table));
  ```
  ```rust
  // inspect.rs:1735
  out.push_str(&format!("pub struct {this_struct} {{\n"));
  ```
  ```rust
  // inspect.rs:1948-1949 — the keyword guard is field-only
  fn safe_field_ident(name: &str) -> String {
      if is_rust_keyword(name) {
  ```
- **Impact**: table `2fa_tokens` (legal; realistic) → `pascal_case_from_table` leaves the leading digit → `pub struct 2faTokens {` — does not parse. `___` → `""` → `pub struct  {`. `self` → `"Self"` — the keyword list even contains `"self"`/`"Self"` (`inspect.rs:1847`), but it never sees struct names. Separately, the attribute payload is raw: a table named `weird"name` (creatable with doubled quotes) emits `#[umbral(table = "weird"name")]` — does not parse; a table containing `\` compiles into a Rust escape and **silently names a different table**. Reachable only via `inspectdb` against an existing DB (dev-time codegen), and the common cases are loud at `cargo build` — hence MEDIUM (per the calibration rule: dev-time codegen emitting invalid output), with the `\` case as the silent one. Cross-ref: the casing-crate half of this is CASING-1 (`planning/review_4/umbral-casing.md`, `crates/umbral-casing/src/lib.rs:89-112`); this finding is the inspect.rs emission/validation half.
- **Fix**: validate struct names where `struct_names` is built — the tree owns the rule (`crates/umbral-core/src/codegen.rs:184 validate_ident`: rejects empty / leading-digit / keyword) — prefix/suffix or refuse; and escape the attribute payload (`\` → `\\`, `"` → `\"`) or refuse names that don't round-trip. Extend `inspectdb_generates_compilable_models_for_edge_cases` (`tests/inspect.rs:1181`) with a `2fa_tokens`-style table, a quote/backslash table, and the CORE-22 collision pair — the current test only covers a keyword column and a non-`id` PK.
- **Status**: new. Self-verified: read `render_one_struct` and `resolve_struct_names` whole; confirmed no validator runs between the introspectors and line 1735; confirmed the keyword list content and its field-only wiring.

### CORE-24 · MEDIUM · correctness · self-verified
**A newly added pk-flagged column IS routed to `AddColumn` (the gap-97 guard exempts `primary_key`), contradicting the render doc's claim that "the autodetector won't route a pk-flagged column through `AddColumn`"; the SQLite render then emits `ADD COLUMN … PRIMARY KEY … AUTOINCREMENT`, which SQLite rejects.**

- **Where**: `crates/umbral-core/src/migrate.rs:5218-5240` (guard + push), `migrate.rs:5418-5423` (the claim), render at `migrate.rs:5592-5596` + `6452-6457`.
- **Evidence**:
  ```rust
  // migrate.rs:5218-5223 — the pk exemption
  if !col.nullable
      && col.default.is_empty()
      && !col.auto_now_add
      && !col.auto_now
      && !col.primary_key
  {
  ```
  ```rust
  // migrate.rs:5237-5240 — pk columns reach the AddColumn push
  ops.push(Operation::AddColumn {
      table: current.table.clone(),
      column: (*col).clone(),
  });
  ```
  ```rust
  // migrate.rs:5418-5420 — the doc block
  /// `AddColumn` ignores the `primary_key` flag: neither SQLite nor
  /// Postgres lets a primary key be added to an existing table without
  /// a table-recreation step, and the autodetector won't route a
  ```
- **Impact**: reachable with a legal edit — rename the pk field **and** change its shape so the single-pair rename heuristic (`column_shape_matches`, 5133-5154) refuses to pair: `id: i64` (pk) → `code: String` (pk) yields dropped=`[id]`, added=`[code]`, and `code` is a pk-flagged new column. The `diff_columns` guard was deliberately given `!col.primary_key` (it exists to refuse NOT NULL adds), so the migration is written; at apply time the SQLite render keeps the pk flag (`col_no_unique` clears only `unique`, 5592-5594) and `build_column_def_sqlite` emits `PRIMARY KEY` + `AUTOINCREMENT` for an int pk (6452-6457) → `ALTER TABLE … ADD COLUMN "code" … PRIMARY KEY … AUTOINCREMENT` → SQLite's "Cannot add a PRIMARY KEY column" / AUTOINCREMENT-not-allowed errors. On Postgres the same statement fails on any populated table (a NOT NULL pk added to existing rows). Both are loud at apply and roll back (the apply loop runs a file's statements in one transaction), so not HIGH — but the doc block at 5418-5423 is wrong on **both** halves (routing happens; the render does not ignore the flag), and the one refusal that exists for this class of add (gap 97) skips exactly this case.
- **Fix**: refuse at diff time with `UnsafeAlter` (same shape as gap 97: "adding a primary-key column needs a hand-written migration / table rebuild"), or route pk-flagged adds through the SQLite rebuild dance; correct the doc block either way.
- **Status**: new. Self-verified: read the `added` loop and both add paths; confirmed the pk flag survives into `build_column_def_sqlite`; confirmed no other gate sits between `diff_columns` and the `ops.push`.

### CORE-25 · MEDIUM · correctness/arch · self-verified

**Two models that claim the same `TABLE` name are accepted silently at boot: alias/meta lookups are last-wins `HashMap`s and the promised `model.table.unique` system check does not exist.**

- **Where**: `crates/umbral-core/src/app.rs:1276-1281` and `:1284-1289` (the two `insert` sites), `crates/umbral-core/src/app.rs:1354-1355` (`meta_by_table`); check catalogue `crates/umbral-core/src/check.rs:8-15, 190-197`; model registry `crates/umbral-core/src/migrate.rs:64-72`
- **Evidence**:
  ```rust
  // app.rs:1276-1281 (per-plugin loop; the app-model loop at 1284-1289 is the same shape)
              let alias = model
                  .database
                  .clone()
                  .or_else(|| plugin_default.map(|s| s.to_string()))
                  .unwrap_or_else(|| "default".to_string());
              table_alias.insert(model.table.clone(), alias);
  ```
  ```rust
  // app.rs:1354-1355
  let meta_by_table: HashMap<&str, &ModelMeta> =
      all_models.iter().map(|m| (m.table.as_str(), m)).collect();
  ```
  ```rust
  // check.rs:11-13 — the promised check, still "landing later" (M5/M7 came and went):
  //! isn't left at the insecure dev default). More checks (`field.
  //! backend`, `model.pk.present`, `model.table.unique`, `route.
  //! collision`, `plugin.dependency.*`) land alongside the registries
  ```
  ```rust
  // check.rs:194-197 — first entries of the real catalogue; it contains
  // model.soft_delete_cascade / model.auto_user / model.materialized_view /
  // settings.* / backend.* / field.* / plugin.security_missing and NO
  // `model.table.unique`, `model.pk.present`, or `route.collision`.
  pub fn framework_checks() -> Vec<SystemCheck> {
      vec![
  ```
  ```rust
  // migrate.rs:65-70 — the registry flatten: no uniqueness rejection
  let mut flat: Vec<(String, ModelMeta)> = Vec::new();
  ...
          for m in per_plugin.get(&plugin).cloned().unwrap_or_default() {
              flat.push((plugin.clone(), m));
          }
  ```
  (Catalogue entry specified in `docs/specs/05-backends-and-system-check.md:164`: "`model.table.unique` | Two models don't claim the same `TABLE` name.")
- **Impact**: A plugin (or the app) declares a model whose `TABLE` collides with another model's — e.g. a third-party plugin shipping a `session` table alongside `umbral-sessions`. Nothing at boot reports it: `table_alias` keeps the last model seen, so on a multi-DB app the *first* model's queries can be routed to the *second* model's database alias (wrong-database reads via a silently-wrong `Alias`); `meta_by_table` binds FK resolution and the dynamic/admin path to one of the two models arbitrarily; and `init_plugins` hands the migration engine both models for one physical table, deferring the symptom to a confusing migrate-time DDL error. The failure is silent exactly where it is detectable (boot) and loud where it is not (runtime query / migration) — the posture CLAUDE.md's "backend mismatches caught at boot, not in prod" rule exists to prevent.
- **Fix**: Implement `model.table.unique` in `check.rs::framework_checks()` walking `umbral::migrate::models()` (the registry now exists), and/or make the two `app.rs` insert sites reject a duplicate instead of overwriting. Once the check guarantees uniqueness the maps can legitimately `expect` it. The spec catalogue (`docs/specs/05-backends-and-system-check.md:164`) should move the entry from "planned" to shipped in the same change. The same catalogue is also missing the other ids it names — `model.pk.present`, `route.collision`, `plugin.dependency.*` (`check.rs:190-192`) — which share this fix.

### CORE-26 · MEDIUM · drift · self-verified
**The commit→fire-signals transaction protocol is hand-copied in four runners; the bodies are identical except the `begin*` call, and the gaps6 #15 rationale comment already survives in only two of four.**
- **Where**: `crates/umbral-core/src/db.rs:1058-1082` (`transaction`), `1098-1121` (`transaction_on`), `1129-1147` (`transaction_sqlite`), `1157-1175` (`transaction_pg`)
- **Evidence**:
  ```rust
  // db.rs:1064-1069 (and byte-for-byte equivalents at 1104-1109, 1135-1140, 1163-1168)
  match f(&mut tx).await {
      Ok(val) => {
          let pending = tx.take_pending_signals();
          tx.commit().await.map_err(E::from)?;
          for sig in pending {
              sig().await;
          }
          Ok(val)
      }
  // db.rs:1076-1078 vs db.rs:1143-1146 — same behavior, drifting commentary:
          // Best-effort rollback — if it fails we surface the original error.
          // The buffered signals are never drained here, so they're simply
          // dropped with `tx` — a rolled-back write fires nothing (gaps6 #15).
          let _ = tx.rollback().await;          // sqlite/pg variants: bare, no comment
  ```
- **Impact**: The post-commit signal-firing contract (signals buffered during the transaction fire only after a successful commit; a rollback fires nothing) is the safety rule that keeps `create`/`delete` signals from lying. It is implemented four times; a future change applied to `transaction` only (e.g. firing a failure signal on commit error, or draining pending signals before rollback for logging) silently leaves `transaction_on`/`transaction_sqlite`/`transaction_pg` divergent — the tree's signature drift mode. The comment loss is the canary: two copies already lost their rationale.
- **Fix**: extract one generic helper (`async fn run_in_tx(tx: Transaction, f) -> …`) that owns the take-commit-fire / rollback-drop protocol; the four public fns become `begin*().await? → run_in_tx(tx, f)` one-liners. Signals fire once, in one place.

### CORE-27 · MEDIUM · resource lifecycle
**`db::close()` iterates only build-time `POOLS`; runtime tenant pools registered via `register_tenant_pool` are never closed on graceful shutdown, while the caller logs "draining DB pools".**
- **Where**: `crates/umbral-core/src/db.rs:773-782` (`close`), `db.rs:141-150` (`DYNAMIC_POOLS`, `Box::leak`'d `&'static`), consumer `crates/umbral-core/src/app.rs:214-215`
- **Evidence**:
  ```rust
  // db.rs:773-782 — closes POOLS only; DYNAMIC_POOLS is never read here
  pub async fn close() {
      if let Some(pools) = POOLS.get() {
          for db in pools.values() {
              match db {
                  DbPool::Sqlite(p) => p.close().await,
                  DbPool::Postgres(p) => p.close().await,
              }
          }
      }
  }

  // app.rs:214-215
  tracing::info!("umbral: server stopped accepting; draining DB pools");
  crate::db::close().await;
  ```
  `close`'s own doc sets the contract it breaks: "For SQLite this also lets WAL checkpoint; for Postgres it sends a clean `Terminate` so the server doesn't log the connections as unexpectedly lost." (`db.rs:760-762`). The live producer is `plugins/umbral-tenants/src/lib.rs:538` (`register_tenant_pool` from `TenantRouter`), and the pools are `Box::leak`ed (`db.rs:275`) so this is their only close opportunity.
- **Impact**: A multi-tenant app that onboarded N tenant databases stops cleanly and its log says pools were drained, but N pools of up to `db_max_connections` connections each stay open: Postgres logs every one as an unexpected disconnect, and per-tenant SQLite files never get their WAL checkpointed by the shutdown path — on a busy tenant DB that means a slower next boot and an un-checkpointed WAL left on disk. Bounded (process exit follows), but the promise in the log/doc is false for exactly the apps that need the path most.
- **Fix**: in `close()`, after closing `POOLS`, read `DYNAMIC_POOLS` and close each entry (the leaked `&'static DbPool` derefs fine for `.close().await`); take the read guard, collect the clones, drop the guard, then await closes.

### CORE-28 · MEDIUM · memory / drift
**`timezone.rs`'s wash-once warning set is unbounded and grows one `String` per distinct unknown tz name — while the public `tz_or_utc` is documented for "per-user tz overrides surfaced from the session", i.e. caller-supplied strings.**
- **Where**: `crates/umbral-core/src/timezone.rs:38-45`, doc claim at `timezone.rs:61-63`, contrast `crates/umbral-core/src/ratelimit.rs:66-70`
- **Evidence**:
  ```rust
  // timezone.rs:38-45
  fn should_warn_unknown_tz(name: &str) -> bool {
      static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
      let mut seen = WARNED
          .get_or_init(|| Mutex::new(HashSet::new()))
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
      seen.insert(name.to_string())
  }

  // timezone.rs:61-63 — the documented consumer is a per-user value:
  /// Parse an IANA name, falling back to UTC with a one-shot warning
  /// on failure. Public for callers that need the raw lookup (e.g.
  /// per-user tz overrides surfaced from the session).

  // ratelimit.rs:66-70 — the sibling primitive got the fix for this exact class:
  /// the map without bound (audit_2 core-web #4). Every `SWEEP_EVERY` checks the
  /// limiter runs [`RateLimiter::sweep_at`] over the WHOLE map, dropping every
  ```
  Nothing ever removes an entry; the set is keyed on the arbitrary `name` argument. In-tree reachability is narrow — today the only caller is `active_tz()` feeding `settings.time_zone` (one configured string) — but the function is re-exported (`crates/umbral/src/lib.rs:316`) and its doc invites the per-user use, so the trap is loaded for the documented third-party consumer: a session/profile tz field lets a user insert unbounded distinct strings (`X1/Y`, `X2/Y`, …), each persisting for the process lifetime, and every entry also permanently suppresses the warning for that name.
- **Impact**: An app taking the docstring's advice grows a process-global set monotonically with attacker-influenced input, with no eviction and no cap — the same bounded-key-map problem `ratelimit` closed with `SWEEP_EVERY`, left open here. In-tree today: memory only from the single settings value (negligible).
- **Fix**: cap the set (e.g. warn-always after 100 entries, or a fixed-size ring), or key the dedupe on a hash with a bounded `VecDeque`; at minimum, reword the docstring so the per-user pattern ships with the bound.

### CORE-29 · MEDIUM · codegen / drift · self-verified
**`ensure_dependency` misses the dotted workspace spelling `name.workspace = true`, so it appends a second key and leaves a Cargo.toml cargo refuses to parse — the exact "broken project" failure the function's own docstring exists to prevent.**
- **Where**: `crates/umbral-core/src/codegen.rs:352-397` (check at `365-366`, `383`; doc at `340-351`)
- **Evidence**:
  ```rust
  // codegen.rs:365-366
  let key = format!("{name} =");
  let table_header = format!("[dependencies.{name}]");
  ...
  // codegen.rs:383
  if in_dependencies && trimmed.starts_with(&key) {
      return Ok(false);
  }
  ```
  Against `umbral-rest.workspace = true` the `starts_with("umbral-rest =")` test fails (character 11 is `.`, not ` `), as does `trimmed == "[dependencies.umbral-rest]"`; the function then inserts `umbral-rest = "{spec}"` (`codegen.rs:396-397`). The doc at `codegen.rs:344-350` names this exact hazard class for the table form ("Form-blind: … we'd append a second `umbral-rest = "..."` key and cargo would refuse to parse the manifest at all") and closes with "Writing the file and leaving the crate unable to build it is a generator that produced a broken project." Tests exist for the plain and table forms (`codegen.rs:549`, `:720`, `:748`) — none for the dotted/space-less spellings.
- **Impact**: Run a generator (`umbral-rest`'s class generators, `startcommand`) against a plugin whose manifest uses Cargo's inherited-dependency spelling (`dep.workspace = true`, increasingly the norm in a workspace like this tree) and the next `cargo build` fails loudly with a duplicate/conflicting `umbral-rest` key. Loud, not silent — dev-time only — hence MEDIUM per the rubric's codegen calibration.
- **Fix**: match the prefix `format!("{name}.")` (and `format!("{name}=")`) inside `[dependencies]` as "already present", mirroring the table-form branch; add a test with `umbral-rest.workspace = true`. Longer term, a per-crate `[dependencies]`-section parser instead of two prefix probes.

### CORE-30 · MEDIUM · ergonomics · self-verified
**A typo'd flag on a valid plugin command is reported as "unknown command", because `CommandSet::dispatch` maps clap's `UnknownArgument` (raised under a *matched* subcommand) to `Unmatched`, discarding clap's precise diagnostic.**
- **Where**: `crates/umbral-core/src/cli.rs:296-306`; consumer `crates/umbral-cli/src/lib.rs:531-545` + `unknown_token` at `:740-745`
- **Evidence**:
  ```rust
  // cli.rs:300-306
  return match e.kind() {
      clap::error::ErrorKind::DisplayHelp
      | clap::error::ErrorKind::DisplayVersion => {
          Ok(DispatchOutcome::Help(e.render().to_string()))
      }
      clap::error::ErrorKind::InvalidSubcommand
      | clap::error::ErrorKind::UnknownArgument => Ok(DispatchOutcome::Unmatched),
      _ => Err(Box::new(e)),
  };

  // umbral-cli/src/lib.rs:740-745 — the later "unknown command" path then names the first non-flag token:
  fn unknown_token(argv: &[std::ffi::OsString]) -> Option<String> {
      argv.iter()
          .skip(1)
          .find(|a| !a.to_string_lossy().starts_with('-'))
          .map(|a| a.to_string_lossy().into_owned())
  }
  ```
  `UnknownArgument` is returned by clap when a **matching** subcommand receives an unrecognized flag; mapping it to `Unmatched` throws away the one diagnostic that says what's wrong. The argv then falls through to the built-in parser, which reports `InvalidSubcommand` for the plugin's name, and `unknown_token` returns that name — so the CLI prints `error: unknown command \`tasks-worker\`` for a command that exists and is registered.
- **Impact**: First contact with any plugin command goes wrong for a trivial typo: `umbral tasks-worker --intervall 5` answers "unknown command `tasks-worker`" plus the full catalogue, sending the user to look for a missing plugin instead of a misspelled flag. Same shape for built-in commands via the umbral-cli half (`umbral migrate --dry-rn` — `unknown_token` returns `migrate`). The unified-catalog intent (gaps2 #54) is right; this is its collateral.
- **Fix**: in `cli.rs`, before mapping `UnknownArgument`, check whether the first non-flag argv token names one of `self.entries`; if it does, return `Err(Box::new(e))` so clap's "unexpected argument" message renders (the consumer's `Err(e) => return Err(e)` already propagates). Keep `Unmatched` for `InvalidSubcommand` / top-level unknowns. The umbral-cli twin is in the Out-of-scope notes.

### CORE-31 · LOW · docs · self-verified
**`save()`'s doc block claims `Manager::create` does NOT fire per-row `post_save` — the opposite of what `create()` does (and of the comment explaining why it was fixed).**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:6089-6096` (doc) vs `:5026-5031` and `:5082-5087` (code + its own comment).
- **Evidence**:
  ```
  /// ## Bulk paths fire bulk signals, not per-row signals
  ///
  /// `Manager::create`, `Manager::bulk_create`, and
  /// `QuerySet::update_values` / `QuerySet::delete` do NOT fire
  /// per-row `post_save` / `post_delete`. They fire
  /// `bulk_post_save:<table>` / `bulk_post_delete:<table>` once per
  /// statement with the affected PKs in the payload. Use `save` /
  /// `delete_instance` when per-row signal semantics are needed.
  ```
  (`mod.rs:6089-6096`) vs, in `create()` itself:
  ```rust
  // gaps3 #29: `create()` emitted NOTHING, while `save()` and
  // `update_or_create()` both emit per-row `post_save`. …
  crate::signals::emit_post_save::<T>(&row, true).await;
  ```
  (`mod.rs:5026-5031`, mirrored at `:5082-5087`) — `create` emits per-row `post_save` and never a bulk signal (the only `emit_bulk_post_save` sites are `:4055`, `:4177`, `:5252`).
- **Impact**: a consumer reading the doc to choose a write API is told the opposite of the truth: it suggests `objects().create()` is invisible to per-row subscribers (so an app might route creates through `save()` "for the signals" — a path that, per CORE-2/CORE-10, is the less complete one), and it misdescribes `create()`'s payload shape for anyone writing a `post_save` subscriber. Not covered by gaps6 #21's stale-comment batch (which lists five other files/sites).
- **Fix**: correct the sentence to name only the genuinely bulk paths (`bulk_create`, `update_values`/`update_expr`, `delete`), and note that `create` fires the per-row signal since gaps3 #29.

### CORE-32 · LOW · ergonomics/docs · self-verified
**`try_for_each`'s caveat under-documents what it drops: `join_related` is silently ignored (not named in the caveat), `.only()` is unguarded, and every returned row's M2M handles are unattached so `add()` no-ops inside the callback.**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:2203-2208` (caveat), `:2209-2279` (body); guard sites it lacks: `:2071-2073` (only), `:2084` (join validation), `:2110` (join application), `:2119-2124` (hydration).
- **Evidence**: the doc says
  ```
  /// transaction. `select_related` and `prefetch_related` hooks
  /// are NOT applied on each row — `try_for_each` is intentionally
  /// the "raw column data, one row at a time" terminal.
  ```
  (`mod.rs:2205-2208`), but the body calls only `check_annotations` and then `self.build_query_for(...)` per chunk (`:2220`, `:2245`, `:2254`) — `build_query_for` does not apply `join_related` (`apply_join_related` is called separately, by `to_sql`/`to_sql_pg`/`fetch` at `:1796,:1814,:2110`), so `.join_related("author")` adds neither the JOIN nor hydration and does not run `validate_join_related_fields` (so a typo'd field name errors under `fetch()` and is silently ignored here). `only_cols` is never checked, so `.only(&["id"])` falls through to the generic `T::FromRow` decode and surfaces as a driver "column not found" instead of the structured `only_with_typed_terminal_error("first")`-style message `fetch`/`first` give. Rows come from `query_as_with` with no `set_m2m_parent_ids` call, so `row.tags.add(&t)` in the callback is the same unattached-handle no-op as CORE-2.
- **Impact**: a caller who chains a relation builder before `try_for_each` (a natural refactor from `fetch()`) gets rows with `ForeignKey::resolved()` empty / M2M writes dropped and no warning, while the doc names only two of the three relation hooks. Boundary case, bounded — the terminal's stated purpose is raw streaming — but the fix is cheap.
- **Fix**: extend the caveat to name `join_related` and M2M handles, and add the same `only_cols`/`join_related` guards the other terminals use (each is two lines) or a `debug_assert`-style refusal.

### CORE-33 · LOW · drift · self-verified
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

### CORE-34 · LOW · docs/drift · self-verified
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

### CORE-35 · LOW · docs
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

### CORE-36 · LOW · correctness
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

### CORE-37 · LOW · error-handling · self-verified
**`reset_sequence` swallows its own failure: `let _ = sqlx::query(&sql).execute(p).await;` — a failed sequence reset leaves the copied ids' next auto-increment value untouched, and the inevitable duplicate-key error surfaces later in unrelated app code.**

- **Where**: `crates/umbral-core/src/transfer.rs:983-984`; the twin that propagates is `crates/umbral-core/src/backup.rs:402-436` (`reset_pg_sequence` — both awaits use `?`).
- **Evidence**:
  ```rust
  // transfer.rs:983-984
  // A non-integer PK has no serial sequence; the guard makes this a no-op.
  let _ = sqlx::query(&sql).execute(p).await;
  ```
- **Impact**: the no-op case the comment describes is already handled by the SQL itself (`WHERE pg_get_serial_sequence('{t}', '{c}') IS NOT NULL`, 976-982 — zero rows on a non-serial PK, still `Ok`). The `let _` therefore only suppresses **real** failures (a permission error, a dropped connection at that instant, a setval edge). After a transfer whose reset failed, the app's next ORM insert into the transferred table collides with a copied id (`23505 duplicate key`) — a confusing failure in unrelated code with no record that the sequence reset was the cause. This is a drift between the two sequence-reset implementations in this area: backup's twin fails loudly.
- **Fix**: `sqlx::query(&sql).execute(p).await?;` — the WHERE guard already makes the non-serial case a no-op, so propagating cannot introduce a spurious failure.
- **Status**: new. Self-verified: re-read the function and its call context, and the backup.rs twin; confirmed the no-op guard is the SQL predicate, not the discard.

### CORE-38 · LOW · drift · self-verified
**Backup restore: the SQLite binder truncates `Integer`/`SmallInt` with `as i32` where the Postgres binder uses checked `i32::try_from`/`i16::try_from` — an out-of-range dump value silently wraps on SQLite and errors on Postgres.**

- **Where**: `crates/umbral-core/src/backup.rs:1045-1047` (SQLite) vs `backup.rs:1164-1171` (Postgres).
- **Evidence**:
  ```rust
  // backup.rs:1045-1047 (bind_value, sqlite)
  SqlType::SmallInt | SqlType::Integer => {
      q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))? as i32)
  }
  ```
  ```rust
  // backup.rs:1168-1171 (bind_value_pg)
  SqlType::Integer => q.bind(
      i32::try_from(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?)
          .map_err(|_| mismatch("number out of i32 range"))?,
  ),
  ```
- **Impact**: a dump row carrying a value outside `i32` for a column the model declares `Integer` (writable only via direct SQL — SQLite's dynamic typing accepts it; both dump paths read it as an i64 JSON number, `column_to_json`) restores on SQLite as a wrapped value (e.g. `4294967303` → `7`) while the same dump into Postgres is rejected loudly with `number out of i32 range`. Silent numerical corruption on the restore path, backend-divergent — and the checked twin already exists 120 lines below.
- **Fix**: use `i32::try_from`/`i16::try_from` in the SQLite arm too; the JSON value space is identical (`as_i64` on both sides).
- **Status**: new. Self-verified: read both binder arms side by side; confirmed `insert_rows_sqlite`/`insert_rows_postgres` route to exactly one binder each.

### CORE-39 · LOW · docs
**backup.rs's module doc says the dump "runs `SELECT * FROM <table>`"; the code selects an explicit column list — the doc describes a shape the module deliberately does not use.**

- **Where**: `crates/umbral-core/src/backup.rs:4-6` vs `backup.rs:470-473`.
- **Evidence**:
  ```rust
  // backup.rs:4-6
  //! The two halves are symmetric. [`dump`] walks
  //! `migrate::registered_models()`, runs `SELECT * FROM <table>` for
  //! each, and dispatches per column's [`SqlType`] to read every value
  ```
  ```rust
  // backup.rs:470-473
  let sql = format!(
      "SELECT {} FROM {}",
      column_list(model),
      quoted_ident(&model.table)
  );
  ```
- **Impact**: readers (and future second dumpers) are told `*` while the round-trip is per-`ModelMeta::fields`; "SELECT *" would actually imply the opposite property (dumps depend on live DB columns, not the model), so the doc misstates the module's data-coverage contract. No runtime behavior impact.
- **Fix**: reword line 5 to "runs `SELECT <the model's columns> FROM <table>` (explicit list, so a dump carries exactly the model's fields)".
- **Status**: new.

### CORE-40 · LOW · correctness
**An error body larger than 64 KiB is silently swapped for an empty string before the error message reaches the log, the `server_error` hook, and the dev-mode page.**

- **Where**: `crates/umbral-core/src/errors.rs:544-550` (`render_500_middleware`), `crates/umbral-core/src/errors.rs:630-634` (`render_error_middleware`)
- **Evidence**:
  ```rust
  // Capture the body to extract the error message for the hook + dev
  // context. 64KB cap: error messages don't need more, and we don't
  // want a malicious upstream to OOM us.
  let (_parts, body) = resp.into_parts();
  let bytes = axum::body::to_bytes(body, 64 * 1024)
      .await
      .unwrap_or_default();
  ```
  (the 4xx sibling at `:630-634` is identical: "Capture the body (the handler's message). 64KB cap, same as the 500 path.")
- **Impact**: `to_bytes` returns `Err(LengthLimitError)` when the body exceeds the cap, and `unwrap_or_default()` turns that into `[]`, so `humanize_error_body("")` produces an empty message. A handler whose 500 / `(status, message)` body exceeds 64 KiB — a rendered error page, a validation-errors JSON with many fields, a long error chain — therefore logs a blank line, fires `fire_server_error_hook` with an empty message, and (dev mode) renders the error page with no message. The served body is still correct (it is re-rendered from the template, not the captured bytes), so the loss is invisible until someone needs the diagnostic. The comment says the cap exists because "error messages don't need more" — the intended degradation is truncation; today it is amnesia.
- **Fix**: On capture failure keep the leading bytes (read through a limit that truncates rather than errors, or re-read with the cap removed after warning), or at minimum `tracing::warn!` when the capture fails so the blank message has an explanation.

### CORE-41 · LOW · api
**`StreamingResponse` panics in `into_response` for a caller-supplied `content_type` that is not a valid header value — the public setter accepts any `String`.**

- **Where**: `crates/umbral-core/src/web/streaming.rs:78-83` (setter), `:121-122` (the `expect`)
- **Evidence**:
  ```rust
  /// Set the `Content-Type`. Defaults to `application/octet-stream`.
  pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
      self.content_type = content_type.into();
      self
  }
  ```
  ```rust
  builder
      .body(self.body)
      .expect("content-type / content-disposition are always valid header values")
  ```
- **Impact**: `http::response::Builder::header` defers `HeaderValue` validation to `.body()`, so a `content_type` containing a control character — e.g. `.content_type(format!("{stored_ct}; charset=utf-8"))` where `stored_ct` comes from a DB column or upload metadata, the natural file-serving pattern — makes `into_response` panic inside the request path: a 500 via `CatchPanicLayer`, or a dropped connection when that layer is disabled. The `expect` message asserts an invariant the API does not enforce ("always valid" is true only for values the framework itself writes; the setter is public and accepts arbitrary strings). Latent today — no in-tree caller passes a dynamic value (`grep '\.content_type('` finds only `crates/umbral-core/tests/streaming_response.rs:20`) — but it is the one panic this slice exposes to user code. `Content-Disposition` is already sanitized (`sanitize_filename`, `:128-132`); `Content-Type` is the unsanitized sibling — the drift *is* the finding.
- **Fix**: Validate at the setter boundary — fall back to `application/octet-stream` for an invalid value (or accept `HeaderValue`), and in `into_response` return a 500 `Response` instead of `expect`, since `IntoResponse` can return a fresh response and there is no reason to panic here.

### CORE-42 · LOW · docs
**Five `ignore`d doc examples call `.build().await?`, but `AppBuilder::build()` is synchronous.**

- **Where**: `crates/umbral-core/src/app.rs:916-917`, `:943-944`, `:1019`, `:1041`; `crates/umbral-core/src/cors.rs:33-34`; signature at `crates/umbral-core/src/app.rs:1122`
- **Evidence**:
  ```rust
  ///     .build()
  ///     .await?
  ```
  (`app.rs:916-917`; `app.rs:1019` and `:1041` are `///     .build().await?;`; `cors.rs:33-34` is the `//!` twin)
  ```rust
  pub fn build(self) -> Result<App, BuildError> {
  ```
- **Impact**: Copy-pasting any of these examples does not compile — `Result` is not awaitable. They are `ignore` blocks, which is why rustdoc/CI never caught the drift, so the reader has to debug the framework's own example. Five occurrences across two files make this a missed find-replace after `build()` became sync, not five independent typos, and they are not covered by the stale-comment batch in `gaps6 #21`.
- **Fix**: Drop `.await` in all five sites; consider un-`ignore`ing one so the compiler pins the signature permanently.

### CORE-43 · LOW · docs
**`AppBuilder::cors()`'s doc claims it "becomes the outermost wrapper" — contradicted by the layering the same function installs.**

- **Where**: `crates/umbral-core/src/app.rs:920-921` (claim) vs `:1887-1888` (global CORS applied here) and `:1898-1901` (host guard applied later, i.e. outside)
- **Evidence**:
  ```rust
  /// The layer is applied LAST in the middleware chain so it
  /// becomes the outermost wrapper — preflight `OPTIONS` is
  ```
  ```rust
  if let Some(cors) = self.cors.take() {
      router = router.layer(cors.into_layer());
  }
  ...
  // Phase 5.95 — Host-header validation (allowed-hosts allowlist). Applied
  // outermost so a forged `Host` is rejected with a 400 before any
  // handler, plugin, or CORS logic runs.
  ```
- **Impact**: Every `router.layer()` wraps what exists, so the layers applied *after* CORS — host guard, body-limit, timeout, default security headers, `TraceLayer` — all sit outside it; the Phase 5.95 comment in the same file even states the host rejection happens "before any ... CORS logic runs". Concretely: a host-rejected 400 (and a 413/408/trace-wrapped response) carries no CORS headers, so a browser reports an opaque CORS failure instead of showing the real status. The behaviour is defensible; the doc sentence invites misdiagnosis, and the two comments in one file contradict each other.
- **Fix**: Reword the `cors()` doc: the layer wraps the app's routes and middleware but sits *inside* the host / hardening / trace layers — describe the order it actually gets.

### CORE-44 · LOW · api
**A middleware that short-circuits from `before_request` never gets its own `after_response` call, and neither the docs nor the pinning test say so.**

- **Where**: `crates/umbral-core/src/middleware.rs:163-170` (`ran` incremented only on `Ok`), `:186-189` (unwind); docs `:20-23` and `:74-75`; test `crates/umbral-core/tests/middleware_pipeline.rs:57-60, 149-161`
- **Evidence**:
  ```rust
  match mw.before_request(req).await {
      Ok(modified) => {
          req_opt = Some(modified);
          ran += 1;
      }
      Err(resp) => {
          short_circuit = Some(resp);
          break;
      }
  }
  ```
  ```rust
  // Only the middleware whose `before_request` ran get an
  // `after_response`, in reverse (onion unwind).
  for mw in stack.iter().take(ran).rev() {
      res = mw.after_response(res).await;
  }
  ```
- **Impact**: The doc sentence "only the `after_response` hooks of the middleware that already ran … get to see the short-circuit response" (module doc `:18-23`, trait doc `:72-75`) is ambiguous about the middleware whose `before_request` *produced* the short circuit: it ran, but `ran` counts only `Ok`, so it is excluded. A middleware that splits the work — reject in `before_request`, decorate the rejection in `after_response` (e.g. adding `Retry-After` to its own 429) — silently loses the decoration, with the only workaround being to set headers on the `Err` response directly. The test that pins the behaviour cannot catch a change: its `Gate` (line 57) implements `before_request` only, so "the short-circuiter's `after_response` must not run" and "it is never called" are indistinguishable; the assertion message at `:161` ("only already-run middleware unwind on short-circuit") encodes the ambiguity rather than resolving it.
- **Fix**: Decide and document explicitly: either run `after_response` on the short-circuiting middleware too (arguably the correct onion — it did run), or state in both doc sites that the short-circuiter is excluded; then give the test's `Gate` a recording `after_response` and assert whichever behaviour is chosen.

### CORE-45 · LOW · correctness
**A `Host` header in fully-qualified form (`example.com.`, trailing dot) fails the prod host guard.**

- **Where**: `crates/umbral-core/src/hosts.rs:66-72` (`hostname_of`), `:53-62` (`match_host`), `:39-44` (`HostPolicy::new` lowercases only)
- **Evidence**:
  ```rust
  fn hostname_of(raw: &str) -> &str {
      let raw = raw.trim();
      if let Some(rest) = raw.strip_prefix('[') {
          return rest.split(']').next().unwrap_or(rest);
      }
      raw.split(':').next().unwrap_or(raw)
  }
  ```
  ```rust
  if let Some(domain) = pattern.strip_prefix('.') {
      // ".example.com" matches "example.com" and any subdomain of it.
      return host == domain || host.ends_with(pattern);
  }
  host == pattern
  ```
- **Impact**: `Host: example.com.` is normalized only by `trim()` + port-strip, so the trailing dot survives into `match_host`, where plain patterns compare with `==` — and a leading-dot pattern `.example.com` misses it too (`"example.com.".ends_with(".example.com")` is false). No match → `HostPolicy` (enforced only in `Environment::Prod`) answers 400. The trailing-dot FQDN is legal (RFC 3986 §3.2.2 / RFC 1035) and is emitted by some HTTP clients, health checkers, and string-built proxies; the guard is off in dev, so the app works locally and starts 400ing those callers in production only.
- **Fix**: Strip one trailing `.` in `hostname_of` (and normalize allowlist entries the same way in `HostPolicy::new`); the existing equality/suffix logic is then correct.

### CORE-46 · LOW · correctness/api
**`Field::select` / `Field::fk_select` with `nullable = false` mark the field required but install zero validators, so an empty submission validates clean.**

- **Where**: `crates/umbral-core/src/forms.rs:576-588` (`select`) and `:596-610` (`fk_select`), `:670-679` (`validate`)
- **Evidence**:
  ```rust
  pub fn select(name: impl Into<String>, options: Vec<(String, String)>, nullable: bool) -> Self {
  ...
          required: !nullable,
          validators: Vec::new(),
  ```
  ```rust
  pub fn validate(&self, value: &str, errors: &mut ValidationErrors) {
      if !self.required && value.is_empty() {
          return;
      }
      for v in &self.validators {
  ```
- **Impact**: A required select has no way to fail server-side: with `required: true`, `validate("")` skips the early return, then iterates an empty validator list and reports nothing — so `POST field=` is accepted. The docstring at `:573-575` ("`nullable` … drops `Required`") implies a `Required` validator exists to drop; it does not, so `required` means only the HTML attribute (client-side only). The derive path is safe — `umbral-macros` attaches `validate_choice_member` as a validator (`crates/umbral-macros/src/lib.rs:5847, 5870-5878`) — so the trap is limited to hand-built forms via the public `forms::Field` API (the only in-tree `Field::select` caller is the macro).
- **Fix**: In both constructors, when `!nullable` push a `Required` validator (or special-case `self.required && value.is_empty()` in `validate`); reserve `validators: Vec::new()` for the nullable case.

### CORE-47 · LOW · tests
**No test drives the slash-redirect layer through `App::builder()` — the snapshot-after-middleware-stack property (the review_3-class fix) has no regression cover.**

- **Where**: wiring `crates/umbral-core/src/app.rs:1794-1805` and the builder `:710-711`; layer tests `crates/umbral-core/tests/slash_redirect.rs:22-27`
- **Evidence**:
  ```rust
  // Slash-redirect layer (see the Phase 5.6 note above). Installed
  // after the middleware stack so the snapshot it probes runs the
  // alternate-form request through the global middleware rather than
  // around it, and before the panic / compression / CORS / host layers
  // so a probe never needs those outermost wrappers to decide a
  // redirect. ...
  if self.slash_redirect != crate::slash::SlashRedirect::Off {
      let snapshot = router.clone();
  ```
  ```rust
  fn router_with_fallback(router: Router, policy: SlashRedirect) -> Router {
      if policy == SlashRedirect::Off {
          return router;
      }
      router.layer(axum::middleware::from_fn(slash_redirect_probe(
  ```
- **Impact**: `grep -rn "\.slash_redirect(" crates/umbral-core/tests plugins/*/tests` returns nothing, and `SlashRedirect::default()` is `Off` (`slash.rs:85-88`) — so no test sets a non-`Off` policy through `App::builder()`. `tests/slash_redirect.rs` exercises `alternate_path` and the probe on hand-built routers, and the auth plugin's `both_slash_forms_of_login_resolve` passes with the layer off; the property that motivated the current wiring — the probe must run *through* the global middleware stack so it cannot bypass rate-limiting/auth — is asserted nowhere. Reverting the fix to a pre-stack snapshot would leave every test green.
- **Fix**: One integration test in the style of `tests/middleware_pipeline.rs`: boot with `App::builder().slash_redirect(SlashRedirect::Append)` plus a counting (or denying) `Middleware`, request the non-canonical form, and assert both the 308 and that the middleware observed the probe (counter incremented, or middleware-denied → no redirect).

### CORE-48 · LOW · docs
**`plugin.rs`'s module doc says this file is an incomplete M7 subset and its `on_ready` doc route plugins through a method that doesn't exist.**

- **Where**: `crates/umbral-core/src/plugin.rs:8-10`, `plugin.rs:540-543`, `plugin.rs:625-633`
- **Evidence**:
  ```rust
  // plugin.rs:8-10
  //! See `docs/specs/02-plugin-contract.md` for the eventual target
  //! shape; this file ships the M7 v1 subset (no middleware, no commands,
  //! no inventory auto-registration).

  // plugin.rs:541-542
  /// Wire signals, start background work, seal admin registrations. …
  /// `ctx.runtime()` when the runtime handle lands.
  ```
- **Impact**: All three "missing" pieces shipped (`fn middleware` at ~:413, `fn commands` at ~:511, `inventory` re-export + `inventory::collect!(DtoMeta)` in lib.rs) — a reader auditing "is the plugin contract complete?" gets a false negative from the file that defines it. The `ctx.runtime()` mention is flagged as future ("when the runtime handle lands") so it's honest, but `AppContext` (only `pool` + `settings`) has no such method anywhere in core; gaps6 #8 (RuntimeProfile) is the tracked home.
- **Fix**: rewrite the module paragraph to describe the shipped contract (or point at gaps6 #8 for the remaining runtime-handle work) and drop the `ctx.runtime()` reference from the sync rationale.

### CORE-49 · LOW · docs
**Milestone-of-record docs are stale in three more places, two of them factually wrong about the shipped runtime.**

- **Where**: `crates/umbral-core/src/lib.rs:5`, `crates/umbral-core/src/backend.rs:13-19` (+ `:434-439`), `crates/umbral-core/src/check.rs:9-17`
- **Evidence**:
  ```rust
  // lib.rs:5
  //! Status: M0 shipped — Settings, db pool, web re-exports, App builder.

  // backend.rs:13-18
  //! - [`PostgresBackend`] — declared and queryable for compatibility
  //!   checks, but the umbral pool is still `sqlx::SqlitePool` at M4. The
  //!   real `sqlx::PgPool` wiring lands when there's a real user need;

  // check.rs:9-11
  //! At M4 the only check that's meaningful without a model registry or
  //! Plugin walk is [`settings_required`] — it verifies that production
  ```
- **Impact**: `backend.rs` is not merely old, it's wrong: `DbPool::Postgres` is a live runtime path (`db.rs` dispatch, `transaction_pg`), and `BackendDetectError`'s Display ("M4 supports `sqlite://` and `postgres://`", `backend.rs:434-439`) misdescribes the shipped detector. `check.rs:9` undercounts a catalogue that now spans model, plugin, route and settings checks. (Coordinator note, added during assembly: of the deferred set the same paragraph lists, only `field.backend` is now registered — `model.pk.present`, `model.table.unique`, `route.collision` remain absent from `framework_checks()`; that missing-implementation angle is filed as CORE-25, and the two findings are complementary, not contradictory.) A reader trusting the docs will re-derive wrong assumptions about the pool layer.
- **Fix**: replace the milestone prose with the current one-liners; keep history in git, not rustdoc.

### CORE-50 · LOW · docs
**A rustdoc block collision in `check.rs` attaches the wildcard-doc to `auto_user_columns_nullable` and leaves `settings_allowed_hosts_wildcard` undocumented.**

- **Where**: `crates/umbral-core/src/check.rs:402-419` (function at `:474`)
- **Evidence**:
  ```rust
  // check.rs:402-403 — the wildcard check's doc …
  /// Warn when `allowed_hosts` contains the `"*"` wildcard in
  /// `Environment::Prod` (audit_2 core-app-config #13). A wildcard makes the
  // … no blank line, then, at :412:
  /// An `auto_user` / `auto_user_add` column must be nullable (gaps3 #55).
  fn auto_user_columns_nullable(_ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
  ```
- **Impact**: `cargo doc` shows the wildcard rationale as the doc of the nullable-column check (both blocks attach to `auto_user_columns_nullable`), and `settings_allowed_hosts_wildcard` (`:474`) has no doc at all — the explanation for a Prod-only security warning is filed under the wrong function.
- **Fix**: move lines 402-411 down to line 474.

### CORE-51 · LOW · docs (contradicts shipped behavior)
**`ratelimit.rs`'s module doc still promises "a periodic sweep is a future hardening" while the periodic sweep shipped, is on by default, and is untested.**

- **Where**: `crates/umbral-core/src/ratelimit.rs:39-44` vs `ratelimit.rs:66-70` and `:207-213`
- **Evidence**:
  ```rust
  // ratelimit.rs:39-44 (module doc, "Scope and limits")
  //! - **Unbounded key set.** The `HashMap` grows one entry per distinct
  //!   key and entries are pruned lazily on next `check` of that key, never
  //!   swept globally. … a periodic sweep is a
  //!   future hardening.

  // ratelimit.rs:66-70 (the shipped fix the doc denies)
  /// the map without bound (audit_2 core-web #4). Every `SWEEP_EVERY` checks the
  /// limiter runs [`RateLimiter::sweep_at`] over the WHOLE map, dropping every
  const SWEEP_EVERY: usize = 1000;
  ```
- **Impact**: The module doc is the first thing a reader sees; it asserts a live memory-bound defect that audit_2 core-web #4 already closed — inviting a duplicate finding/tracker entry and hiding the sweep's existence from anyone tuning the limiter. `tests/ratelimit.rs` has no test touching the sweep (six cases, all window semantics), so the shipped bound itself has zero regression coverage.
- **Fix**: rewrite the bullet to describe `SWEEP_EVERY` and add a unit test asserting the key map shrinks after `SWEEP_EVERY` churned keys (a `len()` probe via an existing accessor, or a test-only hook).

### CORE-52 · LOW · correctness (swallowed error)
**`merge_dotenv` silently drops malformed `.env` lines via `iter.flatten()`, so a typo'd `.env` line quietly boots the app with defaults.**

- **Where**: `crates/umbral-core/src/settings.rs:321`
- **Evidence**:
  ```rust
  for (key, value) in iter.flatten() {
      let Some(key) = dotenv_key(&key) else {
          continue;
      };
  ```
- **Impact**: `dotenvy`'s iterator yields `Err` for a malformed line (an unclosed quote, an invalid escape). `.flatten()` discards it with no message, so a developer who mistypes `UMBRAL_SECRET_KEY="abc` gets an app running on the dev default with nothing anywhere saying why — the same class CLAUDE.md calls out under "fix, don't patch" (a `.ok()`-style discard). Dev-only surface, hence LOW.
- **Fix**: match on the item and `tracing::warn!`/`eprintln!` the malformed line's number (dotenvy's error carries it); keep the last-wins/prefix semantics otherwise.

### CORE-53 · LOW · observability / drift
**The hand-written redacting `Debug` for `Settings` omits `trusted_proxy_hops` — the manual field list already drifted once.**

- **Where**: `crates/umbral-core/src/settings.rs:592-617` vs the struct field at `settings.rs:440-441`
- **Evidence**:
  ```rust
  // settings.rs:592-594 — "prints the rest verbatim so the value stays useful"
  impl std::fmt::Debug for Settings {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          f.debug_struct("Settings")
  // … fields: database_url, databases, max_form_body_bytes, …, static_root, extra
  // trusted_proxy_hops: absent.
  ```
- **Impact**: `trusted_proxy_hops` is the security-relevant setting from audit_2 H9 (mis-set hops mis-attribute client IPs and weaken throttles) and is exactly the field an operator wants in a `?settings` incident dump — it's the only struct field the impl drops. The pattern (manual field list) silently drops any field added later, so the omission is a symptom, not a typo.
- **Fix**: add `.field("trusted_proxy_hops", &self.trusted_proxy_hops)`; consider a `debug_assert`-style compile-time tripwire or a test comparing field names against a mirror list so the next addition fails loudly.

### CORE-54 · LOW · correctness (interactive-tool panic)
**`prompt::ask_target` underflows on input "0": `plugins.get(n - 2)` panics in debug builds.**

- **Where**: `crates/umbral-core/src/codegen.rs:526-534`
- **Evidence**:
  ```rust
  if let Ok(n) = answer.parse::<usize>() {
      if n == 1 {
          return Ok(Target::Root);
      }
      if let Some(p) = plugins.get(n - 2) {   // n == 0 ⇒ 0 - 2 underflow
          return Ok(Target::Plugin(p.clone()));
      }
      println!("  no such choice: {n}");
  ```
- **Impact**: A developer answering the "Where should it live?" prompt with `0` (a plausible mistake — it's the first item's neighbour) crashes the generator with "attempt to subtract with overflow" in debug; release wraps to a huge index, prints "no such choice: 0", and re-prompts — a debug/release behavior split in an interactive path. LOW: dev-time, recoverable.
- **Fix**: `answer.parse::<usize>().ok().and_then(|n| n.checked_sub(2)).and_then(|i| plugins.get(i))` (root for `1`, re-prompt for `0`).

### CORE-55 · LOW · drift
**The dynamic-pool registry's poison policy is asymmetric: writes `.expect()` (fail-closed), reads `.ok()` (fail-open) — and the fail-open read makes `pool_for_dispatched` panic with a message that blames configuration.**

- **Where**: `crates/umbral-core/src/db.rs:270-272` (write), `db.rs:244-249` (read), `db.rs:250-252` (panic text), `db.rs:284-288` (availability read)
- **Evidence**:
  ```rust
  // db.rs:270-272 — write path: panic on poison
      .write()
      .expect("umbral: dynamic pool registry poisoned");

  // db.rs:244-248 — read path: poison ⇒ silently treated as "alias absent"
  if let Some(p) = DYNAMIC_POOLS
      .get()
      .and_then(|reg| reg.read().ok().and_then(|m| m.get(alias).copied()))
  …
  // db.rs:251 — the resulting panic misattributes the cause
  panic!("umbral: no database registered under alias '{alias}'");
  ```
  `pool_alias_registered` (`db.rs:286-288`) likewise returns `false` on poison ("A router can use this to fall back to the default pool" — so a poisoned lock silently reroutes tenant traffic to the default DB if a router gated on it).
- **Impact**: Poisoning is near-unreachable (the write critical section can't panic short of OOM), so this is a consistency hazard, not a live bug — but if it occurs, three sites in one subsystem react three ways (panic, silently-absent, silently-false) and the loudest one gives a misleading reason. The reset-gate path in the same file already uses the deliberate `PoisonError::into_inner` recovery (`timezone.rs:43` chooses recovery too) — the file should pick one policy.
- **Fix**: pick recovery (`.unwrap_or_else(PoisonError::into_inner)`) for the registry reads and keep the write path panicking only if recovery is impossible; or make both fail-closed with the "poisoned" wording. Note the choice in the `DYNAMIC_POOLS` doc.

### CORE-56 · LOW · drift
**The Postgres-only SqlType set is duplicated between `check.rs::is_postgres_only` and `SqliteBackend::map_type`'s panic arms, with nothing pinning the two in agreement.**

- **Where**: `crates/umbral-core/src/check.rs:1013-1034`, `crates/umbral-core/src/backend.rs:330-379`
- **Evidence**:
  ```rust
  // check.rs:1016-1034 — the boot gate's list
      matches!(
          ty,
          SqlType::Array(_)
              | SqlType::Inet
              | SqlType::Cidr
              | SqlType::MacAddr
              // gaps2 #70: text-backed Postgres types (XML / LTREE /
              // BIT VARYING) have no SQLite equivalent; …
              | SqlType::Xml
              | SqlType::Ltree
              | SqlType::Bit
              | SqlType::FullText
              // BUG-10: … | SqlType::Decimal | SqlType::DecimalN(_)
              | SqlType::BigDecimal
              | SqlType::Geometry(_)
              | SqlType::Geography(_)
      )

  // backend.rs:330-379 (SQLite map_type) — the runtime twin, as panic arms:
              SqlType::Array(_) => panic!(
                  "umbral::backend::SqliteBackend::map_type: SqlType::Array is Postgres-only. \
                   The field.backend system check should have failed boot; …"
              ),
  ```
- **Impact**: The pair agrees today (verified variant by variant), but the boot check exists precisely to keep these panics unreachable; a new PG-only `SqlType` added to the panic arm and not to `is_postgres_only` makes boot pass and the panic reachable, and vice versa the check rejects a type the backend could have mapped. `tests/backend.rs` point-checks only `Uuid` — no exhaustive agreement test exists.
- **Fix**: a test iterating every `SqlType` variant asserting `SqliteBackend.map_type` panics iff `is_postgres_only(ty)` (or vice versa via a `catch_unwind` matrix), or derive one list from the other.

### CORE-57 · INFO · hygiene
**`cleaners::any_registered` fails open on a poisoned registry while every sibling `expect`s — on poison, all cleaners silently stop running.**
- **Where**: `crates/umbral-core/src/orm/cleaners.rs:106-108` vs `:87`, `:99`, `:123`.
  `registry().read().map(|r| !r.is_empty()).unwrap_or(false)` → a poisoned lock makes `apply` return `Ok(None)` (cleaner skipped, value written raw), whereas `register`/`clear_for_tests`/`apply`'s own body `expect("cleaner registry poisoned")`. Either is defensible; the mix is not — the signals registry's chosen posture is `into_inner()` recovery (`signals.rs` `lock_registry`). No action required; align the posture when the file is next touched.

### CORE-58 · INFO · robustness
- **Where**: `crates/umbral-core/src/backup.rs:520` (and `:581`).
- A dump row **missing** a column the current model still has silently binds `Value::Null` (`row.get(&col.name).cloned().unwrap_or(Value::Null)`), while a dump row with an **extra** column is a hard `BackupError::UnknownColumn` nine lines above (511-515). The asymmetry looks unintentional: loading an older dump against a newer schema (nullable column added since) fills NULL with no warning, and a typo cannot be distinguished from "legitimately absent". Document the forward-compat intent or warn/count missing columns.

### CORE-59 · INFO · architecture
**Where**: `crates/umbral-core/src/check.rs:973-1007` — `plugin_security_missing` gates built-in behaviour by string-matching plugin names (`names.contains(&"auth")`, `"sessions"`, `"security"`); core can't otherwise know a plugin's role, so a `Plugin::provides()`-style role tag would delete the string coupling — names are reserved so this is safe, observation only (audit_2 core-web #1 owns the warning-vs-error question).

### CORE-60 · INFO · perf
**Where**: `crates/umbral-core/src/db/router.rs:202-205` + `crates/umbral-core/src/db/route_context.rs:106-110` — `schema_qualified_table` calls `route_context::current()` per table reference: an `Arc` clone per FROM/JOIN under a task-local, and a fresh `Arc::new(RouteContext::default())` when none is set (CLI, tests, background work). Cold-path micro-allocation next to SQL string building; no action required.

## Strengths

- **Injection posture is uniform and verifiable, across both the query layer and the DDL/DML paths.** Every user value is bound (`sea_query::Value` / `sqlx` bind), every identifier goes through `Alias::new` or `quote_ident` (`search.rs:99-101`), and the one deliberately-verbatim SQL splice (`Searchable::filter_sql`) is constrained by its return type and documented with the threat model at `search.rs:138-145`. `Q::not` negates the SQLite-specific condition too, so the two backend arms cannot drift (`expr.rs:301-309`). In the migration/backup/transfer files every `format!`-built statement quotes identifiers and doubles embedded quotes (`transfer.rs:980-981`, `migrate.rs:5578-5579`), and values are always bound.
- **The signal/redaction contract was clearly hardened and is respected at every site it was fixed for**: per-row `post_delete` payloads strip `SIGNAL_SKIP_FIELDS` before emitting (`queryset/mod.rs:3923-3937`), the tx writer buffers the *same* closures the non-tx path uses rather than re-deriving payloads, and the `create`/`update_or_create` signal seams carry comments naming the incident that produced them (gaps3 #29/#14/#15 lineage).
- **The SQLite/Postgres decode twins are structurally symmetric.** `backend_sqlite.rs` and `backend_pg.rs` implement the same four functions (`row_to_json`, `hydrate_joined_rels`, `extract_m2m_child_json`, `pk_to_json`, `decode_agg`) with mirrored control flow, and both read hop tables off the shared `RelPath::from_path` resolver (gaps6 #4) instead of hand-rolling path parsing. CORE-1 is the one place the two genuinely disagree, and it disagrees because of a wrong *PG type model*, not a forgotten branch.
- **`implicit_predicates()` is a real single seam.** Soft-delete scoping flows through one trait method consumed by `build_query_for`, `build_update_for`, and the delete paths, so `.with_deleted()`/`.only_deleted()` cannot be honored on one terminal and not its sibling (the review_3-verified `QuerySetTx::delete` hard-delete bug was fixed here).
- **The failure modes are fast and structured where the framework controls them**: unknown columns fail before SQL (`queryset/mod.rs:4310-4315`, `:2643-2655`, `:2977-2990`), `check_annotations` exists precisely so the infallible builder path can skip poisoned annotations while fallible terminals fail loudly (`:810-816`), and the `_pg` terminals refuse unsupported chained features with a message that explains why (`:1639-1667`) rather than returning un-hydrated rows.
- **Comment discipline: write-path and migration comments carry their incident history, and boot-phase comments state their reasons.** The write paths name gap68, features #83, gaps4 #85, BUG-16, gaps3 #29/#54; `migrate.rs` non-obvious branches name gap 97, gaps2 #22, gaps3 #35/#43, audit_2 #5/#10/H21 and review_3's `col_no_unique` fix; `app.rs`'s Phase 5.6/5.65/5.9/5.95/5.99 comments each state *why* the layer sits where it does and which earlier bug motivated it. This is what made both the drift findings and the boot ordering reviewable at all — keep it.
- **`signals.rs`'s registry proves the historic mutex-poisoning incident is genuinely closed, and the design is better than the minimum fix.** `lock_registry` recovers from poison (`into_inner`), sync handlers run under `catch_unwind` (`:310-313`), async handlers under `AssertUnwindSafe(...).catch_unwind()` *and* a documented per-subscriber `ASYNC_SUBSCRIBER_TIMEOUT` of 30s (`:55-61`, `:334-348`), the registry lock is dropped before any handler is awaited, and `emit` returns the total count. The regression test `panicking_sync_handler_does_not_brick_the_registry` asserts the contract directly. Transactional `emit_txn` deliberately does *not* catch panics and documents why.
- **`soft_delete_cascade.rs`'s core design (CORE-3 aside) is exemplary**: subquery-driven (`child.fk IN (SELECT pk FROM parent …)`) so PK types never marshal and a million children cost one statement; one shared timestamp giving restore a precise identity ("a child that was *independently* trashed last week stays trashed"); children-first delete / deepest-first restore with the ordering constraint documented as the reason; a boot check for non-soft-delete cascade targets; explicit refusal to hard-delete a child rather than making the parent's soft delete irreversible.
- **`secrets.rs`'s hard-deny rationale is written down and load-bearing**: `HARD_DENIED_FIELDS` with the asymmetry spelled out ("the failure mode of matching too broadly … is survivable in a way that the failure mode of matching too narrowly … is not"), consumed by `is_secret_column` at every actual serializer redaction site (`dynamic.rs:475`, `aggregate_path.rs:160`), and `Masked`'s seal is enforced on serde, `Encode`, *and* the dynamic path with a dedicated no-reseal regression suite.
- **`validation.rs`'s structured-error ambition is real**: `classify_sql_error` lifts the offending value into the response, SQLite message parsing recovers the column (`sqlite_columns_from_message`), and the in-tx FK check exists specifically so a nested create can see an uncommitted parent — with the visibility rationale documented on both the function and its callers.
- **The refusal surface makes the easy path the safe path.** `UnsafeAlter` blocks NOT NULL adds without default at diff time with three actionable options (`migrate.rs:5226-5236`), plus NOT NULL tightening, UNIQUE adds, unsafe casts and length narrowing; and rename detection is fail-closed (`AmbiguousRename`) unless `UMBRAL_MIGRATIONS_ASSUME_RENAMES` opts in. This is the product's core promise, implemented.
- **The SQLite table-recreation dance is genuinely careful.** It re-creates indexes and `unique_together` (audit_2 #10 held), backfills NOT NULL+default columns, shapes `new_columns` from the **previous** table so a combined alter+add/drop can never reference a missing column (audit_2 H21 held), and the same helper is reused by the constrained-column `DropColumn` path.
- **Constraint-aware drop routing** (`drop_requires_rebuild_sqlite`, `migrate.rs:5267-5279`): SQLite's exact rules (PK / UNIQUE / FK-with-constraint / indexed / in a `unique_together` or index group) encoded once, with the tracker id in the comment.
- **backup.rs's typed binding and ordering.** Per-`SqlType` encode/decode with loud `TypeMismatch`/`UnknownColumn` errors, FK-topological load order with per-table transactions, `quoted_ident` quote-doubling everywhere, and a verified PG sequence reset (`backup.rs:402-436`).
- **transfer.rs's copy core is sound.** FK-level parallelism with per-table tasks, keyset pagination (not OFFSET), a per-batch transactional checkpoint atomically with the data (`upsert_state_in_tx`), M2M junction folding, and cyclic/self-FK deferral — the defects found are in the state/sequence plumbing, not the algorithm.
- **inspectdb's framework-aware layer** recovers FK/unique/index/auto_now/default metadata (Django app-prefix strip + `auth_user` externalization, Rails HABTM, Prisma implicit M2M), and the tests are real-DB round-trips rather than mocks.
- **Every security-critical fix from review_3 / audit_2 that the web layer owns is present and correct in this tree.** CORS scoping is segment-boundary-correct (`cors.rs:369-374` `path_under_prefix` requires the next char to be `/`, with the rationale comment at `:366-368` and a dedicated test at `:383`); the host guard rejects before every downstream layer and supports the HTTP/2 authority form (`hosts.rs:53-62, 100-107`); 5xx bodies are blanked outside dev (`errors.rs:645-647`, "WEB-5 safe-by-default"); JSON 500s pass through un-rendered (`errors.rs:536-540`); slash redirects refuse protocol-relative paths (`slash.rs:279` test); multipart carries a 32 MiB default cap (`web/multipart.rs:34-40`).
- **The middleware onion is small and legible.** `middleware.rs` is one `run_stack` function with a `ran` counter and a comment per branch; `Middleware` is dyn-compatible with defaulted pass-through hooks, and the typed `MiddlewareRegistry` (feature #68) coexists with opaque `wrap_router` layers — the right partition of what can and cannot be introspected.
- **Error rendering is centralized with the right guards.** `render_500_middleware` skips already-HTML and JSON responses before touching anything (`errors.rs:522-540`), and `render_error_middleware` keeps `Accept: application/json` bodies raw — the "one surface, two formats, don't clobber either" split is handled once, not per-handler.
- **`api_error.rs` never leaks internals by accident.** `Database`/`Internal` log the cause and serialize an opaque `"internal server error"`, while `Unauthorized`/`Forbidden`/`TooManyRequests` pass developer-authored text — safe by default, with the escape hatch explicit and four tests.
- **Static publishing is careful.** `collect_into_with` writes a hashed manifest as a `BTreeMap`, `copy_tree` refuses symlinks that escape the source root, collisions are errors, and the suite covers traversal, dev-vs-prod serving, and idempotent re-publish (`static_files.rs`, read in full).
- **The slash probe is defensively designed.** Header-less GET, pre-layer snapshot, `//` refusal, 308 to preserve method/body, and a CRLF canary — the sharp edges were thought about, which is why CORE-47 (missing regression test) is worth closing rather than the code being wrong.
- **`block_on_ready` (`plugin.rs:79-130`) is a model bridge**: three runtime-flavor branches (multi-thread → `block_in_place`, current-thread → `std::thread::scope` + local runtime, no-runtime → fresh runtime), each documented with *why*, a scoped-thread `expect` whose panic cannot be a poisoned-mutex false positive, and a real behavioural test in `plugins/umbral-rls/tests/on_ready_runtime.rs`. This is the hardest piece of the runtime layer and it is right.
- **The boot-check catalogue argues its own severity**: every `SystemCheckFinding` carries a `check_id`, a tailored `message` explaining the failure mode in user terms, and an actionable `hint` ("make it nullable: `{}: Option<ForeignKey<AuthUser>>`") — and the dangerous ones (`settings.allowed_hosts` wildcard, SQLite-in-Prod, `plugin.security_missing`) are deliberately Warnings with the rationale in the doc block. Five dedicated test files (`system_check*.rs`) pin them.
- **The settings security policy is verifiably correct where history says it went wrong**: `client_ip` anchors XFF resolution at the configured hop count from the right (trusting nothing at `hops == 0`) and `trusted_proxy_hops` defaults to 0 — the audit_2 H9 fix holds; the `Debug` redaction masks `secret_key`, URL userinfo and `extra` (with the one omission in CORE-53).
- **The RLS/session-var reset gate holds**: `SESSION_VARS_IN_USE` is written with `Ordering::Release` (`db.rs:601`) and read with `Ordering::Acquire` (`db.rs:608`), and the reset is scoped to umbral's own GUC names (`UMBRAL_GUC_NAMES`, gaps4 #16) instead of `RESET ALL` — the two historic bugs in this exact code are both fixed, not patched.
- **The codegen seam declines instead of guessing**: `write_new_file` never overwrites, `declare_module`/`insert_before_marker` refuse when the file isn't the expected shape, `resolve_target` validates plugin names before `Path::join`, and 16 inline tests exercise each decline path — CORE-29/CORE-54 are gaps in coverage, not in the design.
- **cli.rs's precedence rules are single-sourced and logged**: reserved-name shadowing emits an `eprintln` when a plugin command is shadowed by a built-in, `CommandSet` is built once per dispatch, and `render_help` merges built-ins + plugin commands into one catalogue.
- **The test suite is behavioral, not decorative.** `soft_delete_cascade.rs` asserts live-row counts and independent-delete survival through the public ORM; `router_m2m_junction.rs` drives the free-fn entry and counts router hits; `masked_no_reseal.rs` exists as an explicit regression for the review_3 cluster; signal tests pin panic-during-emit semantics; migrate/backup suites pin the refusal surface with real on-disk round-trips and PG-gated variants; the static-file and system-check suites cover their adversarial cases.

## Clean areas checked

- **Bound parameters and identifiers, crate-wide**: every query/write terminal on both backends passes values via `build_sqlx` bind lists; no `format!`-interpolated user value found in `queryset/**`, `write.rs`, `write_helpers.rs`, or `dynamic.rs`'s update/delete seams (identifier interpolation is `Alias`/`quote_ident` only; `search.rs`'s `kind` literal is single-quote-escaped and author-constant); every `format!`-built DDL/DML statement in migrate/inspect/backup/transfer quotes identifiers and doubles embedded quotes.
- **`unsafe` audit**: zero `unsafe` blocks in the crate's reviewed regions — the six matches in the migrate/transfer/inspect files are comments, message strings, or the keyword list (`migrate.rs:1910/3775/4926/6129/7409`, `inspect.rs:1881`), and the runtime slice has only the keyword string in `codegen.rs:173`. No FFI, no raw pointers.
- **Panics on the request path**: the reachable `expect`s are documented boot/registration guards (`pool_for_dispatched` before `App::build`, `backend::init` twice, `settings` unset, `expect("request present …")` on an invariant maintained two lines up) with "did you call App::build()?" messages; CORE-41 is the one exception reachable from user code. In the data files the `expect`s are backend-tag invariants inside match arms (`tx.as_sqlite_mut().expect("sqlite backend")`, `transfer.rs:955`) that cannot fire while the tag and the arm agree.
- **`only()` guards**: present on `fetch`/`first` and enforced with a structured error (`queryset/mod.rs:2071`, `:2288-2290`) — the `_pg` terminals reuse the same error via `guard_pg_terminal_unsupported` (`:1646-1649`); the only unguarded reader is `try_for_each` (CORE-32).
- **`into_subquery` / `union`/`intersect`/`except`/`combine` backend resolution**: `subquery_backend()` resolves explicit-pool → ambient → `"sqlite"` fallback (`:1669-1681`) and `combine` clears predicates before building both arms (`:1425-1440`), so the review_3 "backend-correct into_subquery/combine" fix is intact.
- **`fetch_annotated`/`aggregate` source-type resolution**: the *lookup* (`meta.fields`, registry) is correct and unknown columns are rejected up front — the bug in CORE-1 is in the PG return-type assumption, not the plumbing.
- **LIMIT/OFFSET and paging**: `try_for_each` honors a caller `.limit(n)` as a total cap and a caller `.offset(n)` as the start (`:2228-2240`), `first()` routes through `fetch()` so it hydrates (review-#2 comment, `:2291-2302`), and the `filter_annotation` LIMIT re-application (gaps6 #3) is present in the terminals.
- **Signal redaction on the delete path**: `SIGNAL_SKIP_FIELDS` stripping happens on the full-row payload and the pk-only payloads carry only the pk (`:3921-3944`) — checked against the gaps6 #14 CRITICAL follow-up; the comment at `:3923-3929` documents the leak that was closed.
- **`M2M`/`ReverseSet` builder invalidation**: every builder that could stale the prefetch cache clears `self.prefetched` (`for_update_skip_locked` `:1100`, `on`/`on_pg` `:1111,:1122`, `prefetch_related` `:1337-1351`), so the `Some`-means-safe invariant in `fetch` (`:2058-2070`) holds.
- **`expr.rs`**: `FExpr`/`Q` are pure builders over `SimpleExpr` with no panics, no string formatting of values, and `eq_f`/`ne_f` implemented per column type including `StrCol` — clean and small.
- **`search.rs` escaping**: SQLite `escape_like` escapes `\`, `%`, `_` and every `LIKE` declares `ESCAPE '\'` (`:178-209`); PG uses `websearch_to_tsquery` (no `to_tsquery` metacharacter injection) and `::float8` on `ts_rank` so the `f64` decode matches.
- **`write.rs` type mapping**: `json_to_sea_value`'s FK arm resolves through `fk_target_pk` (gaps2 #42 lineage) and the timestamptz DST checks (gaps3 #42) are present; `is_default_pk` documents the per-type sentinels (`write.rs:1378`).
- **`signals.rs` registry**: poison recovery, lock-not-held-across-await, sync/async panic isolation, timeout, regression test — read in full; sound (the one dropped candidate — "timeout counts as delivered" — is the tested, documented contract).
- **`audit.rs`**: savepoint-isolated `record_in_tx` (gaps6 #18 shipped), best-effort posture documented per-context rather than blanket-swallowed — sound.
- **`masked.rs` + `secrets.rs` trust boundaries**: seal on serde/`Encode`/dynamic path, key-parse errors typed, no re-seal on restore (`presealed`), denylist rationale — sound.
- **GIS gating**: postgis is feature-gated (compile) + boot-checked + per-backend arms with SQLite skip-with-warn per CLAUDE.md's allowed-exception rule — sound.
- **`nested.rs`**: two-pass validation (FK injection + hidden-field strip + depth/node caps) — no defect found beyond the shared validators reported above.
- **`choices.rs` + `forms_runtime::validate_choice_member`**: compile-time VALUES/LABELS pairing, nullable handling explicit — sound; the CSV storage gap is CORE-36.
- **`reverse_set.rs` / `one_to_one.rs` / `foreign_key.rs` accessors**: unset-state returns empty/`None` (no silent widening — documented on `M2M::query`'s impossible predicate too); `ForeignKey::resolve`/`resolve_pg` take an explicit pool, so no router promise is broken — sound (naming nit is CORE-35).
- **`validators.rs`**: the pure validators run identically from the dynamic and nested paths (pass-1); write-path wiring gaps are tracker items (#22), not new defects.
- **`relation.rs` single-hop paths**: `terminal_queryset()` → `Manager` (router-routed, soft-delete scoped) — sound; only the multi-hop pool fallback is CORE-18.
- **`model.rs` trait machinery**: `set_m2m_parent_ids` default + macro-shadow design, `FkAction` boot-check posture (SetNull's lack of a boot check is honestly documented at `:1098-1101`) — sound beyond the stale docs in CORE-35.
- **Template escaping**: autoescape on for `.html`/`.htm`, `render_str` as the explicit raw path, `safe_url` allowlists schemes against `javascript:`-style hrefs (`templates.rs:443-453`), the `img` filter escapes URL/alt into a `from_safe_string` wrapper (`:345-379`), attribute-context escaping present — no unescaped-interpolation hole found; `safe_string` misuse sites grepped.
- **CSRF (core side)**: the task-local `CURRENT_CSRF` + escaped `csrf_input` helper are correct; the middleware itself lives in `plugins/umbral-security` (see Out-of-scope notes).
- **Host-guard semantics**: prod-only enforcement, `"*"` disable, leading-dot subdomain patterns, IPv6 brackets and ports stripped, HTML-escaped 400 page — all as documented (the trailing-dot case is CORE-45).
- **Slash-redirect protocol safety**: `alternate_path` refuses protocol-relative paths; backslash cannot reach a `Location` header because `http::Uri` rejects it (`http-1.4.1` `URI_CHARS[0x5C] == 0`, and hyper parses request targets via `Uri::from_maybe_shared`), so the "`\` acts as `/`" open-redirect hypothesis is refuted, not merely unfound; 308 preserves method/body.
- **Body/size limits**: `RequestBodyLimitLayer` + `TimeoutLayer` wrap every request including host-guard rejections (`app.rs:1913-1927`); multipart enforces per-part and total caps; `pagination.rs` clamps page size (no unbounded list default).
- **Static file serving**: traversal and escaping-symlink refusal, content-type gating, hashed-vs-dev split, collision errors — adversarial cases are in the suite.
- **`web.rs` / `api_error.rs` / `routes.rs`**: thin plumbing (axum re-exports, `API_BASE` first-write-wins `OnceLock`, error shapes, route registration); no logic to get wrong, no findings.
- **`app.rs::ready()`**: hooks run exactly once, `on_ready` failures are typed (`BuildError::PluginOnReady`) and propagated; the pre-hook `ready_fired` flag is the known boot-lifecycle item below, not a new finding.
- **Tracking/apply idempotency**: drop/readd pairs use `IF EXISTS` (`migrate.rs:6231-6233`), M2M/table DDL uses `IF NOT EXISTS` on drops/recreates, apply is transactional per migration file, and the PG advisory lock is deterministic.
- **Previously-fixed items verified held**: squash-aware drift detection; `UNIQUE` on ADD COLUMN → separate `CREATE UNIQUE INDEX` (`migrate.rs:5588-5599`); backup.rs alias routing; fixtures' `load_fixture` uses `.presealed()` (`fixtures.rs`, review_3's Masked double-seal fix); rename detection fails closed; `DropColumn` of a constrained column still routes through the dance.
- **fixtures.rs**: full read (216 lines) — no panics, typed `FixtureError`, Masked round-trip pinned by review_3's fix; no findings.
- **Ambient-state inventory (runtime)**: every process-wide static enumerated — `SETTINGS`, `POOLS`, `DYNAMIC_POOLS`, `ATOMIC_DEFAULT`, `SESSION_VARS_IN_USE`, `UMBRAL_GUC_NAMES`, `ROUTER`/`DEFAULT`, `ACTIVE` (backend, `backend.rs:384`), `AMBIENT` (cache), `STORAGES`, `DEFAULT_AUTH`, `DRAINING`, `MEM_SEQ`, `WARNED` (timezone) — the ambient-config class is the tracked by-design decision; no *new kind* of global (no hidden thread-locals, `static mut`, or cross-request caches) crept in beyond it. `rt.rs` and `shutdown.rs` hold no state of their own.
- **Transaction correctness**: pending signals fire only after a successful commit and are dropped on rollback in all four runners (gaps6 #15 semantics); `commit` failure propagates via `?` before any signal fires.
- **`ttl`/window arithmetic in `ratelimit.rs`**: window pruning, `retry_after`, `remaining`, and the `sweep` are consistent with the six behavioural tests (which, however, miss the sweep — CORE-51).
- **Route-context isolation**: `ROUTE_CONTEXT` is a task-local; `current()` never leaks a previous request's context across tasks, and `tests/route_context.rs` proves the handler-visible tenant via the public path.
- **`validate.rs`**: small, pure combinator surface; no panics, exhaustive-match style kept.
- **`storage.rs` registry**: `STORAGES` set-once with a panic message naming the plugin (`StoragePlugin`) — the register-once contract is explicit.
- **`auth_contract.rs`**: `DEFAULT_AUTH` warns and ignores a second set instead of panicking midway through the plugin list (deliberate); `parse_basic_credentials` handles the header shapes without panicking on malformed input.
- **codegen file surgery**: CRLF/trailing-newline preservation tested (`edits_preserve_crlf_and_a_missing_trailing_newline`), path-escape refusal tested (`resolve_target_refuses_a_plugin_name_that_escapes_the_project`).
- **Trusted-proxy env parsing**: `UMBRAL_TRUSTED_PROXY_HOPS` + `client_ip_with_hops` boundaries (0, n > list length, padded entries) verified against the doc table at `settings.rs:150-156`.
- **Tests**: structurally behavioral — real on-disk dump/load round-trips (`backup.rs`, `backup_json.rs`, `backup_fk_order.rs`, PG-gated `backup_postgres.rs`), the refusal surface pinned (`rename_detection`, NOT NULL backfill, `gap70_drop_unique_column_sqlite`, `varchar_length_alter`, `migration_safety`), transfer SQLite→SQLite end to end. Gaps the findings expose: no PG-target **rerun** test (CORE-5), no db_constraint-flip diff test (CORE-6), no FK→BigInt transition test (CORE-21), no collision/non-ident codegen test (CORE-22/CORE-23), no pk-add test (CORE-24).

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #21` — stale code doc-comments batch `[open]`. Verified in this sweep: its scope list (`app.rs` incl. `:522-524`, `umbral-realtime`, `umbral-signals`, `umbral-storage`, `umbral-tasks`) does **not** include `queryset/mod.rs:6089-6096` (CORE-31), `model.rs:195`/`:1150-1160` (CORE-35), or `app.rs:916-1041` / `cors.rs:33-34` (CORE-42/CORE-43) — those stay numbered.
- `gaps6 #22` — `FieldSpec::max_length` is emitted as `varchar(N)` but validated by no write path (validation.rs area; insert/update seams) `[open]`.
- `gaps6 #23` — `cleaners::apply` never fires on the typed `update_values` path (`queryset/mod.rs:3650`) `[open]`; CORE-11 adds its sibling sites (`save` UPDATE, `bulk_update`).
- `gaps6 #24` — `request_timeout` is global-only; one slow route forces the whole app's timeout up `[open]`.
- `gaps6 #8` — RuntimeProfile / the runtime handle for plugin context `[open]`; CORE-48's `ctx.runtime()` doc reference points here.
- `gaps6 #17` (residual) — soft-deleting a materialized source row fires only `bulk_post_delete`; that path doesn't recompute `[open]`.
- `gaps4 #24` / `review_3/codex/23_orm_postgres_specific_terminals_drift_from_generic_paths.md` — `_pg` terminals are low-level escape hatches `[open]` (partial: read-side guards shipped; `create_pg`'s missing M2M/audit/signal lifecycle is the still-open half). `upsert` and `save` are **not** in this entry — they are not `_pg` terminals.
- `review_3/codex/26_orm_try_for_each_offset_pagination.md` — try_for_each offset pagination under concurrent writes (deferred, documented) `[open]`. CORE-32 is a different aspect of the same terminal.
- `review_3/codex/24_orm_raw_sql_escape_hatch_lacks_bind_and_write_routing.md` — raw-SQL escape hatch (`raw`/`raw_with`) lacks bind/write routing `[open]`; the `raw` terminals live in the query layer but were only read at their call sites.
- `audit_2/findings/core-migrate.md #2` — combined alter+add/drop on SQLite `[open]` (deferred; the H21 shape-snapshot half is fixed and held).
- `audit_2 #8` — `seq`-from-count, `depends_on` unenforced, alias-aware drift `[open]` (deferred).
- `audit_2 #9` — `snapshot_hash` recorded but never verified `[open]` (deferred).
- `audit_2 #11` — inspectdb constraint introspection `[open]` (deferred; FK/unique/index recovery landed later, CHECK constraints still absent).
- `audit_2 #12` — streaming dump/load `[open]` (deferred).
- `audit_2 #13`/`#16` — check.rs catalogue / `is_loopback_bind` `[open]` (deferred).
- `audit_2 #15` — inspectdb raw field idents + composite PKs `[open]` (deferred; field-ident half since fixed by `safe_field_ident`).
- `audit_2 #17` — alias-aware `record_applied` `[open]` (deferred).
- `review_3/claude/02_triage.md` — PG per-file apply dedup, `migrate.rs:2592` `[open]` (deferred).
- `review_3/claude/02_triage.md` — tracking-DDL dedup, `migrate.rs:2940` `[open]` (deferred).
- `planning/gaps5.md:410` (review_3 triage closeout) `[open]` — fixed: CORS segment-boundary matching, JSON-500 passthrough, prod error-body blanking, `safe_url` scheme allowlist; deferred there and since landed in the tree: slash-redirect probe snapshot taken after the middleware stack (`app.rs:1794-1800`); still deferred: pure code-dedup incl. the `html_escape` ×3 duplication in `templates.rs`, plus assorted doc-drift.
- `planning/audit_2/findings/core-app-config.md:21` (entry 7, "deferred: boot-lifecycle redesign") `[open]` — a failed `build()` has already published settings/pools/backend into panicking `OnceLock`s. Adjacent and folded here rather than numbered: `ready()` sets `ready_fired` before the `on_ready` loop (`app.rs:103` vs `:110-117`), so a failed hook leaves the process reporting ready.
- `audit_2 core-web #1` — `plugin.security_missing` Warning-vs-Error and default security headers `[open]` (deferred design call; CORE-59 only flags the name-matching coupling).
- `gaps2 #88` — M2M junction routing through `DatabaseRouter`, shipped for the free-fns only `[fixed/verified held]` — the frame for CORE-18.
- `gaps2 #48` — `validate_multi_fk_exists` DB-error honesty, shipped `[fixed/verified held]` — the frame for CORE-33.
- `gaps2 #22` — `db_constraint` cross-DB FK opt-out (shipped; `planning/archive/gaps2-done.md:553`) `[fixed/verified held]`. Its "flipping the flag tears down a prior constraint" claim does not hold in the code — the specific defect is CORE-6 (kept numbered; a class-level tracker entry does not absorb it).
- `gaps2 #54` — unified CLI help/unknown-command catalogue `[fixed/verified held]`; CORE-30 is a refinement of its error mapping, not a re-report.
- `gaps2 #90` — `umbral-auth::throttle` consolidated onto `ratelimit::RateLimiter` `[fixed/verified held]`; CORE-51's doc drift postdates it.
- `gaps3 #81`/`#82` — the `umbral::codegen` seam + scaffolder `[fixed/verified held]`; CORE-29/CORE-54 are refinements.
- `gaps4 #16` — `RESET ALL` scoped to umbral GUC names `[fixed/verified held]` (verified holding above).
- `gaps4 #44` — builder-order documentation/behaviour `[fixed/verified held]`.
- `gaps4 #50` — slash-redirect as a layer over every 404 `[fixed/verified held]` (`app.rs:1794-1800` is that fix's own comment).
- `gaps5 #102` / TaskFlow #323 — middleware ordering/registry contract `[fixed/verified held]` (resolved by `MiddlewareRegistry`, feature #68; wired at `app.rs:1791`, tested by `tests/middleware_pipeline.rs` + `tests/middleware_registry.rs`).
- `gaps6 #15` — buffered signals dropped on rollback `[fixed/verified held]` (the semantics are correct and cited in the code comments — CORE-26 is about the copy drift, not the semantics).
- `audit_2 H9` — client-IP/trusted-proxy policy `[fixed/verified held]` (verified holding above).
- `audit_2 core-app-config #11` — redacting `Debug` for `Settings` `[fixed/verified held]`; CORE-53 is the residual field omission inside that fix.
- `audit_2/AUDIT_REPORT.md H23` (+ `#4` superseded) — fail-closed rename heuristic `[fixed/verified held]`.
- `audit_2 #1,#3,#5,#6,#7,#10,#14,#18` — fk_target_pk re-add, backup topo+tx+seq, NOT NULL backfill, destructive gate, advisory lock, dance index re-creation, identifier escaping, duplicate-table detection `[fixed/verified held]` (shipped; spot-verified held).
- `review_3/claude/02_triage.md` — fixtures restore Masked double-seal `[fixed/verified held]` (`.presealed()`, verified held).
- `review_3/claude/02_triage.md` — backup.rs alias routing `[fixed/verified held]`.
- `review_3/claude/02_triage.md` — squash-aware drift detection `[fixed/verified held]`.
- `review_3/claude/02_triage.md` — unique ADD COLUMN on SQLite `[fixed/verified held]`.
- `review_3` file_field LOW (IDOR hardening) — "FIXED: FileField Deserialize rejects absolute/../control-char keys" `[fixed/verified held]` — CORE-4 is the residual seam on the path that fix did not cover.
- `review_3/codex #14` — process-wide `OnceLock` ambient state `[fixed/verified held]` (by design; covers POOLS/DYNAMIC_POOLS/ATOMIC_DEFAULT/SETTINGS/ROUTER/DEFAULT/AMBIENT/STORAGES/DEFAULT_AUTH/ACTIVE/DRAINING and the rest; `WARNED` in CORE-28 is bookkeeping, not ambient config, which is why it is numbered).

## Out-of-scope notes

Observations that belong to *other* crates (or to files outside this review's numbered scope) encountered while reading this one. One line each.

- `plugins/umbral-sessions/src/store.rs` — `DbStore::save` routes through `Session::objects().upsert()` (review_3 fix, `planning/review_3/claude/02_triage.md:219`), so the session store inherits `upsert`'s lifecycle gaps (CORE-2/CORE-9/CORE-10); nothing to change in the plugin today, but the session row writes fire no signals for a reason worth knowing. `[→ umbral-sessions]`
- `plugins/umbral-storage/src/lib.rs:1127-1175` (non-FS proxy `retrieve_stream(&key)`) and `plugins/umbral-storage/src/media.rs:233` (`path_for = dir.join(key)`) — verify traversal containment: this is CORE-4's operative guard on non-FS backends (FS backends use ServeDir + the SymlinkGuard in `static_serve.rs`). `[→ umbral-storage]`
- `plugins/umbral-storage` — stored/sniffed `content_type` values exist there (`media.rs` `sniff_content_type`, `neutralised_upload`); if any consumer feeds a stored value into `StreamingResponse::content_type`, CORE-41 moves from latent to live — worth one check in that review. `[→ umbral-storage]`
- `plugins/umbral-rest/*`, `plugins/umbral-admin/*` — whether any write path deserializes into the typed model (serde guard fires) or uses `DynQuerySet` JSON (guard absent): CORE-4's reachability per surface. `[→ umbral-rest, umbral-admin]`
- `plugins/umbral-security` — the CSRF middleware implementation lives there (core side verified clean); route CSRF questions there. `[→ umbral-security]`
- `plugins/umbral-openapi/src/lib.rs:562` (`properties.insert(col.name.clone(), schema)`) and `plugins/umbral-openapi/src/client_gen.rs:1007` (`{}: {}{null};", f.name`) — the same SQL-name-as-JSON-key assumption as CORE-7; the openapi/rest review should confirm the generated schema/client keys for `#[sqlx(rename)]` models. `[→ umbral-openapi]`
- `crates/umbral-core/src/app.rs:1513-1517` — comment "BUG-20: … multiple plugins contributing the same path produce duplicate entries; umbral-openapi's merge step picks the first" — for the openapi/plugin reviews to cross-check. `[→ umbral-openapi]`
- `crates/umbral-cli/src/lib.rs:531-545` + `:740-745` — the built-in layer maps `UnknownArgument`/`InvalidValue` to the "unknown command" catalogue via `unknown_token`'s first-non-flag heuristic (CORE-30's twin). `[→ umbral-cli]`
- `crates/umbral-macros/src/lib.rs:1564-1583` — PK resolution is first-match; a second `#[umbral(primary_key)]` field is silently not the pk (reachability context for CORE-24). `[→ umbral-macros]`
- `plugins/umbral-tenants/src/lib.rs:538` — the live `register_tenant_pool` caller that makes CORE-27 observable; a tenants-side shutdown hook would be the other half of the fix. `[→ umbral-tenants]`
- `plugins/umbral-auth` — `crates/umbral-core/src/auth_contract.rs`'s `FnAuthentication` doc example calls `umbral_auth::authenticate(&user, &pass)` (cannot be verified from core); confirm the symbol exists. `[→ umbral-auth]`
- `planning/review_4/umbral-casing.md` CASING-1 — the casing-crate half of CORE-23 (`pascal_case_from_table` can return non-identifiers); CORE-23 files only the inspect.rs emission/validation half, no double-count. `[→ umbral-casing]`
- `documentation/docs/v0.0.1/orm/database-routing.mdx:114-120` — "the migration emits no physical `REFERENCES`" needs the pre-existing-FK caveat once CORE-6 is fixed. `[→ documentation]`
- `planning/archive/gaps2-done.md:553` — #22's write-up asserts the flag flip tears down the constraint; needs the CORE-6 correction when the fix lands. `[→ planning]`
- `crates/umbral-core/src/orm/queryset/mod.rs:4956-4960` — typed create flushes pending M2M post-commit, outside the atomic hook window (the code's own "#92 case" comment says so). `[→ umbral-core]`
- `crates/umbral-core/src/migrate.rs:1376-1405` — the deterministic `uniq_`/`idx_` naming contract and inline single-column UNIQUE (which PG auto-names `<table>_<col>_key`) that CORE-17's parser assumes. `[→ umbral-core]`
- `crates/umbral-core/src/orm/dynamic.rs:2323,2536` — the `insert_json` vs `insert_json_in_tx` entry points that select CORE-15's divergent checks. `[→ umbral-core]`
- `crates/umbral-core/src/app.rs:1364` — the cross-DB FK boot guard reads `field.db_constraint`; under CORE-6 the guard's view and the DB's actual constraints can disagree. `[→ umbral-core]`
- `crates/umbral-core/src/orm/model.rs:565-567` — `FieldSpec.name`'s doc claims "there is no field-level column-rename attribute", contradicting the shipped `#[sqlx(rename)]` path (stale doc, same root as CORE-7). `[→ umbral-core]`
- `crates/umbral-core/src/inspect.rs:343` (and `backup.rs:186`) — direct `pool_dispatched()` behind a per-model alias resolution (registry-less fallback); deliberate, confirmed by the review_3 alias-routing fix. `[→ umbral-core]`
- `crates/umbral-core/src/orm/write.rs:726` — the only in-tree `active_tz()`/`tz_or_utc` consumer (one configured settings string); relevant context for CORE-28's fix. `[→ umbral-core]`

## Slice-to-final ID map

| Original | Final | Note |
|---|---|---|
| QW-1 | CORE-1 | spot-verified |
| QW-2 | CORE-2 | spot-verified |
| QW-3 | CORE-8 | |
| QW-4 | CORE-9 | |
| QW-5 | CORE-10 | |
| QW-6 | CORE-11 | |
| QW-7 | CORE-12 | |
| QW-8 | CORE-13 | |
| QW-9 | CORE-14 | |
| QW-10 | CORE-31 | |
| QW-11 | CORE-32 | |
| MD-1 | CORE-3 | spot-verified |
| MD-2 | CORE-4 | spot-verified |
| MD-3 | CORE-15 | |
| MD-4 | CORE-16 | |
| MD-5 | CORE-17 | |
| MD-6 | CORE-18 | absorbed the m2m router-bypass out-of-scope note (citations combined) |
| MD-7 | CORE-19 | |
| MD-8 | CORE-20 | |
| MD-9 | CORE-33 | |
| MD-10 | CORE-34 | |
| MD-11 | CORE-35 | |
| MD-12 | CORE-36 | |
| MD-13 | CORE-57 | |
| MIG-1 | CORE-5 | spot-verified |
| MIG-2 | CORE-6 | spot-verified |
| MIG-3 | CORE-21 | |
| MIG-4 | CORE-22 | |
| MIG-5 | CORE-23 | absorbed the inspect.rs struct-name out-of-scope note |
| MIG-6 | CORE-24 | |
| MIG-7 | CORE-37 | |
| MIG-8 | CORE-38 | |
| MIG-9 | CORE-39 | |
| MIG-10 | CORE-58 | |
| WEBL-1 | CORE-25 | absorbed the check.rs check-id routing note |
| WEBL-2 | CORE-40 | |
| WEBL-3 | CORE-41 | |
| WEBL-4 | CORE-42 | |
| WEBL-5 | CORE-43 | |
| WEBL-6 | CORE-44 | |
| WEBL-7 | CORE-45 | |
| WEBL-8 | CORE-46 | |
| WEBL-9 | CORE-47 | |
| RUN-1 | CORE-7 | spot-verified |
| RUN-2 | CORE-26 | |
| RUN-3 | CORE-27 | |
| RUN-4 | CORE-28 | |
| RUN-5 | CORE-29 | |
| RUN-6 | CORE-30 | |
| RUN-7 | CORE-48 | |
| RUN-8 | CORE-49 | |
| RUN-9 | CORE-50 | |
| RUN-10 | CORE-51 | |
| RUN-11 | CORE-52 | |
| RUN-12 | CORE-53 | |
| RUN-13 | CORE-54 | |
| RUN-14 | CORE-55 | |
| RUN-15 | CORE-56 | |
| RUN-16 | CORE-59 | |
| RUN-17 | CORE-60 | |
