# review_4 · umbral-core (core-1: query/write engine slice)

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 21,827 LOC — src-only slice: `crates/umbral-core/src/orm/queryset/**`, `orm/dynamic.rs`, `orm/column.rs`, `orm/write.rs`, `orm/expr.rs`, `orm/search.rs`, `orm/aggregate.rs` (crate src total 69,759; crate tests 61,055) |
| **Reviewer** | core-1 (query/write engine) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 7 medium · 2 low · 0 info — 11 new, 6 known |

## Summary

- This slice is the ORM's execution engine: the `QuerySet`/`Manager` builder→SQL→decode pipeline on both backends, the typed write terminals (`create`/`bulk_create`/`save`/`upsert`/`bulk_update`/`delete_instance`), the tx-bound writer/reader, and the small support modules (`write.rs`, `expr.rs`, `search.rs`, `aggregate.rs`).
- The **query** half is in very good shape: every value is bound, every identifier is an `Alias`, the SQLite/Postgres decode twins agree, and the heavily-drifted surfaces (join decode, signal redaction, audit snapshot) have clearly been through previous sweeps.
- The **write** half is where the two HIGH findings live: (1) Postgres `SUM`/`AVG` decode assumes the model's declared column type is the SQL result type, but PG widens (`sum(int2|int4)→bigint`, `sum(int8)/avg(int)→numeric`) — every `aggregate`/`annotate` SUM/AVG over an integer column on PG fails at runtime with a `ColumnDecode` mismatch, and no PG test covers it; (2) the per-row write terminals added after `create()` (`save`, `upsert`) never run the M2M seeding that `create()` documents as load-bearing (BUG-16), so M2M writes on rows they return are documented silent no-ops.
- The dominant **drift** shape in this slice: one semantic implemented in three places and fixed in two. `create()` got the full lifecycle (validation, VIEW guard, M2M seeding, signals, audit); `save()`/`upsert()`/`bulk_update()` re-implement the same SQL without the guards. The typed UPDATE path exists three times (`build_update_for`, `save`'s inline loop, `bulk_update`) and each disagrees on normalization/auto-timestamps; the typed DELETE path exists twice (`delete`, `delete_instance`) and disagrees on audit and signal gating.
- The audit ledger loses writes through `save()`'s UPDATE branch and `delete_instance` while the docs promise `#[umbral(audited)]` "records every write"; and the audit argument block serializes the row twice *before* the `audited` flag is checked, so unaudited models do pay (contradicting `audit.rs`'s "Models without `#[umbral(audited)]` pay nothing").
- Fix order for a reader: QW-1 (PG aggregates are broken in production for integer columns), QW-2 (silent M2M loss), then the write-terminal parity batch QW-3…QW-6, which share one fix: route every typed writer through the shared helpers `create`/`build_update_for` already own.

## Findings

### QW-1 · HIGH · correctness · self-verified
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

### QW-2 · HIGH · correctness · self-verified
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

### QW-3 · MEDIUM · correctness (data loss) · self-verified
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

### QW-4 · MEDIUM · security/correctness (audit + signal accounting) · self-verified
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

### QW-5 · MEDIUM · drift · self-verified
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

### QW-6 · MEDIUM · drift · self-verified
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

### QW-7 · MEDIUM · drift (correctness) · self-verified
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

### QW-8 · MEDIUM · perf/alloc · self-verified
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

### QW-9 · MEDIUM · correctness (multi-database) · self-verified
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

### QW-10 · LOW · docs · self-verified
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
- **Impact**: a consumer reading the doc to choose a write API is told the opposite of the truth: it suggests `objects().create()` is invisible to per-row subscribers (so an app might route creates through `save()` "for the signals" — a path that, per QW-2/QW-5, is the less complete one), and it misdescribes `create()`'s payload shape for anyone writing a `post_save` subscriber. Not covered by gaps6 #21's stale-comment batch (which lists five other files/sites).
- **Fix**: correct the sentence to name only the genuinely bulk paths (`bulk_create`, `update_values`/`update_expr`, `delete`), and note that `create` fires the per-row signal since gaps3 #29.

