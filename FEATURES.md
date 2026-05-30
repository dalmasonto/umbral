# Feature backlog

A working list. Each item carries a status (open / shipped) plus rough scope, so it's clear what's a quick win vs what needs design alignment first.

---

## Shipped

- ✅ **Critical update #1 — "Inspired by Django" framing.** Relabelled across README, arch.md, CLAUDE.md, about.mdx, and the PRD. Historical decision docs under `docs/decisions/` keep their wording. Commit: `37c76d2`.
- ✅ **Gap #2 — configurable bind address.** `Settings` grows `bind_addr` (default `127.0.0.1:8000`); override via `UMBRA_BIND_ADDR` or `umbra.toml`. Verified by binding to 8765 and curling. Commit: `760c477`.
- ✅ **Feature #7 — `QuerySet::to_sql` for debug-time SQL introspection.** Renders the prepared statement (with `?` placeholders) without executing. Test pins SELECT/FROM/WHERE/ORDER BY/LIMIT invariants without locking to the exact sea-query formatter output. Commit: `5907f07`.
- ✅ **Medium #3 — apps clarification.** `Plugin` IS the app concept; new page maps every Django app primitive onto its umbra counterpart and explains the rename. Commit: `37f155e`.
- ✅ **Medium #2 — settings extensibility.** `#[serde(flatten)] extra: HashMap<String, toml::Value>` on `Settings`. Unknown env vars and unknown TOML keys (`UMBRA_OPENAI_API_KEY=sk-...`, `[external.openai]` tables) flow into the map; `settings.extra_str(key)` is the scalar-string accessor. Typed `UserSettings` trait alternative parks as a future layer on top. Commit: `6f5243e`.
- ✅ **Medium #1 — PK types beyond i32/i64/Uuid.** `PrimaryKey: Copy` relaxed to `Clone`; built-in impls cover every Rust integer width plus `String` (slug-style PKs). The derive's hardcoded type check is gone — user newtypes opt in with `impl PrimaryKey for MyId {}`, and Rust's trait-bound diagnostic does the validation. Commit: `4e8aee4`.

## Open — large scope (design call first)

4. **Postgres backend body.** `PostgresBackend` is stubbed at M4. Filling it unblocks the `pg_catalog`-based introspection path in `inspectdb`, native `ALTER COLUMN` rendering (lifts the SQLite table-recreation dance), and the `arch.md`-preferred Postgres-first orientation. The dep cost is real (`sqlx` postgres feature; possibly `tokio-postgres`).

5. **Row Level Security (originally feature #1).** Postgres-specific; depends on the Postgres backend. Could ship as a third-party crate that registers via the `Plugin` trait (`AppContext` + `on_ready` set up the RLS scoping). Good first test of "external plugin via the M7 contract".

6. **`DATABASE_ROUTERS` / multiple databases (originally feature #4).** `Settings.databases: HashMap<String, String>` already exists; `umbra::db::pool_for(alias)` already exists. What's missing is the routing layer that picks the right alias per-model. Django ships this as a runtime registry; the typed-in-Rust version is the design question.

7. **Backup / recovery, sql + json import / export (originally feature #3).** Real ask. Best layered as a CLI subcommand (`umbra-cli dumpdata` / `loaddata`) plus a JSON envelope format that captures rows + the schema snapshot for round-trip safety. Medium build; can ship without changes to the engine.

8. **App-rename / model-move detection (originally feature #6).** Django's "did you rename X to Y" prompt. The migration engine already has the snapshot diff machinery; adding heuristic-driven prompts is M8 hardening territory. Defer.

## Reference

- Spec deferrals already tracked elsewhere: `docs/specs/06-migration-engine.md` for migration ops the engine doesn't yet emit (index ops, RunSql, rename detection); `docs/specs/07-inspectdb.md` for FK / index detection; `docs/specs/outlines/auth-and-sessions.md` for sessions, permissions, custom user model; `docs/specs/02-plugin-contract.md` for `Plugin::middleware()` and `commands()`.
