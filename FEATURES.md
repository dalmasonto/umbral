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
- ✅ **Large #7 — backup / recovery via `dumpdata` / `loaddata`.** New `umbra::backup` module walks every registered model, dispatches per-column on `SqlType` to dump rows as JSON, reverses the dispatch on load. Carries an `umbra_dump_version` so a forward-incompatible dump fails loudly. CLI subcommands wired; round-trip verified end-to-end through the binary (2 rows → JSON → wipe → load → same 2 rows back with PKs and nullable column intact). Commit: `3ad51b5`.
- ✅ **Large #6 — per-plugin database routing.** `Plugin::database()` returns `Option<&'static str>`. App::build validates the alias exists in the pool set (typos surface as `BuildError::PluginDatabaseAlias` at boot) and publishes a per-model alias map via `migrate::init_model_aliases`. QuerySet's `resolve_pool` consults it: explicit `.on(&pool)` wins, then the plugin's alias, then the default pool. Commit: `a0767cf`.

## Open — large scope (multi-round work)

### #4 + #5: Postgres backend + RLS

**Decision:** full `sqlx::Any` refactor (you picked this option). The realistic scope is bigger than a single autonomous commit — 55 `SqlitePool` / `SqliteRow` / `SqliteQueryBuilder` references across 18 files, plus 4 new code paths.

**Phased plan**, each phase a dedicated round so the test suite stays green throughout:

- **Phase 1 — AnyPool plumbing.** Enable `sqlx`'s `any` + `postgres` features. Refactor `crates/umbra-core/src/db.rs` (`SqlitePool` → `AnyPool`). Change `Model`'s supertrait bound from `FromRow<'r, SqliteRow>` to `FromRow<'r, AnyRow>`. Update `queryset.rs`. Touch every test (the boots all use `SqlitePool`). Tests still target sqlite at runtime; nothing Postgres-specific yet.

- **Phase 2 — Backend dispatch in migrate.** `render_operation` dispatches on `backend::active().name()`. SQLite path keeps its INTEGER PRIMARY KEY AUTOINCREMENT quirk; Postgres path emits `BIGSERIAL` / `GENERATED ALWAYS AS IDENTITY`. `render_alter_column_dance` similarly conditional (Postgres has native `ALTER COLUMN` and doesn't need the table-recreation dance).

- **Phase 3 — Postgres introspection in inspectdb.** Replace the PRAGMA path with a `pg_catalog` query when the active backend is Postgres. Same `IntrospectedSchema` output; different source.

- **Phase 4 — PostgresBackend.map_type body + RLS plugin.** Fill in the Postgres `ColumnType` mapping. RLS (#5) lands as a third-party plugin crate that registers via the existing M7 contract — `Plugin::on_ready` runs the `ALTER TABLE ... ENABLE ROW LEVEL SECURITY` statements and installs policies.

Each phase is its own session: too much for autonomous ship-and-verify in one round, and a half-finished refactor with broken tests is worse than no refactor.

### #8: rename detection heuristic

Deferred (you chose this). M8 hardening per spec 06; current drop+add behaviour is correct, just lossy. Revisit when inspectdb / migrate hits a real-world rename case.

## Reference

- Spec deferrals already tracked elsewhere: `docs/specs/06-migration-engine.md` for migration ops the engine doesn't yet emit (index ops, RunSql, rename detection); `docs/specs/07-inspectdb.md` for FK / index detection; `docs/specs/outlines/auth-and-sessions.md` for sessions, permissions, custom user model; `docs/specs/02-plugin-contract.md` for `Plugin::middleware()` and `commands()`.
