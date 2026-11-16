# review_4 · umbral-core (migrate + data movement)

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 16,905 LOC (slice of `crates/umbral-core`: `src/migrate.rs` 8,056 · `src/inspect.rs` 2,634 · `src/backup.rs` 1,431 · `src/transfer.rs` 1,188 · `src/fixtures.rs` 216; + 3,380 in `tests/{migrate*,inspect*,backup*,transfer*,fixtures*}.rs`) |
| **Reviewer** | Claude (core slice 3: migrate + data movement) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 4 medium · 3 low · 1 info — 10 new, 17 known |

Slice file (`_scratch/`): ids are slice-local (`MIG-n`); the assembler renumbers into `umbral-core.md`. LOC are slice-local (the five src files + the tests that exercise them).

## Summary

- This slice is the *product*: the autodetector + renderers + apply/tracking of `migrate.rs`, inspectdb's model generator, `backup`/`load`, `transferdata`, and fixtures — 16.9k LOC around the declare → migrate → change → migrate loop.
- The core loop is in good shape. Every previously-flagged fix I re-checked holds (squash-aware drift, unique-ADD-COLUMN → `CREATE UNIQUE INDEX`, backup alias routing, fixtures' `.presealed()`, fail-closed rename detection), the refusal surface (`UnsafeAlter`, `AmbiguousRename`) is genuinely safe-by-default, and the SQLite table-recreation dance is careful.
- Biggest risk is the data-movement plumbing, not the migration core: **MIG-1** — `transferdata`'s documented resume path is **dead on a Postgres target** (state table declares `INTEGER`, decoded as `i64`; second run aborts before copying). **MIG-2** — flipping `#[umbral(db_constraint = …)]` produces **no diff at all**, so the physical FK is never dropped/added; model and schema silently disagree in both directions.
- Codegen edges: inspectdb can emit non-compiling models for legal table names (MIG-4 collision fallback no-op, MIG-5 unvalidated struct idents / unescaped `#[umbral(table = …)]`; cross-ref CASING-1), and a pk-flagged added column is routed to `AddColumn` — a statement SQLite cannot execute (MIG-6), contradicting the render doc's explicit claim.
- Fix order: MIG-1 (one decode-type change), MIG-2 (two comparison terms), MIG-3 (missing `DROP CONSTRAINT` when a column stops being an FK), then MIG-6's refusal.

## Findings

Ordered by severity (HIGH → INFO). Every finding carries exact location, quoted evidence, a concrete impact scenario, a fix sketch, and a status.

### MIG-1 · HIGH · correctness · self-verified
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

### MIG-2 · HIGH · correctness/drift · self-verified
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

### MIG-3 · MEDIUM · correctness/drift · self-verified
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

### MIG-4 · MEDIUM · correctness/codegen · self-verified
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
- **Fix**: make the fallback actually disambiguate — deterministic discriminator (schema-qualified table segment, or a numeric suffix for later tables) or refuse with both table names; add a test (see MIG-5).
- **Status**: new. Self-verified: read `resolve_struct_names` whole plus both introspector call sites (405/449); traced the two-table scenario; refuted the alternative that counts are framework-scoped (non-Django names get no prefix).

### MIG-5 · MEDIUM · correctness/codegen · self-verified
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
- **Impact**: table `2fa_tokens` (legal; realistic) → `pascal_case_from_table` leaves the leading digit → `pub struct 2faTokens {` — does not parse. `___` → `""` → `pub struct  {`. `self` → `"Self"` — the keyword list even contains `"self"`/`"Self"` (`inspect.rs:1847`), but it never sees struct names. Separately, the attribute payload is raw: a table named `weird"name` (creatable with doubled quotes) emits `#[umbral(table = "weird"name")]` — does not parse; a table containing `\` compiles into a Rust escape and **silently names a different table**. Reachable only via `inspectdb` against an existing DB (dev-time codegen), and the common cases are loud at `cargo build` — hence MEDIUM (per the calibration rule: dev-time codegen emitting invalid output), with the `\` case as the silent one. Cross-ref: the casing-crate half of this is CASING-1 (`planning/review_4/umbral-casing.md`, `crates/umbral-casing/src/lib.rs:89-112`); this finding is the inspect.rs emission/validation half, routed here per that file's out-of-scope notes.
- **Fix**: validate struct names where `struct_names` is built — the tree owns the rule (`crates/umbral-core/src/codegen.rs:184 validate_ident`: rejects empty / leading-digit / keyword) — prefix/suffix or refuse; and escape the attribute payload (`\` → `\\`, `"` → `\"`) or refuse names that don't round-trip. Extend `inspectdb_generates_compilable_models_for_edge_cases` (`tests/inspect.rs:1181`) with a `2fa_tokens`-style table, a quote/backslash table, and the MIG-4 collision pair — the current test only covers a keyword column and a non-`id` PK.
- **Status**: new. Self-verified: read `render_one_struct` and `resolve_struct_names` whole; confirmed no validator runs between the introspectors and line 1735; confirmed the keyword list content and its field-only wiring.

### MIG-6 · MEDIUM · correctness · self-verified
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

### MIG-7 · LOW · error-handling · self-verified
**`reset_sequence` swallows its own failure: `let _ = sqlx::query(&sql).execute(p).await;` — a failed sequence reset leaves the copied ids' next auto-increment value untouched, and the inevitable duplicate-key error surfaces later in unrelated app code.**

- **Where**: `crates/umbral-core/src/transfer.rs:983-984`; the twin that propagates is `crates/umbral-core/src/backup.rs:402-436` (`reset_pg_sequence` — both awaits use `?`).
- **Evidence**:
  ```rust
  // transfer.rs:983-984
  // A non-integer PK has no serial sequence; the guard makes this a no-op.
  let _ = sqlx::query(&sql).execute(p).await;
  ```
- **Impact**: the no-op case the comment describes is already handled by the SQL itself (`WHERE pg_get_serial_sequence('{t}', '{c}') IS NOT NULL`, 976-982 — zero rows on a non-serial PK, still `Ok`). The `let _` therefore only suppresses **real** failures (a permission error, a dropped connection at that instant, a setval edge). After a transfer whose reset failed, the app's next ORM insert into the transferred table collides with a copied id (`23505 duplicate key`) — a confusing failure in unrelated code with no record that the sequence reset was the cause. This is a drift between the two sequence-reset implementations in the same slice: backup's twin fails loudly.
- **Fix**: `sqlx::query(&sql).execute(p).await?;` — the WHERE guard already makes the non-serial case a no-op, so propagating cannot introduce a spurious failure.
- **Status**: new. Self-verified: re-read the function and its call context, and the backup.rs twin; confirmed the no-op guard is the SQL predicate, not the discard.

### MIG-8 · LOW · drift · self-verified
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

### MIG-9 · LOW · docs
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

### MIG-10 · INFO · robustness
- **Where**: `crates/umbral-core/src/backup.rs:520` (and `:581`).
- A dump row **missing** a column the current model still has silently binds `Value::Null` (`row.get(&col.name).cloned().unwrap_or(Value::Null)`), while a dump row with an **extra** column is a hard `BackupError::UnknownColumn` nine lines above (511-515). The asymmetry looks unintentional: loading an older dump against a newer schema (nullable column added since) fills NULL with no warning, and a typo cannot be distinguished from "legitimately absent". Document the forward-compat intent or warn/count missing columns.

## Strengths

- **The refusal surface makes the easy path the safe path.** `UnsafeAlter` blocks NOT NULL adds without default at diff time with three actionable options (`migrate.rs:5226-5236`), plus NOT NULL tightening, UNIQUE adds, unsafe casts and length narrowing; and rename detection is fail-closed (`AmbiguousRename`) unless `UMBRAL_MIGRATIONS_ASSUME_RENAMES` opts in. This is the product's core promise, implemented.
- **The SQLite table-recreation dance is genuinely careful.** It re-creates indexes and `unique_together` (audit_2 #10 held), backfills NOT NULL+default columns, shapes `new_columns` from the **previous** table so a combined alter+add/drop can never reference a missing column (audit_2 H21 held), and the same helper is reused by the constrained-column `DropColumn` path.
- **Constraint-aware drop routing** (`drop_requires_rebuild_sqlite`, `migrate.rs:5267-5279`): SQLite's exact rules (PK / UNIQUE / FK-with-constraint / indexed / in a `unique_together` or index group) encoded once, with the tracker id in the comment.
- **Comment discipline at 8k LOC.** Almost every non-obvious branch names the tracker entry and the failure it prevents — gap 97, gaps2 #22, gaps3 #35/#43, audit_2 #5/#10/H21, review_3's `col_no_unique` fix — which is what makes this file reviewable at all.
- **backup.rs's typed binding and ordering.** Per-`SqlType` encode/decode with loud `TypeMismatch`/`UnknownColumn` errors, FK-topological load order with per-table transactions, `quoted_ident` quote-doubling everywhere, and a verified PG sequence reset (`backup.rs:402-436`).
- **transfer.rs's copy core is sound.** FK-level parallelism with per-table tasks, keyset pagination (not OFFSET), a per-batch transactional checkpoint atomically with the data (`upsert_state_in_tx`), M2M junction folding, and cyclic/self-FK deferral — the defects found are in the state/sequence plumbing, not the algorithm.
- **inspectdb's framework-aware layer** recovers FK/unique/index/auto_now/default metadata (Django app-prefix strip + `auth_user` externalization, Rails HABTM, Prisma implicit M2M), and the tests are real-DB round-trips rather than mocks.

## Clean areas checked

- SQL injection in DDL/DML: every `format!`-built statement read in this slice quotes identifiers and doubles embedded quotes (e.g. `transfer.rs:980-981` `replace('\'', "''")`, `migrate.rs:5578-5579` `replace('"', "\"\"")`); values are always bound (`q.bind`) on the backup/transfer paths. The one injection-adjacent surface is generated **source** escaping — MIG-5.
- `unsafe`: none. All six matches in the slice are comments, message strings, or the keyword list (`migrate.rs:1910/3775/4926/6129/7409`, `inspect.rs:1881`).
- Panics: no `unwrap` reachable from user input in the four data files; the `expect`s are backend-tag invariants inside match arms (`tx.as_sqlite_mut().expect("sqlite backend")`, `transfer.rs:955`) that cannot fire while the tag and the arm agree.
- Tracking/apply idempotency: drop/readd pairs use `IF EXISTS` (`migrate.rs:6231-6233`), M2M/table DDL uses `IF NOT EXISTS` on drops/recreates, apply is transactional per migration file, and the PG advisory lock is deterministic.
- Previously-fixed items verified held: squash-aware drift detection; `UNIQUE` on ADD COLUMN → separate `CREATE UNIQUE INDEX` (`migrate.rs:5588-5599`); backup.rs alias routing; fixtures' `load_fixture` uses `.presealed()` (`fixtures.rs`, review_3's Masked double-seal fix); rename detection fails closed; `DropColumn` of a constrained column still routes through the dance.
- fixtures.rs: full read (216 lines) — no panics, typed `FixtureError`, Masked round-trip pinned by review_3's fix; no findings.
- Tests: structurally behavioral — real on-disk dump/load round-trips (`backup.rs`, `backup_json.rs`, `backup_fk_order.rs`, PG-gated `backup_postgres.rs`), the refusal surface pinned (`rename_detection`, NOT NULL backfill, `gap70_drop_unique_column_sqlite`, `varchar_length_alter`, `migration_safety`), transfer SQLite→SQLite end to end. Gaps the findings expose: no PG-target **rerun** test (MIG-1), no db_constraint-flip diff test (MIG-2), no FK→BigInt transition test (MIG-3), no collision/non-ident codegen test (MIG-4/MIG-5), no pk-add test (MIG-6).

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2/findings/core-migrate.md #2` — combined alter+add/drop on SQLite (deferred; the H21 shape-snapshot half is fixed and held).
- `audit_2 #8` — `seq`-from-count, `depends_on` unenforced, alias-aware drift (deferred).
- `audit_2 #9` — `snapshot_hash` recorded but never verified (deferred).
- `audit_2 #11` — inspectdb constraint introspection (deferred; FK/unique/index recovery landed later, CHECK constraints still absent).
- `audit_2 #12` — streaming dump/load (deferred).
- `audit_2 #13`/`#16` — check.rs catalogue / `is_loopback_bind` (deferred; sibling slice).
- `audit_2 #15` — inspectdb raw field idents + composite PKs (deferred; field-ident half since fixed by `safe_field_ident`).
- `audit_2 #17` — alias-aware `record_applied` (deferred).
- `audit_2/AUDIT_REPORT.md H23` (+ `#4` superseded) — fail-closed rename heuristic (fixed; verified held).
- `audit_2 #1,#3,#5,#6,#7,#10,#14,#18` — fk_target_pk re-add, backup topo+tx+seq, NOT NULL backfill, destructive gate, advisory lock, dance index re-creation, identifier escaping, duplicate-table detection (shipped; spot-verified held).
- `review_3/claude/02_triage.md` — fixtures restore Masked double-seal (fixed; `.presealed()`, verified held).
- `review_3/claude/02_triage.md` — backup.rs alias routing (fixed; verified held).
- `review_3/claude/02_triage.md` — squash-aware drift detection (fixed; verified held).
- `review_3/claude/02_triage.md` — unique ADD COLUMN on SQLite (fixed; verified held).
- `review_3/claude/02_triage.md` — PG per-file apply dedup, `migrate.rs:2592` (deferred).
- `review_3/claude/02_triage.md` — tracking-DDL dedup, `migrate.rs:2940` (deferred).
- `gaps2 #22` — `db_constraint` cross-DB FK opt-out (shipped; `planning/archive/gaps2-done.md:553`). Its "flipping the flag tears down a prior constraint" claim does not hold in the code — the specific defect is **MIG-2** (kept numbered; a class-level tracker entry does not absorb it).

## Out-of-scope notes

- `planning/review_4/umbral-casing.md` CASING-1 — the casing-crate half of MIG-5 (`pascal_case_from_table` can return non-identifiers); MIG-5 files only the inspect.rs emission/validation half, no double-count.
- `crates/umbral-core/src/app.rs:1364` — the cross-DB FK boot guard reads `field.db_constraint`; under MIG-2 the guard's view and the DB's actual constraints can disagree (core runtime slice).
- `crates/umbral-core/src/orm/model.rs:668` — `Column::db_constraint` serde shape (`default_true` + skip) — the snapshot field MIG-2's diff ignores; no change needed there, noted as the fix's anchor.
- `crates/umbral-macros/src/lib.rs:1564-1583` — PK resolution is first-match; a second `#[umbral(primary_key)]` field is silently not the pk (reachability context for MIG-6; macros slice).
- `documentation/docs/v0.0.1/orm/database-routing.mdx:114-120` — "the migration emits no physical `REFERENCES`" needs the pre-existing-FK caveat once MIG-2 is fixed.
- `planning/archive/gaps2-done.md:553` — #22's write-up asserts the flip tears down the constraint; needs the MIG-2 correction when the fix lands.