### QW-11 · LOW · ergonomics/docs · self-verified
**`try_for_each`'s caveat under-documents what it drops: `join_related` is silently ignored (not named in the caveat), `.only()` is unguarded, and every returned row's M2M handles are unattached so `add()` no-ops inside the callback.**
- **Where**: `crates/umbral-core/src/orm/queryset/mod.rs:2203-2208` (caveat), `:2209-2279` (body); guard sites it lacks: `:2071-2073` (only), `:2084` (join validation), `:2110` (join application), `:2119-2124` (hydration).
- **Evidence**: the doc says
  ```
  /// transaction. `select_related` and `prefetch_related` hooks
  /// are NOT applied on each row — `try_for_each` is intentionally
  /// the "raw column data, one row at a time" terminal.
  ```
  (`mod.rs:2205-2208`), but the body calls only `check_annotations` and then `self.build_query_for(...)` per chunk (`:2220`, `:2245`, `:2254`) — `build_query_for` does not apply `join_related` (`apply_join_related` is called separately, by `to_sql`/`to_sql_pg`/`fetch` at `:1796,:1814,:2110`), so `.join_related("author")` adds neither the JOIN nor hydration and does not run `validate_join_related_fields` (so a typo'd field name errors under `fetch()` and is silently ignored here). `only_cols` is never checked, so `.only(&["id"])` falls through to the generic `T::FromRow` decode and surfaces as a driver "column not found" instead of the structured `only_with_typed_terminal_error("first")`-style message `fetch`/`first` give. Rows come from `query_as_with` with no `set_m2m_parent_ids` call, so `row.tags.add(&t)` in the callback is the same unattached-handle no-op as QW-2.
- **Impact**: a caller who chains a relation builder before `try_for_each` (a natural refactor from `fetch()`) gets rows with `ForeignKey::resolved()` empty / M2M writes dropped and no warning, while the doc names only two of the three relation hooks. Boundary case, bounded — the terminal's stated purpose is raw streaming — but the fix is cheap.
- **Fix**: extend the caveat to name `join_related` and M2M handles, and add the same `only_cols`/`join_related` guards the other terminals use (each is two lines) or a `debug_assert`-style refusal.

## Strengths

- **Injection posture is uniform and verifiable.** Every user value in this slice is bound (`sea_query::Value` / `sqlx` bind), every identifier goes through `Alias::new` or `quote_ident` (`search.rs:99-101`), and the one deliberately-verbatim SQL splice (`Searchable::filter_sql`) is constrained by its return type and documented with the threat model at `search.rs:138-145` (matching audit_2 core-orm #8). `Q::not` negates the SQLite-specific condition too, so the two backend arms cannot drift (`expr.rs:301-309`).
- **The signal/redaction contract was clearly hardened and is respected at every site it was fixed for**: per-row `post_delete` payloads strip `SIGNAL_SKIP_FIELDS` before emitting (`mod.rs:3923-3937`), the tx writer buffers the *same* closures the non-tx path uses rather than re-deriving payloads, and the `create`/`update_or_create` signal seams carry comments naming the incident that produced them (gaps3 #29/#14/#15 lineage).
- **The SQLite/Postgres decode twins are structurally symmetric.** `backend_sqlite.rs` and `backend_pg.rs` implement the same four functions (`row_to_json`, `hydrate_joined_rels`, `extract_m2m_child_json`, `pk_to_json`, `decode_agg`) with mirrored control flow, and both read hop tables off the shared `RelPath::from_path` resolver (gaps6 #4) instead of hand-rolling path parsing. XW-1 is the one place the two genuinely disagree, and it disagrees because of a wrong *PG type model*, not a forgotten branch.
- **`implicit_predicates()` is a real single seam.** Soft-delete scoping flows through one trait method consumed by `build_query_for`, `build_update_for`, and the delete paths, so `.with_deleted()`/`.only_deleted()` cannot be honored on one terminal and not its sibling (the review_3-verified `QuerySetTx::delete` hard-delete bug was fixed here).
- **The failure modes are fast and structured where the framework controls them**: unknown columns fail before SQL (`mod.rs:4310-4315`, `:2643-2655`, `:2977-2990`), `check_annotations` exists precisely so the infallible builder path can skip poisoned annotations while fallible terminals fail loudly (`:810-816`), and the `_pg` terminals refuse unsupported chained features with a message that explains why (`:1639-1667`) rather than returning un-hydrated rows.
- **Write-path comments carry their incident history** (gap68, features #83, gaps4 #85, BUG-16, gaps3 #29/#54). This made the drift findings in this file cheap to prove — the comments state the intended invariant next to the code that later violated it, which is exactly what a future fixer needs. Keep this.

## Clean areas checked

- **Bound parameters**: every terminal in this slice (both backends) passes values via `build_sqlx` bind lists; no `format!`-interpolated user value found in `queryset/**`, `write.rs`, `write_helpers.rs`, or `dynamic.rs`'s update/delete seams (identifier interpolation is `Alias`/`quote_ident` only; `search.rs`'s `kind` literal is single-quote-escaped and author-constant).
- **`only()` guards**: present on `fetch`/`first` and enforced with a structured error (`mod.rs:2071`, `:2288-2290`) — the `_pg` terminals reuse the same error via `guard_pg_terminal_unsupported` (`:1646-1649`); the only unguarded reader is `try_for_each` (QW-11).
- **`into_subquery` / `union`/`intersect`/`except`/`combine` backend resolution**: `subquery_backend()` resolves explicit-pool → ambient → `"sqlite"` fallback (`:1669-1681`) and `combine` clears predicates before building both arms (`:1425-1440`), so the review_3 "backend-correct into_subquery/combine" fix is intact.
- **`fetch_annotated`/`aggregate` source-type resolution**: the *lookup* (`meta.fields`, registry) is correct and unknown columns are rejected up front — the bug in QW-1 is in the PG return-type assumption, not the plumbing.
- **LIMIT/OFFSET and paging**: `try_for_each` honors a caller `.limit(n)` as a total cap and a caller `.offset(n)` as the start (`:2228-2240`), `first()` routes through `fetch()` so it hydrates (review-#2 comment, `:2291-2302`), and the `filter_annotation` LIMIT re-application (gaps6 #3) is present in the terminals.
- **Signal redaction on the delete path**: `SIGNAL_SKIP_FIELDS` stripping happens on the full-row payload and the pk-only payloads carry only the pk (`:3921-3944`) — checked against the gaps6 #14 CRITICAL follow-up; the comment at `:3923-3929` documents the leak that was closed.
- **`M2M`/`ReverseSet` builder invalidation**: every builder that could stale the prefetch cache clears `self.prefetched` (`for_update_skip_locked` `:1100`, `on`/`on_pg` `:1111,:1122`, `prefetch_related` `:1337-1351`), so the `Some`-means-safe invariant in `fetch` (`:2058-2070`) holds.
- **`expr.rs`**: `FExpr`/`Q` are pure builders over `SimpleExpr` with no panics, no string formatting of values, and `eq_f`/`ne_f` implemented per column type including `StrCol` — clean and small.
- **`search.rs` escaping**: SQLite `escape_like` escapes `\`, `%`, `_` and every `LIKE` declares `ESCAPE '\'` (`:178-209`); PG uses `websearch_to_tsquery` (no `to_tsquery` metacharacter injection) and `::float8` on `ts_rank` so the `f64` decode matches.
- **`write.rs` type mapping**: `json_to_sea_value`'s FK arm resolves through `fk_target_pk` (gaps2 #42 lineage) and the timestamptz DST checks (gaps3 #42) are present; `is_default_pk` documents the per-type sentinels (`write.rs:1378`).
- **No `unsafe`, no reachable `unwrap`/`expect`** in the reviewed slice on a request path; the three `expect`s in `db.rs`'s `pool_dispatched` are pre-`App::build()` guards with messages naming the fix, and the fallible twin `try_pool_dispatched` is what the fallible callers use (`:216-223`).

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #21` — stale code doc-comments batch (open); verified for this slice: it lists `app.rs`, `umbral-realtime`, `umbral-signals`, `umbral-storage`, `umbral-tasks` — it does **not** include the `mod.rs:6089-6096` claim reported as QW-10.
- `gaps6 #22` — `FieldSpec::max_length` validated by no write path (open); touches this slice's insert/update seams.
- `gaps6 #23` — `cleaners::apply` never fires on the typed `update_values` path (open; names `mod.rs:3650`). QW-6 adds its sibling sites (`save` UPDATE, `bulk_update`) but the `update_values` member is this entry's.
- `gaps4 #24` / `review_3/codex/23_orm_postgres_specific_terminals_drift_from_generic_paths.md` — `_pg` terminals are low-level escape hatches (partial: read-side guards shipped; `create_pg`'s missing M2M/audit/signal lifecycle is the still-open half). `upsert` and `save` are **not** in this entry — they are not `_pg` terminals.
- `review_3/codex/26_orm_try_for_each_offset_pagination.md` — try_for_each offset pagination under concurrent writes (deferred, documented). QW-11 is a different aspect of the same terminal.
- `review_3/codex/24_orm_raw_sql_escape_hatch_lacks_bind_and_write_routing.md` — raw-SQL escape hatch (`raw`/`raw_with`) lacks bind/write routing (open); the `raw` terminals live in this slice but were only read at their call sites.

## Out-of-scope notes

- `crates/umbral-core/src/orm/m2m.rs:223,358,523,581,617,631,893,993,1015` — the M2M helpers call `crate::db::pool_dispatched()` directly (same router-bypass class as QW-9); this file belongs to the models/relations slice, and it partly documents a fallback (`:607,:657,:804`) — worth a parity check there, not restated here.
- `crates/umbral-core/src/backup.rs:186` and `crates/umbral-core/src/inspect.rs:343` — same `pool_dispatched()` text, but each sits behind a per-model alias resolution with the direct call as the registry-less fallback; confirmed deliberate by the review_3 fix note (`planning/review_3/claude/02_triage.md:59`).
- `plugins/umbral-sessions/src/store.rs` — `DbStore::save` routes through `Session::objects().upsert()` (review_3 fix, `planning/review_3/claude/02_triage.md:219`), so the session store inherits `upsert`'s lifecycle gaps (QW-2/QW-4/QW-5); nothing to change in the plugin today, but the sibling plugin reviewer should know why the session row writes fire no signals.
- `crates/umbral-core/src/orm/audit.rs` and `crates/umbral-core/src/orm/validation.rs` — cited by QW-4/QW-5/QW-8 as the contract owners but outside this slice's file list; verify the fixes land there (or in the shared helpers both slices touch).
