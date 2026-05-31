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
- ✅ **Forms primitives + `#[derive(Form)]`.** `umbra::forms::{Field, Validator, ValidationErrors}` for hand-built forms; the derive macro lowers a struct + per-field `#[form(min_length = N, email, password, optional, ...)]` attrs into an `impl Form` that validates a `HashMap` into the typed struct and renders the HTML. Per-SqlType dispatch + XSS-escape on render. Commits: `a9bf17f` (primitives), `ebcf8fc` (derive).
- ✅ **umbra-admin plugin.** Auto-CRUD admin at `/admin`. HTTP Basic Auth gate against `umbra_auth::AuthUser.is_staff`. List/detail/create/edit/delete pages, per-SqlType form widgets, embedded jinja templates. Second built-in plugin crate; structurally identical to a third-party. Commit: `7ff0659`.
- ✅ **umbra-sessions plugin.** DB-backed sessions linked to umbra-auth users. `create_session` / `read_session` / `destroy_session` / `current_user(headers)` helpers. Secure-by-default cookie flags (`HttpOnly`, `Secure`, `SameSite=Lax`). Commit: `a9bf17f`.
- ✅ **umbra-rest plugin.** Auto-generated JSON CRUD at `/api/<table>/`. Per-SqlType dispatch, safe-by-default block-list (`auth_user`, `session`, `umbra_migrations`), error envelope with HTTP status codes. Verified end-to-end: same Article model serves HTML AND JSON consistently in the derive-demo. Commit: `5e27a55`.
- ✅ **umbra-tasks plugin.** DB-backed background task queue (Celery shape). `TaskRow` model, `enqueue(name, payload)` / `register_handler(name, fn)` / `run_worker(opts)` / `run_worker_once()`. Retry policy, panic recovery via spawned task, cooperative shutdown via `tokio::sync::watch`. Commit: `01be657`.
- ✅ **umbra-email plugin.** SMTP via `lettre` with tokio + rustls (no OpenSSL system deps). `EmailMessage` builder, `send()` async entry, `ConsoleBackend` fallback for dev so missing SMTP config doesn't silently no-op. SMTP credentials read from `Settings.extra` (`email_smtp_host`, `email_smtp_user`, etc.). Commit: `3901a2b`.
- ✅ **umbra-openapi plugin.** Auto-generated OpenAPI 3.0 spec at `/openapi/openapi.json` + embedded Swagger UI at `/openapi/`. Walks the same model registry umbra-rest does; the spec describes every exposed endpoint with per-SqlType OpenAPI type mapping. Mirrors umbra-rest's block-list. Commit: `23fb623`.
- ✅ **`Plugin::wrap_router` middleware lift.** Closes the M7 deferral. Plugins take a `Router` and return a wrapped one, sidestepping the `tower::Layer` generic / lifetime erasure that blocked `Vec<Box<dyn Layer<...>>>`. App::build applies `wrap_router` in topological order so later plugins wrap earlier ones (security after auth sees the auth-augmented router). Commit: `c4e8469`.
- ✅ **umbra-signals plugin.** In-process pub/sub. `emit(name, payload)`, sync `subscribe`, async `subscribe_async`; payloads are `serde_json::Value`. Strictly in-process v1 (no Redis / NATS broker, no replay); use umbra-tasks when work must survive the process. Commit: `16be3de`.
- ✅ **umbra-static plugin.** Static file serving wrapping `tower_http::services::ServeDir`. `StaticPlugin::new("/static", "./assets")` mounts a directory at a URL prefix; ServeDir handles MIME sniffing, range requests, If-Modified-Since. Commit: `b97f4f1`.
- ✅ **umbra-cache plugin.** Django's cache framework, the small slice that matters: `Cache` handle over a `CacheBackend` trait, in-memory + SQLite backends, generic `get<T>` / `set<T>` with serde encoding, TTL with lazy expiry-on-read plus opt-in `sweep()`. Commit: `43effa0`.
- ✅ **umbra-security plugin.** CSRF protection (double-submit cookie) + default security headers (`X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`); opt-in HSTS. First consumer of `Plugin::wrap_router`. Commit: `03fb8dd`.
- ✅ **umbra-testing crate.** Django `TestCase + Client` ergonomics: `TestClient` over an `axum::Router` with verb-shaped methods, a per-client cookie jar (Set-Cookie on response → Cookie on next request), default header pinning, JSON helpers. `TempPool` for tempfile-backed sqlite. `TestResponse` with `assert_status`, `assert_status_ok`, `assert_body_contains`, `assert_header`, `body_json::<T>` (body printed on parse error). Lives under `crates/` (not a plugin); consumers drop it in `[dev-dependencies]`. Commit: `12bb39c`.

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
