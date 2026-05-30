# Feature backlog

A working list. Each item carries a status (open / shipped) plus rough scope, so it's clear what's a quick win vs what needs design alignment first.

---

## Shipped (this round)

- ✅ **Critical update #1 — "Inspired by Django" framing.** Relabelled across README, arch.md, CLAUDE.md, about.mdx, and the PRD. Historical decision docs under `docs/decisions/` keep their wording. Commit: `37c76d2`.
- ✅ **Gap #2 — configurable bind address.** `Settings` grows `bind_addr` (default `127.0.0.1:8000`); override via `UMBRA_BIND_ADDR` or `umbra.toml`. Verified by binding to 8765 and curling. Commit: `760c477`.
- ✅ **Feature #7 — `QuerySet::to_sql` for debug-time SQL introspection.** Renders the prepared statement (with `?` placeholders) without executing. Test pins SELECT/FROM/WHERE/ORDER BY/LIMIT invariants without locking to the exact sea-query formatter output. Commit: `5907f07`.

## Open — medium scope (need a small design call)

1. **Primary-key types beyond `i32`/`i64`/`uuid::Uuid` (originally feature #2).** The M3 derive currently hand-rolls each. Two paths: (a) expose a `PrimaryKey` trait users can impl for their own types, (b) auto-derive from any sqlx-compatible scalar. The trait path is the right shape but needs the trait surface pinned. Low-medium effort once the design is fixed; the macro is the simpler half.

2. **Settings extensibility (originally gap #1).** Today `Settings` is a fixed struct. Real apps want their own keys (`OPENAI_API_KEY`, `STRIPE_SECRET`, etc.). Two options: (a) an `extra: HashMap<String, String>` catch-all, (b) a `UserSettings` trait the app implements and registers via the builder. Path (b) is more typed; path (a) is simpler. Lands when the first plugin needs typed settings.

3. **Apps clarification (originally feature #5 — not actually a missing feature).** The original ask said "I haven't seen apps". umbra's `Plugin` *is* the app concept — `plugins/umbra-auth/` is an app. This is a documentation ask: rename / cross-link so plugin authors find this without bouncing through the spec set. Doc-only change.

## Open — large scope (design call first)

4. **Postgres backend body.** `PostgresBackend` is stubbed at M4. Filling it unblocks the `pg_catalog`-based introspection path in `inspectdb`, native `ALTER COLUMN` rendering (lifts the SQLite table-recreation dance), and the `arch.md`-preferred Postgres-first orientation. The dep cost is real (`sqlx` postgres feature; possibly `tokio-postgres`).

5. **Row Level Security (originally feature #1).** Postgres-specific; depends on the Postgres backend. Could ship as a third-party crate that registers via the `Plugin` trait (`AppContext` + `on_ready` set up the RLS scoping). Good first test of "external plugin via the M7 contract".

6. **`DATABASE_ROUTERS` / multiple databases (originally feature #4).** `Settings.databases: HashMap<String, String>` already exists; `umbra::db::pool_for(alias)` already exists. What's missing is the routing layer that picks the right alias per-model. Django ships this as a runtime registry; the typed-in-Rust version is the design question.

7. **Backup / recovery, sql + json import / export (originally feature #3).** Real ask. Best layered as a CLI subcommand (`umbra-cli dumpdata` / `loaddata`) plus a JSON envelope format that captures rows + the schema snapshot for round-trip safety. Medium build; can ship without changes to the engine.

8. **App-rename / model-move detection (originally feature #6).** Django's "did you rename X to Y" prompt. The migration engine already has the snapshot diff machinery; adding heuristic-driven prompts is M8 hardening territory. Defer.

## Reference

- Spec deferrals already tracked elsewhere: `docs/specs/06-migration-engine.md` for migration ops the engine doesn't yet emit (index ops, RunSql, rename detection); `docs/specs/07-inspectdb.md` for FK / index detection; `docs/specs/outlines/auth-and-sessions.md` for sessions, permissions, custom user model; `docs/specs/02-plugin-contract.md` for `Plugin::middleware()` and `commands()`.
