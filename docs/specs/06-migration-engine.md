# 06 — Migration engine

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M5 (model snapshot, basic autodetection, tracking table, `migrate` and `makemigrations` CLI) |
| **Companions** | `00-overview.md`, `02-plugin-contract.md`, `04-orm-model-and-fields.md`, `05-backends-and-system-check.md`, `07-inspectdb.md` |

## Purpose

**The north star.** This spec defines the declare → migrate → change → migrate loop end-to-end. From the user's perspective:

1. Declare or change a model.
2. Run `cargo run -p umbra-cli -- makemigrations` and a new migration file appears, generated from a diff between the current models and the last snapshot.
3. Run `cargo run -p umbra-cli -- migrate` and pending migrations apply to the database.
4. Change the model again. Repeat.

What this spec owns:

- The **migration file format** (one JSON file per migration, carrying ordered operations plus a snapshot of the post-migration schema).
- The **operation catalogue** (`CreateTable`, `DropTable`, `AddColumn`, `DropColumn`, `AlterColumn`, indices, constraints, `RunSql`, `RunCode`). **What shipped at M5:** table-level only (`CreateTable`, `DropTable`). Column-level ops (`AddColumn`, `DropColumn`, `AlterColumn`), index / constraint ops, and the `RunSql` / `RunCode` data-migration escape hatches are deferred to **M8** alongside the rename-detection and data-preserving-alter hardening. Until M8 lands, changing a field on an existing model produces `MigrateError::UnsupportedChange("column changes on {model}: deferred to M5.1")`. The slot is called "M5.1" in code comments; the actual milestone is M8 per `arch.md §7`.
- The **autodetection algorithm**: diff the current `FIELDS` against the latest snapshot, produce ordered ops, write a new migration file.
- The **tracking table** (`umbra_migrations`) and the rules for what counts as "applied".
- The **plugin-aware ordering**: cross-plugin FKs and the rule that a plugin's migrations apply after its dependencies'.
- The **CLI surface**: `makemigrations`, `migrate`, `showmigrations`, `sqlmigrate`.

What this spec **does not** own:

- Rename detection vs drop-and-add. M5 emits drop-and-add. Rename detection is M8 hardening.
- Data migration patterns beyond the `RunSql` and `RunCode` escape hatches.
- Squashing migrations or `--fake`. PRD F-MIG-6 P2; deferred.
- Introspecting an existing database into models. That's `07-inspectdb.md`.

## Concepts

### Migration files

One migration is one JSON file inside the plugin's `migrations/` directory:

```
plugins/umbra-auth/migrations/0001_initial.json
plugins/umbra-auth/migrations/0002_add_email_index.json
my-app/blog/migrations/0001_initial.json
```

The filename is `<NNNN>_<short_name>.json`, `NNNN` is a four-digit sequence number, `short_name` is snake_case generated from the dominant operation.

A migration file's shape:

```json
{
  "id": "0001_initial",
  "plugin": "blog",
  "depends_on": [
    { "plugin": "auth", "migration": "0001_initial" }
  ],
  "operations": [
    { "kind": "CreateTable", "name": "post", "columns": [/* … */], "indexes": [/* … */], "foreign_keys": [/* … */] }
  ],
  "snapshot_after": {
    "models": {
      "post": { /* full FieldSpec for each field */ }
    }
  }
}
```

- `id` matches the filename (minus extension).
- `plugin` matches the owning `Plugin::name()`.
- `depends_on` lists cross-plugin and within-plugin predecessors. Within-plugin predecessors are implicit (the prior numeric file); cross-plugin predecessors are explicit so the engine knows the global order.
- `operations` is the ordered list applied when this migration runs. Each operation is reversible (it knows its inverse), except `RunSql` and `RunCode` which carry an explicit `reverse` field that is either an inverse operation or `null` (irreversible).
- `snapshot_after` is the source of truth for the schema state once this migration has run. The next `makemigrations` reads it to know what to diff against. Without snapshots, `makemigrations` would have to replay every prior migration in memory; with them, it reads one file.

### Operation catalogue

