# Data Transfer Engine (`umbral transferdata`)

| | |
|---|---|
| **Date** | 2026-08-16 |
| **Status** | Approved — phase 1 building |
| **Authors** | Dalmas Ogembo + Claude |
| **Scope** | A resumable, PK-preserving, streaming row-copy between two umbral databases (env1 → env2). The data half of the inspectdb porting story (features #85), and a general-purpose DB-to-DB move (VPS1 → VPS2, SQLite → Postgres). |

---

## 1. What this is, and is not

**Is:** umbral DB → umbral DB. Both ends share the app's registered schema (same `ModelMeta`). Rows are copied verbatim — **primary keys and foreign keys preserved** — so the object graph is identical on the far side. Safe to interrupt and resume; sized for tens of GB across a slow link.

**Is not:** a Django→umbral converter. That renaming/translation is inspectdb's job (it owns the old→new map); once you've migrated the *schema* into umbral and stood up env2, this moves the *data*. A future `--map` layer could reuse inspectdb's rename map to read a foreign-shaped source, but v1 assumes matching schemas.

## 2. The PK-preservation foundation (the thing to confirm first)

The whole engine rests on inserts keeping their source id. **Confirmed:** `DynQuerySet::insert_json` only omits the PK column when it is an *integer* PK carrying a *sentinel* value (`is_default_pk` → `0` / nil-UUID / empty). Any real id — `10`, a UUID, a slug — falls through and is bound. So `insert_json` already round-trips explicit PKs; no new "force PK" flag is needed. (This also means `loaddata` between two umbral SQLite DBs preserves ids today, which the port relies on.) A regression test pins this so it can't silently break.

## 3. Architecture

Two explicit pools, not the ambient router (the CLI builds its app once; a second pool can't be injected). Everything runs through the ORM's `_in_tx` / explicit-pool entry points so type coercion and PK handling are reused, not reimplemented:

- **Read** source: `DynQuerySet::for_meta(meta).filter(pk > last).order_by(pk).limit(batch).fetch_as_json_on(&source)` — a new explicit-pool read (raw columns, no M2M echo).
- **Write** target: `insert_json_in_tx(&row, &mut target_tx)` — existing, PK-preserving.
- Pools via `db::connect(url)`, txns via `db::begin_sqlite`/`begin_pg(&pool)`.

### 3.1 Ordering — non-linked tables first

Models are copied in **FK-topological order** (Kahn over `ModelMeta.fields[].fk_target`; self-FKs ignored). A table with no outward FK ships first; a child ships only after its parents exist on the target, so the target's FK constraints never reject a row. This directly answers "transfer non-linked tables first."

### 3.2 Streaming — keyset pagination, never OFFSET

Per table: `WHERE pk > :last ORDER BY pk ASC LIMIT :batch`, carrying the last pk forward. Keyset paging is O(batch) per page regardless of table size — `OFFSET` on a 50M-row table is O(n²) and unusable. Only one batch is ever in memory.

### 3.3 Resumability — checkpoint committed *with* the batch

A tooling-owned table on the **target**, `umbral_transfer_state(table_name TEXT PK, last_pk TEXT, done INTEGER)`, created `IF NOT EXISTS` (the schema-DDL exception; same pattern as the migrations ledger). Each batch's inserts **and** its `last_pk` bump commit in **one target transaction**. So:

- Crash mid-batch → the whole tx rolls back → resume re-reads that page cleanly. No partial rows, no double-insert, no PK-conflict on restart.
- Resume reads `last_pk` per table and continues strictly after it; a `done` table is skipped. Exact-once, idempotent, no `ON CONFLICT` needed because the checkpoint is transactional with the data.

This is what makes a 30 GB VPS1→VPS2 move survive a dropped connection: kill it, rerun the same command, it picks up at the last committed page.

### 3.4 Sequence reset (finalization)

After a table completes, its autoincrement cursor must clear the copied ids so future app inserts don't collide:

- **Postgres:** `SELECT setval(pg_get_serial_sequence(table, pk), MAX(pk))`.
- **SQLite:** rowid tables auto-track (next = max+1); an `AUTOINCREMENT` table needs `sqlite_sequence` bumped. Done once per table at `done` time.

## 4. CLI

```
umbral transferdata --from <url> --to <url> [--batch 1000] [--only table,table] [--dry-run]
```

`--dry-run` reports per-table row counts and the copy order without writing. `--only` limits the set (respecting FK order among the chosen). Progress is logged per table + running row count.

## 5. Phasing

- **Phase 1 (this build):** SQLite→SQLite, registered models, FK-topo order, keyset batches, transactional checkpoint/resume, PK preservation, per-table counts. Tests: PK-preserve pin; full copy with an FK graph; resume after a simulated interruption.
- **Phase 2:** M2M junction tables (not registered models — enumerate from `M2M_RELATIONS`, copy after both endpoints); Postgres ends + cross-backend (SQLite→PG) with sequence reset; parallel per-table workers; a `--map` layer reusing inspectdb's rename map for foreign-shaped sources; circular-FK handling (deferred constraints / two-pass insert-then-update).

## 6. Why not reuse `dumpdata`/`loaddata`

Fixtures load the *entire* table into one JSON array in memory — fine for a dev seed, fatal for 30 GB. And a file round-trip doubles the disk + can't stream or resume mid-file. The transfer engine streams pool→pool with a transactional cursor; fixtures stay the small-dataset tool.
