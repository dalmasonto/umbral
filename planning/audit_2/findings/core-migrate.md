# Audit: migration engine, inspectdb, backup/fixtures, system checks (`core-migrate`)

Scope: `crates/umbral-core/src/{migrate,inspect,backup,fixtures,check}.rs` (read in full), plus the `documentation/docs/v0.0.1/migrations/` pages. Supporting evidence consulted read-only: `crates/umbral-cli/src/lib.rs` (flag existence), `crates/umbral-core/src/db/router.rs` (`Schema` validation), vendored `sea-query-0.32.7` (identifier quoting).

## A. Executive summary

The migration engine's guardrails are better than most young frameworks: `diff_columns` refuses NOT-NULL-without-default adds, UNIQUE additions, unsafe casts and PK flips at `makemigrations` time (migrate.rs:3240–3272, 3391–3409); FK-ordered `CreateTable` toposort exists (2878–2931); each migration applies in one transaction with its tracking row; drift detection blocks on applied-but-missing files. However, several paths that only fire on *evolving, populated* databases are broken or misleading. The three most urgent: (1) the Postgres `AlterColumn` FK re-add hardcodes `REFERENCES <target>("id")`, so any FK-attribute change against a non-`id`-PK target (a shape the framework explicitly supports post-PK-lift) either aborts the migration or silently attaches the constraint to the wrong column; (2) a single `makemigrations` run that combines an alter with an add/drop on the same table produces a migration that cannot apply on SQLite (the table-recreation dance references columns that don't exist yet / drops columns twice); (3) `backup::load` restores tables in alphabetical order with FK constraints active, per-row autocommit, and no Postgres sequence reset — the disaster-recovery path fails on any schema where a child table sorts before its parent, and a mid-load failure leaves a half-restored database. A fourth systemic risk: the column-shape rename heuristic can silently convert an intended drop+create of two unrelated models into a `RenameTable`, handing one model's rows to another with only a stderr warning. I could not assess the CLI plumbing, `DynQuerySet::insert_json` internals (fixtures' validation path), `App::build` phase ordering, or live-Postgres behavior — see Blind spots. Deserialization surfaces (backup/fixtures) are typed and column-validated; no mass-assignment or type-confusion hole was found in the files themselves.

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | Migrations / DDL correctness | crates/umbral-core/src/migrate.rs:4078–4082 | `AlterColumn` FK drop+re-add renders `REFERENCES {q_target}("id")` — the referenced PK column is hardcoded to `id`, unlike the CreateTable path which resolves it via `fk_target_pk` (migrate.rs:4173–4208). | Changing `on_delete`/`on_update`/`fk_target` on an FK whose target PK isn't named `id` (String/Uuid PKs are supported, e.g. `Permission.codename`) aborts the migration mid-deploy; if the target happens to have a *unique non-PK* `id` column, the constraint silently attaches to the wrong column (referential corruption). | Resolve the referenced column via `fk_target_pk(target)` in the re-add branch. | ✅ done |
| 2 | HIGH | Migrations / autodetection | crates/umbral-core/src/migrate.rs:3305–3314 (op emission order + `new_columns = current.fields`), 3902–3943 (SQLite dance) | Combined changes to one table in one diff break SQLite apply: `AlterColumn` ops are emitted *first* and carry the full post-change column list, so the recreation dance's `INSERT … SELECT` references newly *added* columns that don't exist in the old table; conversely a *dropped* column is already absent after the rebuild, then the subsequent `DropColumn` op fails ("no such column"). | Any routine edit that both alters and adds/drops fields on one model produces a written migration that cannot apply on SQLite (transaction rolls back; deploy blocked; file must be hand-edited). | Either fold same-table adds/drops into the recreation dance (single rebuild) or restrict `new_columns` in the dance to `prev ∩ current` and order add/drop ops before the alter. | deferred: SQLite combined alter+add/drop dance — subtle reorder, high schema-corruption risk |
| 3 | HIGH | Backup / recovery | crates/umbral-core/src/backup.rs:159–173 (dump sort), 186–213 (load loop), 342–373 (per-row execute) | `load` inserts tables in the dump's alphabetical order with FK constraints active, executes row-by-row on the pool (no transaction, no FK deferral), and never resets Postgres sequences after inserting explicit PKs. | Restore fails wholesale on any schema where a child table sorts before its FK parent (`comment` < `post`); a mid-load failure leaves a partially restored DB with no rollback; even a "successful" Postgres restore then throws duplicate-PK errors on the first ORM insert because BIGSERIAL sequences still start at 1. The recovery path is unreliable exactly when it's needed. | Topologically sort tables by `fk_target` before loading; wrap the load in one transaction per backend; after load, `SELECT setval(pg_get_serial_sequence(t, pk), max(pk))` for integer-PK tables. | ✅ done |
| 4 | HIGH | Migrations / autodetection | crates/umbral-core/src/migrate.rs:2839–2862 (table), 3345–3366 (column) | Second-pass rename heuristic auto-pairs an unrelated dropped model with an unrelated created model whenever their column shapes match bit-for-bit, emitting `RenameTable` with only an `eprintln!` warning. Same pattern for single-column drop+add. | Two trivial lookup models (`Category{id,name}` dropped, `Genre{id,name}` added) silently become a rename: `Genre` inherits every `Category` row, `Category`'s intended drop never happens, and the only signal is stderr in a CI log. Silent data mis-association in production. | Don't auto-emit on the shape-only match: emit `DropTable`+`CreateTable` and print the rename *suggestion* (with the exact `RenameTable` JSON to paste), or gate on an interactive/`--rename from:to` confirmation. | deferred: rename heuristic — needs interactive confirm / --rename policy (UX design) |
| 5 | MEDIUM | Migrations / NOT NULL tightening | crates/umbral-core/src/migrate.rs:3259–3265 (guard), 3992–4005 (PG render), 3919–3925 (SQLite dance) | The nullable→NOT NULL guard passes when `curr_col.default` is non-empty ("requires a default/backfill before tightening"), but neither renderer emits a backfill `UPDATE` — Postgres runs bare `SET NOT NULL` (fails on existing NULLs; `SET DEFAULT` doesn't backfill), and the SQLite dance's `INSERT … SELECT` copies NULLs into a NOT NULL column and fails. | The engine's own error message steers users to add a default, then the generated migration still aborts at deploy time on any pre-existing NULL row. Fails loudly (transactional), but blocks releases and contradicts the guard's promise. | Emit `UPDATE "{t}" SET "{c}" = <default> WHERE "{c}" IS NULL` before `SET NOT NULL` (and before the SQLite copy), or keep refusing the tighten unless a backfill op is present. | ✅ done — both renderers backfill: Postgres emits `UPDATE … WHERE c IS NULL` before `SET NOT NULL`; the SQLite dance copies each NOT-NULL-with-default column as `COALESCE(c, <default>)` in the INSERT…SELECT (no `prev` needed — COALESCE is a no-op for a column that never held NULLs). New `default_sql_literal(col, is_postgres)` renders type-aware literals (numeric/boolean unquoted, else quoted). Tests `migrate_notnull_backfill`: SQLite (runs) + live-PG (`#[ignore]`) — the PG test VERIFIED to fail with `column "status" contains null values` when the backfill is disabled, pass (NULLs → default) with it. |
| 6 | MEDIUM | Migrations / destructive changes | crates/umbral-core/src/migrate.rs:2941–2948, 2970–2979; gate: 2555–2557 ("nothing here gates migrate") | Unregistering a model/plugin (commented-out `.model::<T>()`, feature-flag removal, plugin dropped from the builder) auto-generates `DropTable`/`DropM2MTable`, and `migrate` applies them with no confirmation. `checkmigrations` is advisory and only helps if wired into CI. | One missing registration line + the standard `makemigrations && migrate` loop drops production tables and all rows. | Require an explicit acknowledgment for `DropTable` at apply time (e.g. `migrate --allow-destructive` or an interactive confirm), or at minimum print the UNSAFE classification during `migrate` itself, not only in `checkmigrations`. | deferred: destructive DropTable confirm at apply — CLI/UX policy |
| 7 | MEDIUM | Tracking-table integrity | crates/umbral-core/src/migrate.rs:1562–1687 (apply loops) | No cross-process migration lock: concurrent `migrate` runs (multi-replica deploy, the norm at the stated scale) both read the applied set, then race the same DDL; the loser errors mid-deploy ("relation already exists") and aborts its remaining migrations. | Flaky deploys; with per-alias multi-file runs, replicas can fail at different points leaving pools at different migration depths until retried. | Take `pg_advisory_lock` (Postgres) / `BEGIN IMMEDIATE` on a lock row (SQLite) around the whole run, keyed per pool. | ✅ done (Postgres) — every PG apply path (`run_in_postgres_for_alias`, and via it the checked run; `run_for_schema_in` for tenant schemas) brackets the applied-set-read + apply in a session `pg_advisory_lock`, keyed by a deterministic FNV hash of the alias/schema (so different DBs/schemas migrate concurrently, the same one serializes). Bounded wait via `UMBRAL_MIGRATION_LOCK_TIMEOUT_SECS` (default 300) → `MigrateError::MigrationLockTimeout`; a crashed migrator's session lock auto-releases. SQLite is left to its file lock (single-writer / single-process migration). Tests: unit `pg_migration_lock_key_is_deterministic_and_distinct`; live-PG `migration_lock_postgres::concurrent_migrators_serialize_and_apply_once` — VERIFIED to fail with `relation already exists` when the lock is disabled, pass (applied exactly once) with it. |
| 8 | MEDIUM | Migration ordering | crates/umbral-core/src/migrate.rs:1074 (`depends_on`), 1329–1331 & 1395 (`seq = existing.len() + 1`), 2216–2219 (OutOfOrder), 1592–1598 (per-alias skip) | `depends_on` is serialized but never read or enforced anywhere in the workspace (grep: only ever written as `Vec::new()`); sequence numbers derive from the *count* of files, so a deleted file yields duplicate prefixes and two merged branches both produce `0002_*` with no conflict detection; OutOfOrder files warn but still apply after later migrations; multi-alias files whose ops all target another pool are permanently reported Pending/OutOfOrder on this pool's drift walk. | Cross-branch merges apply in lexical order that may not match author intent; a restored old migration applies against a schema it wasn't written for; multi-DB apps get perpetual drift-report noise that trains operators to ignore warnings. | Derive seq from `max(existing prefix) + 1`; detect duplicate prefixes at `make`/`run` time; either enforce `depends_on` or delete the field; make drift detection alias-aware (the code comment at 1444–1450 already acknowledges this). | deferred: seq-from-max + depends_on enforcement + alias-aware drift — cross-cutting ordering redesign |
| 9 | MEDIUM | Tracking-table integrity | crates/umbral-core/src/migrate.rs:1606, 2173–2247, 3476–3503 | `snapshot_hash` is written into `umbral_migrations` "for drift detection" (526–528) but never read back or compared; drift detection only checks file *presence*. | An applied migration file edited after apply (rebased, squashed, hand-"fixed") silently changes the base snapshot for every future diff — schema state and history diverge undetected. | On `run`/`show`, re-hash each applied file's `snapshot_after` and flag hash mismatches as a fifth drift state. | deferred: snapshot-hash drift — new fifth drift state, needs design |
| 10 | MEDIUM | Migrations / SQLite dance | crates/umbral-core/src/migrate.rs:3894–3897, 3902–3943 | The SQLite table-recreation dance rebuilds columns only: single-column indexes, multi-column `indexes`, and composite `unique_together` constraints are dropped with the old table and never re-created (acknowledged in the comment, but the snapshot *does* carry `unique_together`/`indexes` now — the data is available). | Any nullable flip / safe cast on SQLite silently strips composite UNIQUE constraints (duplicates become insertable — integrity loss, not just perf) and all secondary indexes. | Re-emit `unique_together` in the temp `CREATE TABLE` and re-run `create_index_stmt`/`create_multi_index_stmt` after the rename, using the `ModelMeta` already in the snapshot. | ✅ done — dance re-creates indexes + `unique_together` (commit dbf87307, test `sqlite_dance_preserves_constraints`). Follow-up: the autodetector now also DIFFS model-level `unique_together`/`indexes` (new `AddIndex`/`DropIndex` ops + `diff_indexes`), which was silently ignored before and is the blocker called out in plugin-storage-tasks #5; `unique_together` is now rendered as a named, droppable `CREATE UNIQUE INDEX` everywhere. Test `migrate_index_autodetect`. |
| 11 | MEDIUM | inspectdb → later migrations | crates/umbral-core/src/inspect.rs:806–844 (`Column::from`, `unique: false`, `fk_target: None`, `default: ""`), 39–41 | inspectdb captures no UNIQUE/FK/default/index/CHECK metadata, so the generated snapshot describes a constraint-less schema. Combined with finding 10, the first `AlterColumn` on a ported SQLite DB rebuilds the table from that snapshot and silently strips the legacy database's *real* constraints and indexes. | A ported production DB loses its FK and UNIQUE enforcement on the first routine column alter, with no error and no warning. | Until constraint introspection ships, refuse (or loudly warn on) the SQLite recreation dance for tables whose origin is an introspected `0001_initial`; prioritize UNIQUE/FK introspection. | deferred: inspectdb constraint introspection + dance guard — large |
| 12 | MEDIUM | Backup / scale | crates/umbral-core/src/backup.rs:247–252, 321–326 (unbounded `SELECT`, full materialization) | `dump` runs `SELECT <all cols> FROM <table>` per model and holds every row of every table as `serde_json::Value` in memory; `load` similarly parses the whole file. Deferred-streaming is documented (35–39) but nothing guards against use at scale. | At the stated 10M-user scale the dump path OOMs or stalls the process; operators discover this during an incident. | Stream per-table (fetch in pages, write incrementally); until then, document the size ceiling and add a row-count warning. | deferred: streaming dump/load — feature, not a fix |
| 13 | MEDIUM | Boot checks / gaps | crates/umbral-core/src/check.rs:142–181 (catalogue) | Production misconfigs the check catalogue misses: (a) SQLite backend in `Environment::Prod` (framework is Postgres-first; SQLite at scale is a misconfig); (b) weak `secret_key` that isn't the literal dev default — a 1-char key passes (199–228 compares only equality with the default); (c) unapplied pending migrations at boot (schema/code drift); (d) `allowed_hosts = ["*"]` passes the allowed-hosts check (290–310 only detects the unchanged dev default). | Each is a silent prod footgun the check framework exists to catch. | Add `backend.sqlite_in_prod` (Warning), a min-length/entropy floor to `settings.required`, a `migrations.pending` Warning that reuses `detect_all_drift`, and a wildcard-host Warning. | deferred: check catalogue — owned by check.rs, out of scope |
| 14 | LOW | DDL identifier escaping | crates/umbral-core/src/migrate.rs:3705–3718 & 3839–3852 (CreateM2MTable raw DDL), 4017–4025 & 4056–4058 & 4090–4093 (constraint names), 929–948 (`create_multi_index_stmt` strips quotes from the ON-clause table name instead of escaping, unlike `create_index_stmt` 894–902) | The M2M junction DDL interpolates five identifiers with no quote-escaping; Postgres ALTER constraint names embed unescaped table/column; the multi-index helper references a quote-*stripped* table name. sea-query paths are safe (verified: Iden::prepare doubles quotes, sea-query-0.32.7/src/types.rs:31–39), and identifiers are developer-supplied (`#[umbral(table=..)]`), not runtime-attacker input, so this is robustness, not an exploitable injection. | A table/column name containing `"` produces malformed or wrong-target DDL in exactly these paths while working everywhere else. | Route the raw templates through `quote_pg_ident`/the existing `replace('"', "\"\"")` idiom; better, validate identifiers at derive time (`^[A-Za-z_][A-Za-z0-9_]*$`, as `db::Schema::new` already does — router.rs:44–56). | ✅ done |
| 15 | LOW | inspectdb codegen | crates/umbral-core/src/inspect.rs:636–644 | Generated field idents are raw column names: a column named `type`/`match`/`ref` renders `pub type: String` (doesn't compile); composite-PK tables render multiple `primary_key` fields the derive will reject. | Port of a legacy DB fails at compile time with confusing errors instead of a clear diagnostic. | Emit `r#`-escaped idents for Rust keywords; detect composite PKs during introspection and error with the table name. | deferred: r#-escaped idents need umbral-macros to unraw the column name (out of scope); else column mis-maps. composite-PK detection needs introspection change |
| 16 | LOW | Boot checks | crates/umbral-core/src/check.rs:274–284, 414–449 | `is_loopback_bind` misparses a bare unbracketed IPv6 `::1` (rsplit on `:` yields host `::`, classified non-loopback → spurious warning). `field_backend` findings use `CheckLocation::Settings` even though `CheckLocation::Field` exists and other field checks use it. | Cosmetic noise / mis-attributed findings. | Try `bind_addr.parse::<SocketAddr>()` first; use `CheckLocation::Field` (leak pattern already established at 499–506). | deferred: check.rs — out of scope |
| 17 | LOW | Tracking table / multi-DB | crates/umbral-core/src/migrate.rs:2109–2147, 2303–2314, 2336–2341 | `record_applied` / `fake_apply` / `fake_initial` write only to the default pool (`pool_dispatched()`), while `run` walks every registered alias. | On multi-DB apps, `--fake` / `--fake-initial` / `inspectdb --mark-applied` cannot reconcile a secondary pool's ledger; the next `migrate` re-runs (and fails) there. | Accept an alias parameter or walk `registered_aliases()` like `run_checked_in` does. | deferred: alias-aware record_applied/fake_* — needs alias plumbing |
| 18 | LOW | Backup load | crates/umbral-core/src/backup.rs:199–212 | `by_table.remove(&model.table)` means a dump containing two entries for the same table loads the first and silently routes the second to `skipped_tables` (the "unknown table" bucket). | A merged/hand-edited dump partially loads with a misleading report ("skipped" implies unknown schema, not duplicate). | Look up without removing; track loaded tables separately and error on duplicates. | ✅ done |

No CRITICAL findings: every data-losing path found either fails loudly inside a transaction or requires a developer-side action (model removal, shape-coincidence) — but findings 3, 4, and 6 sit one operator mistake away from that tier.

## C. Detailed findings (CRITICAL / HIGH)

### C1. AlterColumn FK re-add hardcodes the referenced column to `"id"` (HIGH, #1)

Vulnerable code — `render_alter_column_postgres`, migrate.rs:4064–4083:

```rust
if let Some(target) = &new.fk_target
    && new.db_constraint
{
    let q_target = quote_pg_ident(target);
    ...
    stmts.push(format!(
        "ALTER TABLE {q_table} ADD CONSTRAINT \"{cname}\" \
         FOREIGN KEY ({q_column}) REFERENCES {q_target}(\"id\")\
         {on_delete_clause}{on_update_clause}"
    ));
}
```

Scenario: the auth plugin's `Permission` model has PK `codename: String` (the PK-lift work made this a first-class shape; `fk_target_pk` at migrate.rs:4173–4208 exists precisely to resolve it for CreateTable). A model with `#[umbral(on_delete = ...)]` changed on its FK to `permission` diffs into `AlterColumn`; the Postgres renderer drops the old constraint, then re-adds `REFERENCES "permission"("id")`. Postgres aborts the migration transaction with `column "id" referenced in foreign key constraint does not exist` — deploy blocked. Worse variant: if the target table has a unique `id` column that is *not* its PK, the constraint is created against the wrong column and every future insert is validated against the wrong key.

Corrected snippet:

```rust
if let Some(target) = &new.fk_target
    && new.db_constraint
{
    let q_target = quote_pg_ident(target);
    let (pk_col, _ty) = fk_target_pk(&target.replace('"', "\"\""));
    let q_pk = quote_pg_ident(&pk_col);
    stmts.push(format!(
        "ALTER TABLE {q_table} ADD CONSTRAINT \"{cname}\" \
         FOREIGN KEY ({q_column}) REFERENCES {q_target}({q_pk})\
         {on_delete_clause}{on_update_clause}"
    ));
}
```

### C2. Combined alter + add/drop on one table generates an unappliable SQLite migration (HIGH, #2)

Vulnerable code — `diff_columns`, migrate.rs:3305–3314 (AlterColumn carries the FULL current column set, and is emitted before adds/drops):

```rust
let new_columns: Vec<Column> = current.fields.clone();
...
for name in alter_columns {
    ops.push(Operation::AlterColumn {
        table: current.table.clone(),
        column: name.to_string(),
        new_columns: new_columns.clone(),
        prev_columns: Some(prev_columns_snapshot.clone()),
    });
}
// ... drops and adds are pushed AFTER this
```

and the dance, migrate.rs:3919–3925, uses that same list on both sides of the copy:

```rust
let column_list = new_columns.iter().map(...).join(", ");
let insert_sql =
    format!("INSERT INTO \"{tmp}\" ({column_list}) SELECT {column_list} FROM \"{table}\"");
```

Scenario: one edit makes `Post.summary` nullable AND adds `Post.subtitle: Option<String>`. `makemigrations` writes `[AlterColumn{new_columns: [.., subtitle]}, AddColumn{subtitle}]`. On SQLite apply, step 2 of the dance runs `SELECT ... "subtitle" ... FROM "post"` — `no such column: subtitle`, transaction rolls back, `migrate` is bricked until the JSON is hand-edited. The mirror case (alter + drop) fails on the trailing `DropColumn` because the rebuilt table already omitted the column.

Corrected approach (minimal): compute the dance's schema/copy list per-op instead of from the final snapshot —

```rust
// In diff_columns: the alter op's new_columns must describe the table as it
// exists AT THAT POINT in the op stream: previous columns with only the
// altered attributes applied (no adds, no drops).
let dance_columns: Vec<Column> = previous.fields.iter()
    .map(|p| curr_cols.get(p.name.as_str()).copied().cloned().unwrap_or_else(|| (*p).clone()))
    .collect();
```

…and keep AddColumn/DropColumn as separate later ops (they already render correctly in isolation). Alternatively emit ops in the order drop → alter → add and exclude added/dropped names from `new_columns`.

### C3. `backup::load` cannot restore FK-ordered schemas; no transaction; no sequence reset (HIGH, #3)

Vulnerable code — backup.rs:159–163 (dump order is alphabetical), 199–212 (load follows dump order), 342–372 (per-row `execute(pool)`, no transaction):

```rust
let mut models = crate::migrate::registered_models();
models.sort_by(|a, b| a.table.cmp(&b.table));          // dump order
...
for model in &dump.models {                            // load in same order
    ...
    let inserted = load_one(pool, &meta, &model.rows).await?;
}
...
q.execute(pool).await?;                                // row-by-row, autocommit
```

Scenario: schema `post` + `comment(post_id FK → post)`. Dump writes `comment` before `post` (alphabetical). Restore into a freshly migrated Postgres DB: the first `comment` row fails `violates foreign key constraint` — restore dead on arrival. Second scenario: a mid-file `TypeMismatch` on row 40,000 aborts the run leaving 39,999 rows committed with no rollback. Third: a "successful" restore inserts explicit `id`s but BIGSERIAL sequences still sit at 1 — the first user signup after restore throws `duplicate key value violates unique constraint "user_pkey"`, repeatedly, until the sequence catches up past max(id).

Corrected shape:

```rust
pub async fn load(dump: &Dump) -> Result<LoadReport, BackupError> {
    // 1. Topo-sort dump.models by fk_target dependencies (reuse the Kahn
    //    walk from migrate::diff, keyed on ModelMeta.fields[].fk_target).
    // 2. One transaction per backend:
    let mut tx = pool.begin().await?;
    for model in &ordered {
        load_one_tx(&mut tx, &meta, &model.rows).await?;
    }
    // 3. Postgres only: fix sequences for integer PKs.
    for meta in &loaded {
        if let Some(pk) = meta.pk_column().filter(|c| matches!(c.ty, SqlType::BigInt | SqlType::Integer)) {
            sqlx::query(&format!(
                "SELECT setval(pg_get_serial_sequence('{t}', '{c}'), \
                 COALESCE((SELECT MAX({qc}) FROM {qt}), 0) + 1, false)",
                t = meta.table, c = pk.name,
                qt = quoted_ident(&meta.table), qc = quoted_ident(&pk.name),
            )).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    ...
}
```

### C4. Shape-match rename heuristic silently reassigns one model's data to another (HIGH, #4)

Vulnerable code — migrate.rs:2839–2861:

```rust
if create_shape == drop_shape {
    eprintln!("umbral makemigrations: rename detected (column-shape match): ...");
    ops.push(Operation::RenameTable {
        from: drop.table.clone(),
        to: create.table.clone(),
    });
    ...
}
```

Scenario: a team deletes `Category { id: i64, name: String }` and, in the same change, introduces `Genre { id: i64, name: String }` — an extremely common minimal shape. The diff pairs them, `makemigrations` writes `RenameTable { from: "category", to: "genre" }`, CI (which nobody watches for stderr) applies it. Result in production: every old category row is now served as a `Genre`; the intended empty `genre` table never exists; `category` is gone. No error at any point. The doc page even instructs users to review and hand-replace the op (managed-migrations.mdx "Rename detection" callout) — the framework should not require that vigilance for correctness.

Corrected approach: keep the detection, drop the auto-emit —

```rust
if create_shape == drop_shape {
    eprintln!(
        "umbral makemigrations: `{}` and `{}` have identical column shapes. \
         If this is a rename, replace the DropTable+CreateTable ops in the \
         generated file with: {}",
        drop.table, create.table,
        serde_json::to_string(&Operation::RenameTable {
            from: drop.table.clone(), to: create.table.clone(),
        }).unwrap(),
    );
    // fall through: emit DropTable + CreateTable as the default
}
```

(The first-pass struct-name match at 2802–2816 is fine to keep automatic — `Model::NAME` identity is a strong signal.)

## D. Blind spots

- **`umbral-cli`**: flag parsing, exit codes, and how `boot_for_management` orders registry init vs. these entry points. I only grepped for flag existence (`--strict`, `--empty`, `--fake*`, `--allow-drift` all exist in `crates/umbral-cli/src/lib.rs`).
- **`DynQuerySet::insert_json`** (orm/dynamic.rs): fixtures.rs delegates all validation (unknown-column, choices, FK existence, mass-assignment) to it. Whether fixture rows can set `noform`/`noedit` columns or bypass validators was not verifiable from the in-scope files.
- **`App::build` phase ordering** (app.rs): claims about when `init_plugins` / `init_plugin_order` / checks run are taken from doc comments, not verified.
- **`plugin_order()` topology source**: whether the phase-1.5 sort actually reflects `Plugin::dependencies()` and whether cross-plugin FK apply-order is correct end-to-end (the per-plugin *full-directory* apply loop at migrate.rs:1572–1621 cannot interleave files across plugins; whether that ever matters depends on plugin dep declarations I didn't audit).
- **Live-backend behavior**: SQLite `foreign_keys=ON` interaction with `DROP TABLE` inside the recreation dance (a referenced parent table may fail to drop / other tables' FK references may not follow the rename); Postgres lock behavior of the emitted ALTERs on large tables. Static read only — nothing was executed.
- **`connect_sqlite` / pool config** (db.rs): pragmas, WAL, pool sizing — out of scope but load-bearing for several findings.
- **Masked-field interaction with `dump`**: whether `Masked<T>` columns dump as plaintext or ciphertext (sensitive-data leak into backup JSON) is decided in the ORM's type layer, not in backup.rs — could not verify. Worth its own check given "sensitive data: YES".

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Fix the `REFERENCES ...("id")` hardcode via `fk_target_pk` (#1) — one-line change plus a test with a String-PK target.
2. Demote the shape-match rename to a suggestion; emit drop+create by default (#4).
3. Backfill `UPDATE ... WHERE col IS NULL` before `SET NOT NULL` / the SQLite copy (#5).
4. `seq = max(prefix)+1` and duplicate-prefix detection in `make_in` (#8, partial).
5. Escape identifiers in the M2M/constraint-name/multi-index raw DDL (#14).
6. Add the `backend.sqlite_in_prod` and secret-key-length checks (#13, partial).

**Short term (< 2 weeks)**
7. Rework `diff_columns`/dance so combined same-table changes apply (#2), and rebuild indexes + `unique_together` after the SQLite dance (#10).
8. Topo-sort + single transaction + sequence reset in `backup::load` (#3).
9. Advisory lock around `migrate` (#7).
10. Verify `snapshot_hash` on applied files as a fifth drift state (#9); make drift alias-aware (#8).
11. `migrations.pending` boot warning; wildcard-hosts warning (#13).
12. Keyword-escaped idents + composite-PK diagnostics in inspectdb codegen (#15); alias-aware `record_applied`/`fake_*` (#17); duplicate-table detection in `load` (#18).

**Structural (needs design)**
13. Destructive-op gating at apply time (`--allow-destructive` or interactive confirm) (#6).
14. Either implement `depends_on` (write it at `make` time from the previous head, verify it at `run` time) or remove the field (#8).
15. Constraint/FK/UNIQUE introspection in inspectdb, and a guard preventing the recreation dance from running on constraint-lossy snapshots (#11).
16. Streaming dump/load (#12).
17. Decide and document the Masked-field ↔ backup policy (Blind spots).

## Docs updated

All three edits align `documentation/docs/v0.0.1/migrations/` pages with the code as read; no code was changed.

1. **managed-migrations.mdx** — the `makemigrations` CLI section claimed "No flags." but `makemigrations --empty <plugin>` exists (migrate.rs:1364–1412, `MigrateError::UnknownPlugin` at 1204–1207, CLI at umbral-cli/src/lib.rs:490) and is documented on the data-migrations page. Replaced with a sentence documenting `--empty`.
2. **adding-not-null-columns.mdx** — the final caveat claimed `#[umbral(default = "...")]` strings are "passed through verbatim to the DDL as literal SQL. Use single quotes around string defaults yourself if needed (`default = "'pending'"`)". The code passes the value to sea-query's `def.default(...)` which renders a *quoted, escaped string literal* (`DEFAULT 'hello'` — pinned by the test at migrate.rs:5084–5099); following the old advice would store a value with embedded quote characters. Rewrote the caveat to describe the actual quoting behavior and to warn that function-call defaults are not expressible via the attribute.
3. **inspectdb.mdx** — two claims contradicted the code: (a) "VARCHAR(n) width is recorded as a comment" — `map_sqlite_type` strips the `(n)` (inspect.rs:546–551) and `render_one_struct` emits no comments (625–645); removed. (b) The callout said tables with unknown SQL types "are excluded automatically" — an unknown column type actually aborts the whole run with `UnsupportedColumnType` (inspect.rs:514–518, 365–377), which the same page's prose states correctly; fixed the callout to match.