| Operation | Inverse | Notes |
|---|---|---|
| `CreateTable` | `DropTable` | Carries columns, indexes, foreign keys, the primary key column. |
| `DropTable` | `CreateTable` (reconstructed from snapshot) | Irreversible if executed against a table the snapshot doesn't know about. |
| `AddColumn` | `DropColumn` | Nullable columns add cleanly. Non-nullable additions require a `default` to fill existing rows; the engine refuses to generate a non-nullable `AddColumn` without one. |
| `DropColumn` | `AddColumn` (reconstructed from snapshot) | |
| `AlterColumn` | `AlterColumn` (the previous shape) | Carries `from` and `to` for nullable, default, and type. Type changes that aren't safely castable surface as a `MigrationError::UnsafeAlter`. |
| `AddIndex` / `DropIndex` | each other | |
| `AddConstraint` / `DropConstraint` | each other | Unique, check, foreign-key constraints. |
| `RunSql` | An explicit `reverse_sql` or `null` (irreversible) | The data-migration escape hatch. Backend-portable users avoid this; users on Postgres only reach for it freely. |
| `RunCode` | An explicit reverse function reference or `null` | A migration step is a Rust function `fn(&mut MigrationContext) -> Result<()>`. Useful when the migration needs to read existing rows, transform them, and write them back. The function is registered in the plugin's `migrations` module so the JSON can reference it by name. |

Every operation knows how to render itself for the active backend via the `DatabaseBackend` trait (`05-backends-and-system-check.md`). The migration file is dialect-neutral; rendering happens at apply time.

### Tracking table

```sql
CREATE TABLE umbra_migrations (
    id BIGSERIAL PRIMARY KEY,
    plugin TEXT NOT NULL,
    name TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    snapshot_hash TEXT NOT NULL,
    UNIQUE (plugin, name)
);
```

`migrate` records a row per applied migration, with the SHA-256 of the migration file's `snapshot_after` for drift detection. If a migration was applied but its file has since been edited (different hash), `migrate` refuses to proceed and reports the drift. Users fix this by reverting the file edit or, in exceptional cases, by issuing a `migrate --force-reseal <plugin> <migration>` (M5+ when needed).

The table is created by the very first `migrate` run, before any other migration applies. This is a chicken-and-egg case that we resolve by hard-coding the create statement in the migrate command itself.

### Plugin ordering

Three rules combined:

1. **Within a plugin**, migrations are ordered by their numeric prefix.
2. **Across plugins**, a plugin's migrations apply only after its `Plugin::dependencies()` have caught up. (`umbra-admin` depends on `umbra-auth`; auth's migrations run first.)
3. **Cross-plugin migration-level dependencies** declared in a migration file's `depends_on` add finer-grained ordering. (Plugin `blog`'s `0001_initial` depends on plugin `auth`'s `0001_initial` because `blog.post.author_id` FKs to `auth.user.id`.)

Rules 2 and 3 are joined: the engine builds a global DAG of (plugin, migration) nodes, topologically sorts, and applies in that order. A cycle is `MigrationError::CycleInDependencies { cycle }`.

### Autodetection: the diff

`makemigrations` builds two `SchemaSnapshot` values:

- `previous`: from the most recent migration file's `snapshot_after`, per plugin, then unioned into a global view.
- `current`: from the running framework's `FIELDS` per model.

It walks both and produces operations:

| Difference | Operation emitted at M5 |
|---|---|
| Table in `current`, not in `previous` | `CreateTable` |
| Table in `previous`, not in `current` | `DropTable` |
| Column in `current`, not in `previous` (within an existing table) | `AddColumn` |
| Column in `previous`, not in `current` | `DropColumn` |
| Column with same name, different `FieldSpec` | `AlterColumn`, carrying the from/to shapes |
| Index in `current`, not in `previous` | `AddIndex` |
| Index in `previous`, not in `current` | `DropIndex` |
| Constraint changes (unique, check, foreign-key) | `AddConstraint` / `DropConstraint` pairs |

