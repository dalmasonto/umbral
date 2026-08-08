# Migration engine evolution: online orchestration, rollback/targeting, and RunCode

| | |
|---|---|
| **Status** | Draft (design sketch for gaps5 #25 / #26 / #27) |
| **Date** | 2026-08-08 |
| **Owns** | The forward design for three migration-engine capabilities that do not exist yet. Nothing here has shipped. |
| **Companions** | `docs/specs/06-migration-engine.md`, `crates/umbral-core/src/migrate.rs`, `crates/umbral-cli/src/lib.rs` |

This is a **draft**. It records the intended shape of three features so a later implementation PR has a target to build against. Numbers cited (`#25`, `#26`, `#27`) are gaps5 tracker entries. None of the surfaces described below are implemented today; where the text says "would" or "will", read it as a proposal, not a promise.

## Where the engine is today (the real baseline)

Everything in this section is true of the engine as it ships, and the three designs build on it rather than replace it.

- **The operation type is `Operation`**, a `#[serde(tag = "kind")]` enum in `crates/umbral-core/src/migrate.rs`. Its shipped variants are: `CreateTable`, `DropTable`, `CreateView`, `DropView`, `AddColumn`, `DropColumn`, `AlterColumn` (nullable flips only), `RenameTable`, `CreateM2MTable`, `DropM2MTable`, `RenameColumn`, `SetColumnComment`, `RunSql`, `AddIndex`, `DropIndex`. There is **no `RunCode` variant** yet, and no reverse-apply path.
- **A migration is one JSON file** carrying an ordered `Vec<Operation>` plus a post-migration schema snapshot. The engine renders each op to SQL via the active backend and runs the file's ops in declaration order inside **one transaction per migration file** (SQLite's `AlterColumn` recreation dance runs inside that same transaction).
- **Applied migrations are recorded** in the `umbral_migrations` tracking table with columns `(plugin, name, applied_at, snapshot_hash)`. Apply is strictly forward: there is no code path that un-applies a migration. `record_applied` inserts one row after a migration's ops succeed.
- **The CLI (`crates/umbral-cli/src/lib.rs`) `migrate` command** exposes only forward-oriented flags: `--fake` (record without running SQL), `--fake-initial`, `--allow-drift`, `--allow-destructive`, and `--allow-in-memory`. There is **no `--target`, no `--reverse`, no rollback**. `RunSql` already carries an optional `reverse_sql` string, documented as "used by a future `migrate --reverse`", but nothing consumes it today.
- **`checkmigrations` already classifies** every pending op as `Safe` / `Warning` / `Unsafe` via `classify_operation` returning `OpSafety`, with an expand-contract note on each non-safe op, and exits non-zero on any `Unsafe` (or any `Warning` under `--strict`). This is the read-only CI gate the online-migration design (#25) extends.
- **Schema-per-tenant is already wired**: the schema-migrate loop applies each op once per tenant schema under a `<schema>, public` search_path, so a `RunSql` writes tenant rows while reading shared `public` lookup tables.

The three features below are additive. They do not change how an existing migration file applies.

## gaps5 #25: zero-downtime online migration orchestration

### Problem

`checkmigrations` tells an operator whether a batch of pending ops is blue-green safe, but it stops at classification. It does not help the operator turn an unsafe change into a safe sequence, and it does not model the multi-deploy choreography (expand, migrate data, contract) that a real online change needs. A `DROP COLUMN` is correctly flagged `Unsafe`, but the framework offers no structured way to stage the safe version of that intent across releases.

### Design sketch

Build an **expand/contract phase model** on top of the existing `OpSafety` classification, so a single logical change compiles into an ordered set of **phase plans**, each of which is independently deployable and independently safe.

- **Phase tags on operations.** Introduce a phase annotation the autodetector can attach when a change is decomposable: `Expand` (additive, old and new code both run against it), `Backfill` (data movement, no schema-breaking effect), `Contract` (the destructive tail, safe only once no old code remains). This reuses `classify_operation`: `Expand` ops are exactly today's `Safe` tier, `Contract` ops are the `Unsafe` tier, `Backfill` is where `RunSql` / the future `RunCode` (#27) live.
- **Dual-write / read-both windows.** For a column type change or a rename, the safe decomposition is: add the new column (expand), dual-write both columns from application code, backfill the old rows, switch reads, then drop the old column (contract). The engine cannot write the application's dual-write code, but it can **emit the phase plan** (which migration belongs to which phase, and the gate between them) and can generate the `AddColumn` + backfill `RunSql`/`RunCode` + `DropColumn` migrations as separate files rather than one unsafe `AlterColumn`.
- **Background backfills.** A large backfill must not run inside the one-transaction-per-migration model, because a single long transaction locks the table and blows the statement timeout. The design routes backfills through a **batched, resumable op** (the `RunCode` design in #27 supplies the batching + checkpoint primitive) marked as a phase that `migrate` can run out-of-band or that the tasks plugin can drive.
- **Rollout gates.** Between phases the plan records a **gate**: a human-or-CI acknowledgement that the prior phase's deploy is fully rolled out (no old code left) before the contract phase is allowed to apply. This is metadata in the phase plan, enforced by the CI phase-plan check, not a lock the engine holds.
- **CI phase plans.** Extend `checkmigrations` to emit a machine-readable phase plan (JSON) alongside its human report: for a batch of pending migrations, which phase each belongs to, which gates sit between them, and which phases are safe to apply in the current deploy. CI consumes this to refuse a deploy that would apply a contract phase before its expand phase has been confirmed live.

### Open questions

- Where the phase tag lives: on the `Operation` enum, on the migration-file header, or as a derived classification computed at plan time. Deriving it keeps old on-disk migrations valid and avoids a data-format change.
- Whether dual-write windows need any framework support beyond documentation and the split migration files, or whether a model-level `#[umbral(dual_write = ...)]` affordance is worth it.

## gaps5 #26: migration rollback and targeting CLI

### Problem

`migrate` only moves forward. There is no `migrate <target>` to roll the database to a named migration, no way to reverse the most recently applied migration, and no enforcement that a migration is even reversible before it is applied. The `reverse_sql` field on `RunSql` is dead metadata. An operator who applies a bad migration has no first-class un-apply; they hand-write a new forward migration to compensate.

### Design sketch

- **`migrate <plugin>/<name>` targeting.** Accept an optional positional target on `migrate`. If the target is ahead of the current state, apply forward up to and including it (a partial forward migrate, which the engine can already express by filtering the pending set). If the target is behind the current state, **reverse** every applied migration after the target, in reverse application order, deleting each `umbral_migrations` row as its ops un-apply. A bare `migrate <plugin>/zero` reverses the whole plugin.
- **Reverse execution per op.** Each `Operation` variant gains a reverse rendering: `CreateTable` reverses to `DropTable`, `AddColumn` to `DropColumn`, `AddIndex` to `DropIndex`, `RenameColumn { from, to }` to `RenameColumn { to, from }`, `CreateM2MTable` to `DropM2MTable`, `CreateView` to `DropView`, and so on. The reverse of a migration is its ops rendered in reverse order inside one transaction, symmetric with the forward apply.
- **Reversible-op metadata enforcement.** Some ops are inherently irreversible: `DropTable` and `DropColumn` destroy rows the reverse cannot reconstruct, and a `RunSql` with `reverse_sql: None` declares itself irreversible. The engine classifies each op as reversible or not (this reuses the `OpSafety::Unsafe` "irreversible" reasons already written in `classify_operation`). A reverse that hits an irreversible op **stops with a clear error** naming the migration, the op, and why it cannot be reversed, rather than silently doing a lossy best-effort.
- **Irreversible-op handling.** For the legitimate "I know it is destructive, do it anyway" case, an explicit `--allow-irreversible` flag (parallel to today's `--allow-destructive`) lets a reverse proceed past a `DropTable` by simply un-recording it, with a loud warning that the dropped data is gone. Without the flag, the reverse refuses. `RunSql`'s `reverse_sql` finally becomes live: it is the forward-authored un-apply statement the reverse executes.
- **Recording.** Reverse deletes the tracking row; the snapshot chain is walked backward so `migrate` afterward sees the target migration as the new head.

### Open questions

- Whether `--reverse` (reverse exactly the last N) is worth having alongside `migrate <target>`, or whether the target form subsumes it.
- Cross-plugin ordering on reverse: a reverse must un-apply a dependent plugin's migration before the dependency's, the mirror of the forward FK ordering.

## gaps5 #27: RunCode / Rust data migrations

### Problem

Data migrations today are hand-authored `RunSql`: a raw SQL string the author owns for portability across SQLite and Postgres. That is a real escape hatch, but it is exactly the "plugin code writing raw SQL" the project's ORM rule pushes against. A non-trivial backfill (read rows, transform in Rust, write them back, in batches, idempotently) has no first-class home.

### Design sketch

Add a **`RunCode` operation** that runs a registered Rust closure instead of a SQL string, with the same per-migration transaction guarantees as every other op.

- **The op.** `RunCode { name: String }` where `name` keys into a registry of migration functions the plugin registers at build time. The migration file stores only the name (a closure cannot be serialized to JSON), so the file stays a portable, replayable record and the code lives in the plugin. A missing registration at apply time is a hard error, not a skip.
- **Transaction access.** The closure receives a handle to the same transaction the migration file runs in, so its writes commit or roll back atomically with the rest of the migration. On the reverse path (#26) a `RunCode` may register a paired reverse function; absent one it is irreversible, handled exactly like `RunSql { reverse_sql: None }`.
- **Typed model APIs.** The whole point over `RunSql` is that the closure uses the ORM: `Post::objects().filter(...).update_values(...)`, `bulk_create`, the typed QuerySet. It runs against the active backend through the same ambient-pool dispatch every other ORM call uses, so one closure works on SQLite and Postgres. A **historical-model** concern applies: a data migration should see the schema as it was at that migration, not today's model. First cut can use the live typed model with a documented caveat; a later refinement reconstructs a model view from the migration's snapshot.
- **Batching helper.** A `batched(query, size, |batch| ...)` helper walks a large table in chunks so the backfill does not build one giant transaction. Under #25 this is the primitive that makes background backfills resumable: each batch checkpoints its progress.
- **Idempotency helper.** A data migration must be safe to re-run after a partial failure. The helper offers a checkpoint (a marker row, or a `WHERE not-yet-migrated` predicate the author supplies) so re-running resumes rather than double-applying. Idempotency is the author's contract; the helper makes the common shape easy.
- **Tenant-aware execution.** `RunCode` inherits the existing schema-per-tenant loop: the closure runs once per tenant schema under that schema's search_path, so it writes tenant rows while reading shared `public` lookups, matching `RunSql`'s current tenant semantics. The closure does not need to know it is running per-tenant; the ambient pool and search_path carry that.

### Open questions

- Registry mechanics: an inventory-style collect at startup vs an explicit `Plugin` hook that hands the engine its named migration functions. The explicit hook is more in keeping with the plugin contract.
- Historical models: how far to go on reconstructing a point-in-time model. The snapshot has the column shapes; deriving a usable typed API from it is non-trivial and can be deferred behind the live-model first cut.

## Sequencing

The three interlock. #27's batching primitive is what makes #25's background backfills work, and #26's reverse-op metadata is what lets #25's contract phase be rolled back if a deploy is aborted. A reasonable order is: #26 first (reverse rendering + `migrate <target>`, the smallest self-contained piece that makes `reverse_sql` live), then #27 (`RunCode` + batching/idempotency), then #25 (phase plans on top of both). Each lands behind its own flag and changes no existing migration file's apply behaviour.
