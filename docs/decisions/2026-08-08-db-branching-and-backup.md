# Database branching, preview environments, and PITR-grade backup

Status: draft for ratification (proposes the design for gaps5 #29 and gaps5 #30; the final call is the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #29 (tf #242), planning/gaps5.md #30 (tf #243). This records the accepted design target; implementation and docs/runbook publication remain tracked on the tasks.

## Scope and posture

Two data-ops gaps that organizations expect before they self-host on umbral:

- **#29 (tf #242): database branching / preview environments.** Per-PR ephemeral databases, a shadow database for migration verification, schema diffs, seed loading, and teardown.
- **#30 (tf #243): PITR-grade backup and recovery.** Scheduled encrypted backups, WAL archiving with point-in-time recovery, restore drills, retention, and restore verification.

Both are **Stage 2 self-hosted platform** concerns per `docs/decisions/2026-08-08-product-north-star.md`. The honest framing for both: umbral is framework-first, you own the runtime, and the database itself (Postgres) is the source of truth for physical operations. So the deliverable here is **recipes and a runbook that compose umbral's real CLI with standard Postgres tooling**, plus a thin optional plugin where umbral can add value it uniquely holds (the model registry, the migration engine, the beat scheduler). We do not reimplement `pg_dump`, WAL-G, or Neon. We wire them.

This design deliberately leans on commands that already exist. The real umbral CLI surface these recipes build on (from `crates/umbral-cli/src/lib.rs`):

- `makemigrations` (with `--empty <plugin>`), `migrate` (with `--fake`, `--fake-initial`, `--allow-drift`, `--allow-destructive`, `--allow-in-memory`), `showmigrations`, `checkmigrations [--strict]`, `squashmigrations <plugin>`.
- `inspectdb --output <dir> [--mark-applied]`.
- **`dumpdata --output <path>`** and **`loaddata <input>`** - the logical, backend-portable, model-aware JSON snapshot pair (backed by `umbral::backup::dump_to_path` / `load_from_path`, carrying an `umbral_dump_version` so a forward-incompatible dump fails loudly).
- `importcsv <table> <file>`, `typegen`, `maskkeygen`, `serve`, `dev`.

There is no `umbral db`, `umbral backup`, or `umbral branch` command today. Where this doc sketches one, it is labelled a **sketch** and is explicitly not-yet-built.

---

## Part 1 - Database branching and preview environments (gaps5 #29)

### The honest boundary

True database branching (instant, copy-on-write forks of a live database) is a **storage-engine** feature, not something a framework can synthesize. It comes from:

- A managed provider that implements it: **Neon** (`neonctl branches create`), **Supabase** branching, PlanetScale-style branches, or a cloud snapshot/restore (RDS snapshot to a new instance).
- Or **scripting** it yourself: `pg_dump` the parent into a freshly-created database, or `CREATE DATABASE ... TEMPLATE ...` for a same-cluster copy.

umbral's contribution is not the fork primitive. It is the **four things you do to a branch once you have one**: create-and-migrate it, seed it, diff its schema against `main`, and tear it down. Those are exactly the operations umbral's migration engine and `dumpdata`/`loaddata` already own. So the branching story is: "bring your own fork primitive; umbral drives the schema and data lifecycle on top of it."

### Recipe A - ephemeral per-PR database, fully scripted (no managed provider)

The zero-dependency path. Every PR gets its own database in a shared Postgres cluster, created from scratch, migrated by umbral, seeded, and dropped when the PR closes. This is a CI recipe, not a framework feature.

```bash
#!/usr/bin/env bash
# ci/pr-db-up.sh - create + migrate + seed an ephemeral DB for one PR.
set -euo pipefail

PR="${PR_NUMBER:?set PR_NUMBER}"
BRANCH_DB="app_pr_${PR}"
ADMIN_URL="${ADMIN_DATABASE_URL:?postgres URL with CREATEDB rights}"

# 1. Create the branch database (empty). psql against the maintenance DB.
psql "$ADMIN_URL" -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"${BRANCH_DB}\";"

# 2. Point umbral at it and bring the schema up from zero.
export UMBRAL_DATABASE_URL="postgres://…/${BRANCH_DB}"
cargo run -- migrate

# 3. Seed it. Prefer a checked-in dumpdata snapshot so every preview is identical.
#    (See "Seeding" below for the three seed sources.)
cargo run -- loaddata fixtures/preview-seed.json

echo "preview DB ready: ${BRANCH_DB}"
```

```bash
#!/usr/bin/env bash
# ci/pr-db-down.sh - tear the ephemeral DB down when the PR closes.
set -euo pipefail
PR="${PR_NUMBER:?set PR_NUMBER}"
BRANCH_DB="app_pr_${PR}"
ADMIN_URL="${ADMIN_DATABASE_URL:?}"

# FORCE terminates lingering connections (Postgres 13+); without it a stray
# connection from a still-running preview app blocks the DROP.
psql "$ADMIN_URL" -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS \"${BRANCH_DB}\" WITH (FORCE);"
```

Wire both into the CI provider's PR-opened and PR-closed events. The migrate step is the same `cargo run -- migrate` you run in production, so a PR that breaks migrations fails the preview build for the same reason it would fail a real deploy. That is the point: **existing rows are the test** applies here too, so if you seed from a production-shaped snapshot, a bad data-migration surfaces in the preview, not in prod.

For same-cluster speed, `CREATE DATABASE branch TEMPLATE parent` clones an existing database's contents in one statement (the parent must have no other connections during the copy). Useful when you want the preview to start from a real-data copy rather than a seed file, but note it copies the whole database, so it is only appropriate for non-sensitive datasets or after a scrubbing pass.

### Recipe B - ephemeral database on a managed branching provider

When the team already runs on **Neon** (or similar), lean on the provider's real copy-on-write branch and let umbral only drive migrate + seed:

```bash
# 1. Provider creates the branch (instant, copy-on-write from `main`).
neonctl branches create --name "pr-${PR_NUMBER}" --parent main
BRANCH_URL="$(neonctl connection-string pr-${PR_NUMBER})"

# 2. umbral applies whatever migrations this PR adds on top of the fork.
export UMBRAL_DATABASE_URL="$BRANCH_URL"
cargo run -- migrate

# 3. Teardown on PR close.
neonctl branches delete "pr-${PR_NUMBER}"
```

Here the branch already carries production data (copy-on-write), so there is usually no seed step: the PR's migrations run against a real-shaped schema, which is the strongest possible preview. Supabase branching and RDS snapshot-restore fit the same three-step shape with their own CLI verbs.

### Recipe C - the shadow database for `makemigrations` verification

A **shadow database** is a throwaway database used to prove that a generated migration actually applies cleanly from zero before it is committed. It is the migration-engine analogue of a dry run, and it catches the class of bug where `makemigrations` writes a file that autodetection is happy with but Postgres rejects (a `UNIQUE` added over duplicate rows, a `NOT NULL` with no default, a cross-plugin FK ordered wrong).

The recipe, using only real commands:

```bash
#!/usr/bin/env bash
# ci/verify-migrations.sh - prove pending migrations apply from zero.
set -euo pipefail
SHADOW="app_shadow_$$"
ADMIN_URL="${ADMIN_DATABASE_URL:?}"

cleanup() { psql "$ADMIN_URL" -c "DROP DATABASE IF EXISTS \"${SHADOW}\" WITH (FORCE);"; }
trap cleanup EXIT

psql "$ADMIN_URL" -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"${SHADOW}\";"
export UMBRAL_DATABASE_URL="postgres://…/${SHADOW}"

# 1. First prove the committed history applies from an empty database.
cargo run -- migrate

# 2. Then prove there is nothing left un-generated: makemigrations against a
#    fully-migrated shadow MUST report "no changes detected". If it writes a
#    file, the working tree has a model change with no committed migration.
OUT="$(cargo run -- makemigrations)"
echo "$OUT"
if ! grep -q "no changes detected" <<<"$OUT"; then
  echo "error: models drifted from migrations - a migration was not committed" >&2
  exit 1
fi

# 3. Optional CI gate: classify the pending ops for rolling-deploy safety.
cargo run -- checkmigrations --strict
```

Step 2 is the key verification and it uses `makemigrations`'s own `NoChanges` result (it prints `no changes detected`). This is the umbral-native "is the schema in sync with the models" check, and it belongs in CI on every PR. `checkmigrations --strict` layers the zero-downtime classification on top (it exits non-zero on any UNSAFE, or any WARNING under `--strict`).

> Note on today's engine: `makemigrations` autodetection and `migrate` apply are the same code paths CI and production use, so a shadow run is a faithful rehearsal. The shadow database is created and dropped with plain `psql`; umbral does not (yet) own a `--shadow` flag. If we later add one, its job is only to wrap this script so the operator does not hand-manage the scratch database name.

### Schema diff output

Two honest options today, no new engine work required:

1. **umbral-native, model-level.** `cargo run -- makemigrations` against a database migrated to `main` is *itself* the diff: the migration file it writes (or `no changes detected`) is the exact set of operations that move `main`'s schema to the PR's models. To see the diff without committing it, run `makemigrations` on the shadow DB and read the generated file, then discard it. This is the diff expressed in umbral's own `Operation` vocabulary (CreateTable, AddColumn, AddIndex, and so on, as enumerated in the `checkmigrations` op tags).
2. **SQL-level, ground truth.** For a literal DDL diff between two live databases (branch vs main), shell out to an external schema-diff tool (`migra`, `apgdiff`, or `pg_dump --schema-only` of each side piped through `diff`). This is the physical truth including anything applied outside umbral. Recommended as a belt-and-suspenders check in the preview teardown report, not as a gate.

A future **sketch** (`umbral db diff --from <url> --to <url>`) would wrap option 1: introspect both databases via the existing `inspectdb` machinery (`umbral::inspect::introspect_pool_pg`), diff the two `IntrospectedSchema` values through the autodetector, and print the operations. It reuses code that already exists (`inspectdb` + the migration autodetector); it is not built yet.

### Seeding a branch (via the real `loaddata`)

Three seed sources, in increasing fidelity:

- **A checked-in fixture:** `cargo run -- loaddata fixtures/preview-seed.json`. The file is a `dumpdata` envelope (produced once with `cargo run -- dumpdata --output fixtures/preview-seed.json` against a curated database). Deterministic, reviewable, version-controlled. This is the recommended default for Recipe A.
- **A scrubbed production snapshot:** `dumpdata` a production replica, run it through a scrubbing pass (null out PII columns, or rely on `Masked<T>` columns already being ciphertext), commit or store the scrubbed envelope, and `loaddata` it into each preview. `dumpdata`/`loaddata` are model-aware and backend-portable, so a snapshot taken on Postgres loads into a SQLite preview and vice versa.
- **The app's own idempotent seed hook:** if the app wires `seed_on_serve`, the first `serve` against the fresh branch seeds it. Good for reference/lookup data that belongs in code rather than a fixture.

`loaddata` reports rows loaded and warns on tables not in the current schema (a skipped-table warning), so a stale fixture against a newer schema is visible rather than silent.

### The `umbral db branch` sketch (not built)

If demand justifies a first-class command, the smallest useful shape is a thin orchestrator over the recipes above, provider-pluggable so it never hard-codes Neon or psql:

```
umbral db branch create <name> [--from main] [--seed <fixture.json>]
    -> create/fork the database (driver: script | neon | supabase | rds-snapshot)
    -> UMBRAL_DATABASE_URL=<branch>  cargo run -- migrate
    -> loaddata <fixture.json>              (if --seed)

umbral db branch diff <name> [--against main]
    -> introspect both via inspectdb machinery, run the autodetector, print ops

umbral db branch drop <name> [--force]
    -> driver-specific teardown (DROP DATABASE … WITH (FORCE) | neonctl delete | …)
```

Design constraints if we build it: the fork/teardown primitive is a **driver trait** (script, neon, supabase, rds) so umbral stays honest that it does not own the storage fork; migrate/seed/diff go through the existing engine, not new SQL. It is explicitly a Stage 2 convenience wrapper, not a new capability. Until it exists, the scripts in Recipes A-C are the supported path and are what CI should call.

---

## Part 2 - PITR-grade backup and recovery (gaps5 #30)

### The two backup tiers, and where umbral fits each

Postgres backup is two distinct disciplines. umbral relates to them differently:

| Tier | Tool | Granularity | umbral's role |
|---|---|---|---|
| **Logical** | `pg_dump` / `pg_restore`, and umbral's own `dumpdata`/`loaddata` | Whole database or per-table; portable across versions/backends | umbral **participates**: `dumpdata` is a model-aware logical snapshot |
| **Physical + WAL / PITR** | WAL-G, pgBackRest, or `pg_basebackup` + `archive_command` | Byte-level base backup plus continuous WAL; restore to any second | umbral **orchestrates and verifies**, never reimplements |

The key honesty: **`dumpdata` is not PITR.** It is a point-in-time *logical* snapshot, excellent for pre-migration safety and cross-environment seeding, but it cannot restore to an arbitrary second and it locks in read consistency only for the dump's duration. PITR-grade recovery is a physical-backup + WAL-archiving job, and that is owned by Postgres tooling (WAL-G / pgBackRest). This runbook wires that tooling; it does not replace it. The deployment reference already states this boundary (`reference-architecture.mdx` "Postgres backups": "umbral does not manage your database backups; that is the operator's job").

### The backup runbook

#### 1. Logical backups (two flavors, both scheduled)

**Postgres-native (`pg_dump`), the recovery baseline.** Custom format so `pg_restore` can do selective/parallel restores:

```bash
# nightly full logical backup, custom format, then encrypt with age.
pg_dump --format=custom --no-owner --no-privileges "$DATABASE_URL" \
  | age -r "$BACKUP_AGE_RECIPIENT" \
  > "backup-$(date -u +%Y%m%dT%H%M%SZ).dump.age"
# upload to the same S3-compatible bucket used for media / WAL archive.
```

**umbral-native (`dumpdata`), the portable, model-shaped snapshot.** Use this specifically as the **pre-migration safety net**, because it composes with the migration loop and is restorable on any backend:

```bash
# Take immediately BEFORE applying a schema migration in production.
cargo run -- dumpdata --output "pre-migrate-$(date -u +%Y%m%dT%H%M%SZ).json"
age -r "$BACKUP_AGE_RECIPIENT" -o pre-migrate.json.age pre-migrate.json && rm pre-migrate.json
cargo run -- migrate      # if the data-migration goes wrong, loaddata the snapshot
```

This is the concrete realization of the reference architecture's rule "take a backup immediately before applying a schema migration." Restore is `age -d | ... ; cargo run -- loaddata pre-migrate.json`. Both flavors are logical, so neither is a substitute for WAL/PITR below; they cover the "oops, that migration corrupted data" case with a bounded, reviewable artifact.

#### 2. Physical base backup + WAL archiving (the PITR foundation)

PITR needs a base backup plus every WAL segment since. Use **WAL-G** (or pgBackRest); do not hand-roll `archive_command` in production. WAL-G ships base backups and WAL to S3-compatible storage and handles compression and encryption:

```bash
# postgresql.conf
wal_level = replica
archive_mode = on
archive_command = 'wal-g wal-push %p'
archive_timeout = 60          # bound RPO: a WAL segment is shipped at least every 60s

# environment (WAL-G reads these)
WALG_S3_PREFIX="s3://mybucket/pg/"
WALG_LIBSODIUM_KEY_TRANSFORM="..."   # WAL-G-native encryption at rest
```

```bash
# nightly (or more often) base backup, driven by beat or cron:
wal-g backup-push "$PGDATA"
# retention: keep 7 full base backups, prune older WAL:
wal-g delete retain FULL 7 --confirm
```

RPO (how much data you can lose) is bounded by `archive_timeout`; RTO (how long recovery takes) is bounded by base-backup frequency (less WAL to replay). Tune both to the app's tolerance and document the target in the runbook.

#### 3. Encryption

Two layers, both required for "encrypted backups":

- **In transit / at rest of the artifact:** either the tool's native encryption (WAL-G `WALG_LIBSODIUM_KEY`, pgBackRest `repo-cipher`) or an envelope pass with `age`/`gpg` as shown above. The `dumpdata` JSON must be encrypted before it leaves the box - it contains full row data.
- **Field-level, already in umbral:** `Masked<T>` columns are stored as ciphertext, so they are ciphertext inside every backup automatically. Destroying the `UMBRAL_MASK_PRIVATE_KEY` crypto-shreds those columns across all backups at once (see `maskkeygen`). This is complementary to backup encryption, not a replacement: it protects specific PII columns even if the backup encryption key leaks.

Store backup-encryption keys and `UMBRAL_MASK_PRIVATE_KEY` in a real secret manager (Vault, cloud secret manager, sealed CI variable), never in the backup bucket and never in the repo.

#### 4. Scheduling via the beat scheduler

The backup jobs above are cron-shaped, and umbral already ships a cron scheduler: **umbral-tasks beat**. Rather than a separate system crontab, register the logical snapshot as a periodic task so it lives with the app, is visible in the admin, and rides the same "exactly one beat replica" guarantee:

```rust
// A task that shells out to pg_dump / dumpdata and uploads the artifact.
App::builder()
    .plugin(
        TasksPlugin::default()
            .periodic_task::<NightlyLogicalBackup>(
                "nightly_backup",
                Schedule::cron("0 2 * * *"),   // 02:00 UTC daily
                (),
            ),
    )
    .build()?;
```

`beat` claims each due `PeriodicTask` row atomically (a second beat cannot double-fire it), so the backup runs once even across a worker fleet. Physical base backups (`wal-g backup-push`) can also be beat-scheduled, but many operators keep those in the OS scheduler on the database host itself, closer to `$PGDATA`; either is fine, and the runbook should pick one and be explicit. WAL archiving is **not** scheduled - it is continuous via `archive_command`.

#### 5. Retention

- **Logical (`pg_dump`/`dumpdata`):** a tiered policy on the bucket, for example keep nightly for 30 days, weekly for 12 weeks, monthly for 12 months. Enforce with S3 lifecycle rules or the backup task pruning by object age; do not rely on manual cleanup.
- **Physical (WAL-G/pgBackRest):** `wal-g delete retain FULL <n>` (or pgBackRest `repo-retention-full`) prunes old base backups and the WAL they no longer need. The retention count sets your PITR window: retaining 7 full backups with continuous WAL means you can recover to any second within that span.

#### 6. Restore drills and restore verification

An untested backup is a hope, not a backup. Make restore a **scheduled, automated drill**, not a fire-drill:

```bash
#!/usr/bin/env bash
# ops/restore-drill.sh - prove the latest backup restores and the app agrees.
set -euo pipefail
SCRATCH="restore_drill_$(date -u +%Y%m%d)"
ADMIN_URL="${ADMIN_DATABASE_URL:?}"
trap 'psql "$ADMIN_URL" -c "DROP DATABASE IF EXISTS \"${SCRATCH}\" WITH (FORCE);"' EXIT

# --- Physical PITR path (WAL-G): restore base + replay WAL to a target time. ---
# wal-g backup-fetch /var/lib/pg-scratch LATEST
# configure recovery_target_time in the scratch cluster, start it, let it replay.

# --- Logical path (the CI-friendly drill shown here): ---
psql "$ADMIN_URL" -c "CREATE DATABASE \"${SCRATCH}\";"
age -d -i "$AGE_IDENTITY" latest-backup.dump.age \
  | pg_restore --no-owner --no-privileges --dbname="postgres://…/${SCRATCH}"

# --- VERIFICATION: the restore is only successful if the app accepts it. ---
export UMBRAL_DATABASE_URL="postgres://…/${SCRATCH}"

# 1. Schema is in sync with the app's models: makemigrations finds nothing.
cargo run -- makemigrations | grep -q "no changes detected" \
  || { echo "FAIL: restored schema drifted from models" >&2; exit 1; }

# 2. Migration ledger is intact and consistent: showmigrations is all applied.
cargo run -- showmigrations

# 3. Row-count / smoke assertions against known-populated tables (app-specific),
#    e.g. a small Rust/SQL check that critical tables are non-empty.
echo "restore drill PASSED for ${SCRATCH}"
```

The verification step is what turns a restore into a *verified* restore, and it reuses umbral's own commands: `makemigrations` returning `no changes detected` proves the restored schema matches the models, and `showmigrations` proves the migration ledger came back intact (`umbral_migrations` is included in a physical backup; for a `dumpdata` restore, restore or re-`migrate` the schema first, since `dumpdata` is data, not DDL). Run the drill on a schedule (weekly is a good default), fail loudly, and alert on failure. A drill that has not run in a month is a backup you do not have.

> Ordering caveat for `dumpdata` restores: `dumpdata`/`loaddata` carry rows, not schema. The restore sequence is `migrate` (build the schema) then `loaddata` (fill it), exactly as the `loaddata` command's own help states. Physical (WAL-G) restores bring schema and data together and need no separate migrate.

### Optional `umbral-backup` plugin (sketch, not built)

Where a plugin earns its place over a pure runbook: umbral holds three things a shell script does not - the **model registry** (so it knows every table to snapshot and can verify counts), the **migration ledger** (so it can assert a restore is consistent), and the **beat scheduler** (so backups schedule like any other periodic task). A thin `umbral-backup` plugin would wrap, never replace, the external tools:

- `BackupPlugin::default().logical(Schedule::cron(...)).encrypt_to(recipient).store(s3_uri)` - registers a beat-scheduled `dumpdata`-plus-`pg_dump` job that encrypts and uploads, with retention pruning.
- A `restore-verify` command that runs the drill's verification block (the `makemigrations`/`showmigrations`/row-count checks) against a given database URL, so verification is a first-class subcommand rather than a copied script.
- Physical backup and WAL/PITR stay **external** (WAL-G/pgBackRest): the plugin documents and optionally shells out to them, but does not reimplement base backups or WAL shipping. That line is non-negotiable, same as the "do not reimplement primitives" rule for HTTP/SQL/JSON.

Constraints if built: all row-level reads for the logical path go through the ORM / `dumpdata` (no hand-rolled `sqlx::query` in the plugin, per the ORM-only rule); the physical path is a documented shell-out. It is a Stage 2 convenience, gated on real demand.

### Tie-in to the deployment reference

This runbook expands `documentation/docs/v0.0.1/deployment/reference-architecture.mdx` "Postgres backups" (currently the short "umbral does not manage your backups; here is the shape" section that names gaps5 #30 as the tracking item) and `migrations-in-production.mdx` (the "back up before you migrate" rule). When #30 ships as docs, the reference-architecture section should link to a new `deployment/backup-and-recovery.mdx` page carrying the runbook above, and the "backups / PITR target" box in the topology diagram (`reference-architecture.mdx` line 88-89) becomes the WAL-G/pgBackRest + S3 target this doc specifies.

---

## Summary of decisions

- **#29 branching is recipe-first, not a new engine.** Ship CI scripts (create/migrate/seed/drop, shadow-DB verification) that compose `migrate` / `makemigrations` / `loaddata` with a bring-your-own fork primitive (scripted `CREATE DATABASE`, or a managed provider like Neon). True copy-on-write branching stays external. A future `umbral db branch` command is a thin, driver-pluggable wrapper, explicitly deferred.
- **#30 backup is a runbook plus an optional thin plugin.** Logical backups via `pg_dump` and umbral's own `dumpdata` (the pre-migration safety net); physical + WAL/PITR via WAL-G/pgBackRest (external, never reimplemented); encryption via tool-native + `age` plus `Masked<T>` field-level; scheduling via beat; retention via tool policy; and **restore drills whose verification reuses `makemigrations` (`no changes detected`) and `showmigrations`** to prove a restore is not just bytes-back but app-consistent.
- **Both are Stage 2**, both are honest about the framework/Postgres boundary, and both reuse the real CLI rather than inventing parallel machinery.