At M5, **rename detection is not performed**: a column renamed from `body` to `content` produces `DropColumn("body")` + `AddColumn("content")`. The data is lost. The user can override by editing the migration file to use `AlterColumn(rename)` once the M8 detector lands. This is intentional; the cases Django spent years on (rename ambiguity, data-preserving alters, complex constraint changes) are iterated at M8, not gated on at M5.

After diffing, the engine orders operations within the migration so each one is valid when applied:

- `CreateTable` before any `AddColumn` that references the new table.
- All `DropColumn` before the `DropTable` for the same table.
- All `AddIndex` after the `CreateTable` they live on.
- All `DropConstraint` before `DropColumn` if the constraint references the column.

### CLI commands

```
cargo run -p umbra-cli -- makemigrations [PLUGIN]
cargo run -p umbra-cli -- migrate [PLUGIN[:MIGRATION]]
cargo run -p umbra-cli -- showmigrations
cargo run -p umbra-cli -- sqlmigrate PLUGIN MIGRATION
```

- **`makemigrations`** generates new migration files. With no argument, runs over every registered plugin. The filenames it picks come from a deterministic naming rule (the dominant op's table name plus suffix), so two developers running `makemigrations` against the same model change get the same filename.
- **`migrate`** applies pending migrations in topological order. With no argument, brings every plugin up to the latest. With `PLUGIN`, brings one plugin up. With `PLUGIN:MIGRATION`, applies (or rolls back to) a specific target migration.
- **`showmigrations`** prints the per-plugin migration list and a tick next to applied ones, mirroring Django's output.
- **`sqlmigrate`** prints the SQL a specific migration would emit against the active backend without applying it. Useful for review.

## API-shape sketch

A migration applied via the engine, end-to-end (sketch):

```rust
pub async fn migrate(target: MigrateTarget) -> Result<MigrateReport> {
    let backend = umbra::db::backend();
    let pool = umbra::db::pool();

    ensure_tracking_table(&pool, backend).await?;

    let migrations = collect_migrations_from_plugins()?;        // reads every plugin's migrations/ dir
    let plan = build_plan(&migrations, target, &applied_set(pool).await?)?;

    let mut report = MigrateReport::default();
    for (plugin, migration) in plan.iter() {
        let mut tx = pool.begin().await?;
        for op in &migration.operations {
            op.apply(&mut tx, backend).await?;
        }
        record_applied(&mut tx, plugin, migration).await?;
        tx.commit().await?;
        report.applied.push((plugin.clone(), migration.id.clone()));
    }
    Ok(report)
}
```

Each migration runs inside a transaction. The transaction wraps both the schema operations and the tracking-table insert, so a failure during a multi-op migration rolls back atomically. (On Postgres, most DDL is transactional. On SQLite, DDL is also transactional. A migration that includes a non-transactional operation surfaces a check at file-load time.)

## Mechanics and invariants

### One migration is one transaction

A migration's operations apply atomically. If op N fails, ops 1..N-1 roll back. The tracking-table insert lives inside the same transaction, so the record-keeping always matches the actual database state.

Exception: migrations explicitly marked `transactional: false` in the file run outside a transaction (e.g. `CREATE INDEX CONCURRENTLY` on Postgres). The engine surfaces a warning at load time and the user accepts the trade-off knowingly. M5 ships with the default `transactional: true` only; the opt-out lands when a real need surfaces.

### Snapshots are the source of truth

Future `makemigrations` runs never re-execute prior migrations to figure out the schema state. They read `snapshot_after` from the latest migration file per plugin. Snapshots are the contract that lets migration files be small (only the diff) and fast to process (linear in migration count, not in DB rows).

### The chicken-and-egg of `umbra_migrations`

The tracking table itself can't be a migration of any plugin, because every plugin's migrations need the tracking table to exist before they can run. Resolution: `migrate` checks for the table before doing anything else. If it doesn't exist, `migrate` creates it directly via the backend's DDL renderer, then proceeds. The table's existence is the engine's responsibility, not a plugin's.

### Drift detection

`snapshot_hash` in the tracking table catches the "edited a migration file after it ran" case. When `migrate` runs, it walks applied rows and recomputes each file's snapshot_hash. A mismatch is a `MigrationError::DriftDetected { plugin, migration }` that names the file. Users either revert the edit or, knowingly, re-seal the hash. Drift is a real production hazard; the engine surfaces it before damage.

### Cross-plugin FK ordering at M8 (forward reference)

M5 ships with `Plugin::dependencies()` driving the order. At M8, the autodetector becomes smart enough to walk FK targets directly: if `blog.post.author_id` FKs to `auth.user.id`, the engine can derive the cross-plugin migration dependency without requiring the plugin author to put `"auth"` in `dependencies()` at all. M5 still requires the explicit declaration; M8 adds the inference and keeps the explicit declaration as a shortcut for clarity.

## Trade-offs and alternatives considered

**JSON migration files vs Rust migration modules.** Django uses Python files because migrations sometimes need code. Rust migration modules would mean every migration is a .rs file checked in and compiled into the binary. JSON files are programmatically easier to diff, easier to inspect by hand, and harder to break by editing. The `RunCode` operation references named Rust functions when a data migration is needed; that's the only place real Rust code lives in the migration pipeline.

**Snapshots in every file vs reconstructing on demand.** Reconstructing the schema state by replaying every migration would mean `makemigrations` cost scales with migration count. Storing the post-state in each file makes it O(1) to read the latest. The cost is bigger migration files; that's fine because they're JSON and disk is cheap.

**One file per migration vs one file per plugin.** One file per plugin would put every change in one place, but conflicts on every change. One file per migration is what Django does and what version control wants.

**At M5, drop+add for renames vs prompt the user.** Django's `makemigrations` interactively asks "did you rename `body` to `content`?" That UX is hard to do well in a non-Python ecosystem. M5 says "drop+add by default; edit the file to use `AlterColumn(rename)` if you need to preserve data." M8 introduces the heuristic detector (Hamming distance of `FieldSpec` shapes, name similarity); the spec note already plans for it.

**Tracking-table location: shared `umbra_migrations` vs per-plugin tracking tables.** A shared table is much easier to reason about (one row per applied migration, anywhere). Per-plugin tables would let plugins manage their own state but force the engine to query N tables to know "has X been applied?". The shared design wins on simplicity and the cost (one table the engine owns) is trivial.

## Open questions

- **Exact `RunCode` registration shape.** A migration file references a function by name; the plugin's `migrations` module needs to publish a registry mapping names to function pointers. The exact API (proc macro that auto-registers? trait the user impls?) is open. Pick at M5 when the first data migration needs to be written.
- **`AlterColumn` for non-castable type changes.** Changing a column from `i32` to `String` is not safely automatic. The engine should produce a multi-step migration (add new column, RunSql to copy values, drop old, rename new) when the user explicitly opts in. M5 surfaces `MigrationError::UnsafeAlter` and refuses; M8 introduces opt-in synthesis.
- **Snapshot format versioning.** Adding a new `FieldSpec` field changes the snapshot shape. Old snapshots need to be readable forever. Strategy: include a `snapshot_format_version: N` field in every snapshot; the engine has a chain of upgraders that walk older snapshots forward. Implement at M5 entry.
- **`--fake` and `--fake-initial`.** Django supports marking a migration as applied without actually running it (useful when porting in via `inspectdb`). Hooks for this lurk in `07-inspectdb.md`. Pick the exact CLI flag and the safety guards (don't fake against an inconsistent state) at M6.

## Cross-links

- `FIELDS` and `FieldSpec` (the source for snapshots): `04-orm-model-and-fields.md`.
- The backend abstraction that renders SQL: `05-backends-and-system-check.md`.
- `Plugin::migrations()` and `Plugin::dependencies()`: `02-plugin-contract.md`.
- Where the snapshot ends up when porting an existing DB: `07-inspectdb.md` builds the equivalent of "migration 0001_initial" from an introspected schema and feeds it into the same engine.
- Rename detection and data-preserving alters: deferred to M8; this spec carries the open question and the engine has the seams to accept the M8 additions without restructuring.
- The CLI binary `umbra-cli` is where the subcommands live; full CLI surface gets a spec when the command list grows past these four.
