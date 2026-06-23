---
name: throwaway-postgres-verification
description: Use when a feature has PG-gated/#[ignore]'d integration tests you must actually RUN (multitenancy, schemas, RLS, replicas) — stand up a throwaway Postgres cluster on a spare port. Also covers the SET LOCAL vs session SET search_path connection-pool-pollution trap.
---

# Verifying PG-gated tests with a throwaway Postgres

## Context
Some umbra features are Postgres-only and ship with `#[ignore]` integration tests gated on an env var (`UMBRA_TENANTS_TEST_PG`, `UMBRA_TEST_POSTGRES_URL`, `DATABASE_URL`) — schema-per-tenant multitenancy, RLS, read replicas, the PG-only field types. The unit/SQLite suites pass while the actual PG behavior is **completely unverified**. "It compiles and unit-tests pass" proves nothing about a feature whose whole point is Postgres semantics. You MUST run the gated test against a real PG before claiming the feature works.

The `umbra-tenants` schema-per-tenant feature is the canonical case: every unit test was green, but a real cross-tenant data bug lived purely in PG connection-pool semantics (see Pitfalls). Standing up real PG caught it.

## Approach
`initdb`/`pg_ctl` are usually present (`/usr/lib/postgresql/<ver>/bin/`). Stand up a throwaway cluster, run the test, auto-clean. Template:

```bash
SCRATCH=<your scratchpad dir>
PGDATA=$SCRATCH/pgdata; PGPORT=54331; PGBIN=/usr/lib/postgresql/16/bin
cleanup(){ $PGBIN/pg_ctl -D "$PGDATA" -w stop >/dev/null 2>&1; rm -rf "$PGDATA"; }
trap cleanup EXIT                      # always stop + delete, even on test failure
rm -rf "$PGDATA"; mkdir -p "$PGDATA"
$PGBIN/initdb -D "$PGDATA" -U postgres --auth-local=trust --auth-host=trust -E UTF8 >/dev/null 2>&1
$PGBIN/pg_ctl -D "$PGDATA" -o "-p $PGPORT -k /tmp" -l "$SCRATCH/pg.log" -w start
$PGBIN/createdb -h localhost -p $PGPORT -U postgres appdb
export UMBRA_TENANTS_TEST_PG="postgres://postgres@localhost:$PGPORT/appdb"
cd crates && cargo test -p <crate> --test <gated_test> -- --ignored --nocapture
```

Two gotchas that bite immediately:
- **Socket path length:** the Unix socket dir has a **107-byte** limit. The scratchpad path is too long → `could not create Unix-domain socket`. Use `-k /tmp` (short) for the socket; TCP (`localhost:$PGPORT`) is what the test connects on anyway, so the socket dir is irrelevant to the test.
- **Port already in use:** pick an uncommon high port (54329, 54331…); don't reuse 5432/5433.

`--auth-local=trust --auth-host=trust` means no password (`postgres://postgres@localhost:port/db`). Each bash invocation with `trap cleanup EXIT` is self-contained — to iterate on a fix, just re-run the whole block (fresh cluster each time = deterministic).

## Why
- The alternative — trusting a subagent's "workspace builds, 11 tests pass" — is exactly what hides PG-only bugs. A subagent often *cannot* run the gated test (no PG in its sandbox) and reports the feature done on unit tests alone. The throwaway cluster is how you actually close the loop.
- `trap cleanup EXIT` guarantees no leaked cluster/process even when the test panics — important since a failing test is the common case while iterating.

## Pitfalls
- **The SET LOCAL vs session SET search_path pool-pollution trap** (the bug the throwaway PG caught): `SET search_path TO "tenant_x"` (session-level) on a **pooled** connection persists after the work — the connection returns to the pool still pinned. The next, unrelated ORM query that reuses it resolves unqualified tables against `tenant_x` instead of `public` → in multitenancy that's a cross-tenant data leak / `relation "..." does not exist`. **Fix: always `SET LOCAL search_path` inside a transaction** (`pool.begin()` → `SET LOCAL` → work → `commit`); Postgres auto-resets it at commit/rollback, so the pool stays clean. This applies to *every* block that pins search_path, including the migration-ledger ensure/read, not just the DDL tx. See `crates/umbra-core/src/migrate.rs::run_tenant_apps_in_postgres_schema`.
- A SQLite pool has no schemas — schema-per-tenant helpers return `MigrateError::SchemaUnsupportedOnSqlite`, never a silent TEXT fallback. Don't try to run these gated tests on SQLite.

## See also
- `plugins/umbra-tenants/tests/isolation_postgres.rs` — the gated isolation proof (provision 2 tenants, write under each `scope`, assert isolation).
- gaps2 #69 Phase 2 write-up (the schema-per-tenant management layer).
- `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md` — the router foundation.
