# Audit: core app / config / DB pool (`core-app-config`)

Scope: `crates/umbral-core/src/{app.rs, settings.rs, plugin.rs, lib.rs, db.rs, backend.rs, signals.rs, storage.rs, timezone.rs, auth_contract.rs, cli.rs}` + `crates/umbral-core/src/db/{router.rs, route_context.rs}`. `check.rs`, `crates/umbral-cli/src/{lib.rs,scaffold.rs}`, and the owned doc pages were read as supporting evidence only. Focus: config/deployment posture and the DB-pool layer at ~10M-user scale.

## A. Executive summary

The configuration and pool layer is carefully built in the small (bounded acquire timeouts, WAL PRAGMAs, backend cross-checks at boot, deterministic plugin toposort) but has three urgent problems for a production deployment at scale. First, every production protection — Host-header validation, the insecure-SECRET_KEY boot failure, prod error pages — keys off an `Environment` that silently defaults to `Dev`, and the only safety net (a non-loopback-bind warning) misses the single most common production topology: binding `127.0.0.1` behind a reverse proxy. A forgotten `UMBRAL_ENVIRONMENT=Prod` ships a 10M-user app with a publicly known signing key, no Host validation, and dev-mode behaviors, with zero warnings. Second, the documented `UMBRAL_DB_*` pool knobs are silently ignored for the default pool: `PoolConfig::resolve()` reads ambient settings that `App::build()` publishes *after* the canonical `db::connect()` call, so the pool an operator "tuned" to 100 connections still opens with 10 — the primary scaling knob is a no-op. Third, `Settings.databases` is parsed, documented, and never consumed: an alias configured only there panics in the request path at first query. Additionally, `transaction()` always targets the default pool regardless of routing, the signals registry executes user handlers under a global mutex (deadlock + serialization hazard), and `App::builder()` mutates the process environment with `unsafe set_var` while tokio worker threads are already running. I could not assess how the session/CSRF plugins consume `secret_key`, the exact emit point of ORM signals relative to transaction commit, or `Masked<T>`'s serialization behavior in signal payloads (all outside scoped files).

Three most urgent: (1) Dev-by-default environment gating all prod protections with a heuristic that misses reverse-proxy deploys; (2) `UMBRAL_DB_*` knobs never applied to the default pool; (3) `Settings.databases` documented but dead, causing request-path panics.

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|
| 1 | HIGH | Config/deploy | `settings.rs:419-425` (Env default Dev), `check.rs:199-229` (secret-key check), `check.rs:244-269` (host-validation check), `app.rs:1237-1249` (host guard Prod-only) | All production protections key off explicit `Environment::Prod`; default is `Dev`; the fallback heuristic (`is_loopback_bind`) exempts loopback binds, which is exactly the reverse-proxy topology | A prod deploy behind nginx on `127.0.0.1:8000` that omits `UMBRAL_ENVIRONMENT` runs with the known dev SECRET_KEY accepted, no Host-header validation, dev error pages, and dev static serving — with zero warnings | Detect proxy-fronted deploys (warn when loopback-bound AND the insecure key is set outside a TTY/dev context), or invert: require an explicit environment declaration and fail boot when `secret_key` is the known default unless `Environment::Dev` is explicit. See §C1 |
| 2 | HIGH | Config/secrets | `settings.rs:55-57` (default key), `check.rs:201-211` (Prod-only hard error) | Boot does NOT fail on the default SECRET_KEY unless Prod is declared; and in Prod any non-default string passes — no length/entropy floor (`secret_key = "x"` boots) | Sessions/CSRF/token signatures forgeable with the publicly known key (outside declared Prod) or a trivially guessable one (inside Prod) | In `settings_required`, additionally hard-error in Prod on `secret_key.trim().len() < 32`; warn elsewhere. See §C2 |
| 3 | HIGH | DB pool | `db.rs:382-402` (`PoolConfig::resolve` reads `settings::get_opt()`), `db.rs:431-448` / `db.rs:478-529` (connect fns), `app.rs:824` (`settings::init` in build phase 3), `scaffold.rs:428-432` + `crates/umbral-cli/src/lib.rs:23-27` (canonical boot connects BEFORE build) | `UMBRAL_DB_*` pool knobs are silently ignored for the default pool: settings are published only during `build()`, but the default pool is opened before `build()` in every documented boot path, so `PoolConfig::resolve()` hits the `None` fallback and hardcodes defaults | At 10M users the operator sets `UMBRAL_DB_MAX_CONNECTIONS=100`; the pool still opens with 10 connections + 30s acquire timeout → saturation, 30s request stalls, then errors — while config *appears* correct. Only runtime tenant pools honour the settings | Make `connect()` take the config explicitly (e.g. `connect_with_settings(url, &settings)`), or read the env directly in `PoolConfig::resolve()` as a fallback, or have `build()` verify the default pool's options match settings and error on divergence. See §C3. Docs corrected meanwhile |
| 4 | HIGH | Replica routing | `settings.rs:273-274` (field), `plugin.rs:262-265` (doc claims it registers pools), `app.rs:623-714` (build only reads builder-registered `self.databases`), `db.rs:239-253` (`pool_for_dispatched` panics on unknown alias) | `Settings.databases` is deserialized and documented as a pool source but never consumed: no code opens or registers pools from it | An operator configures `[databases] replica = "postgres://…"` per the docs; a model routed to `"replica"` panics `no database registered under alias 'replica'` on its first query — a request-path panic (500) in production | `App::build()` should either open pools from `settings.databases` (async problem: do it in a `build_async`, or validate-and-error) or hard-error at boot when a model/plugin alias resolves to a `settings.databases`-only entry. At minimum fix the `plugin.rs:263` doc claim. Docs corrected |
| 5 | MEDIUM | Transactions | `db.rs:657-682` (`begin()` uses `pool_dispatched()`), `db.rs:753-770` (`transaction()`) | `transaction()` / `begin()` always target the `"default"` pool — they consult neither the `DatabaseRouter`, per-model aliases, nor the tenant `RouteContext`; there is no alias-aware `begin_for(alias)` | In a multi-DB or DB-per-tenant app, `Model::objects().on_tx(tx)` for a replica/tenant-routed model executes its SQL on the default database — silent wrong-database writes inside "safe" transactions | Add `begin_for(alias)` / `transaction_on(alias, f)`; have `transaction()` assert (debug) or document loudly that `on_tx` pins the statement to the tx's pool regardless of routing |
| 6 | MEDIUM | Concurrency/UB | `app.rs:42-52` | `App::builder()` calls `unsafe { std::env::set_var }` in a loop; in the canonical `#[tokio::main]` boot, multi-thread runtime worker threads already exist, and concurrent `getenv` from any thread during `setenv` is a data race (UB on glibc) | Rare but real startup crash/corruption window; the SAFETY comment's premise ("before the server spawns request handlers") does not cover pre-existing runtime workers or user-spawned threads | Don't mutate the process env: merge `.env` values into the figment pipeline only (settings.rs already does) and pass plugin credentials through `Settings::extra` instead of ambient `std::env::var` |
| 7 | MEDIUM | Boot lifecycle | `app.rs:824-829` (ambient publish, phase 3) vs `app.rs:940-976` (system checks, phase 4); `settings.rs:11-18`, `db.rs:176-180`, `backend.rs:328-332` (panicking `init`s) | A failed `build()` (system-check error, template error, on_ready error) has already published settings/pools/backend into `OnceLock`s that panic on re-`set`; a retry `build()` in the same process panics `settings::init called more than once` instead of surfacing the real error. Double-init semantics are inconsistent across globals (settings/db/backend panic; router/atomic/storage are first-write-wins) | Config-reload or supervise-and-retry patterns crash; error reported is the misleading "called more than once" | Publish ambient state only after phase 4 passes where possible, and make the three panicking `init`s first-write-wins-with-`tracing::error` like their siblings (or return a `BuildError::AlreadyBuilt`) |
| 8 | MEDIUM | Signals | `signals.rs:205-238` (`emit`: sync handlers run inside the `lock_registry()` block), `signals.rs:159-163` (non-reentrant `std::sync::Mutex`) | Sync signal handlers execute while the global registry mutex is held: (a) every signal-emitting ORM write across the whole process serializes on one mutex for the duration of user handler code; (b) a handler that calls `subscribe`/`has_subscribers`/anything that re-locks deadlocks | At 10M-user write volume, a moderately slow sync audit handler becomes a process-wide write throttle; a re-entrant handler hangs every subsequent ORM write forever | Clone the handler list (`Arc<dyn Fn>` instead of `Box`) under the lock, drop the guard, then invoke — same pattern the async branch already uses for its futures |
| 9 | MEDIUM | Signals delivery | `signals.rs:262-321` and `signals.rs:507-537` (`let Ok(json) = serde_json::to_value(...) else { return; }`), `signals.rs:222-226`/`247-250` (panics swallowed) | Delivery is best-effort, at-most-once, in-process: a serialization failure silently drops the signal entirely (no log), handler panics are logged-and-skipped, and there is no transactional coupling (a `post_save` fired inside a later-rolled-back tx is not retracted — emit point relative to commit not verifiable from scoped files) | Audit-log / cache-invalidation / search-index subscribers silently miss writes; compliance-grade audit trails cannot be built on this contract but nothing warns the operator | Log the serde error at `error!` in every `let Ok(..) else { return }` arm; document the at-most-once, non-transactional contract on the module; longer term add a post-commit hook or outbox seam |
| 10 | MEDIUM | Secrets in payloads | `signals.rs:262-321` (full-instance JSON payloads) | Every `pre/post_save`/`pre/post_update` payload carries the entire serialized row — password hashes, tokens, PII, and `Masked` fields (in whatever form their `Serialize` produces — unverified, see blind spots) fan out to every subscriber | Any subscriber that logs or persists payloads (the natural audit-log implementation) copies sensitive columns into logs/secondary stores | Let models exclude fields from signal serialization (e.g. a `#[umbral(signal_skip)]`-style attribute honoured by the emit path), or emit PK + changed-field names by default with full-row opt-in |
| 11 | MEDIUM | Secrets in logs | `settings.rs:268` (`derive(Debug)` on `Settings`), `plugin.rs:571-582` (`derive(Debug)` on `AppContext` containing `Settings`) | `Settings`' `Debug` prints plaintext `secret_key`, `database_url` (with DB password), and `extra` (arbitrary API keys); `AppContext` inherits this. Nothing in scoped files prints it today, but every plugin `on_ready(ctx)` gets a `Debug`-able secrets bundle one `{:?}` away from a log line | One `tracing::debug!(?ctx, ...)` in any plugin leaks the DB password, signing key, and all `UMBRAL_*` credentials | Hand-implement `Debug` for `Settings` redacting `secret_key`, the userinfo of `database_url`/`databases`, and `extra` values |
| 12 | MEDIUM | Legacy panic surface | `db.rs:98-110` (`sqlite_or_panic`), `db.rs:193-195` (`pool()`), `db.rs:229-231` (`pool_for()`) | `pool()`/`pool_for()` still panic on a Postgres default pool with a stale "Postgres support arrives in Phase 2" message (Postgres is fully wired elsewhere); module docs `db.rs:29-43` repeat the stale Phase-1 story | Any remaining plugin/user code on the legacy accessors turns a Postgres deployment into runtime panics with a message that actively misdirects the operator ("wait for Phase 2") | Update the panic text and module docs to "this call site must migrate to `pool_dispatched()`"; consider deprecating `pool()`/`pool_for()` |
| 13 | MEDIUM | Deploy lifecycle | `app.rs:56-83` (`serve`, "no graceful shutdown hook"), `db.rs:531-558` (`close()` exists, never called by framework) | No graceful shutdown: `serve()` runs until killed; in-flight requests are dropped on deploy and pools are never drained (Postgres logs abrupt terminations; SQLite skips WAL checkpoint) | At 10M-user request rates every deploy/restart drops live requests mid-write | Add `serve_with_shutdown(signal)` wiring `axum::serve(...).with_graceful_shutdown(...)` + `db::close().await`, and make it the CLI `serve` default |
| 14 | LOW | Tenant pools | `db.rs:266-277` (`register_tenant_pool`: `Box::leak`, first-write-wins) | Every tenant pool is leaked for process lifetime; no eviction or replacement — a rotated tenant DB URL cannot replace its pool without a restart | DB-per-tenant at scale accumulates pools (each up to `max_connections` server slots) for offboarded tenants; credential rotation requires restart | Acceptable for v1 (documented), but add an eviction/replace API before promoting DB-per-tenant as a scale pattern |
| 15 | LOW | Observability | `timezone.rs:41-55` (`tz_or_utc`) | Doc comment claims a "one-shot warning" for an unknown IANA tz, but `tracing::warn!` fires on every call — and `active_tz()` is on the per-value datetime marshalling path | A typo'd `UMBRAL_TIME_ZONE` floods production logs at request rate | Gate the warning with a `std::sync::Once` (matching the doc), or cache the parsed `Tz` in a `OnceLock` |
| 16 | LOW | Config validation | `settings.rs:415-416` (`#[serde(flatten)] extra`), `settings.rs:419-425` (case-sensitive `Environment`) | (a) Misspelled framework keys (`UMBRAL_DB_MAX_CONNECTION`, `UMBRAL_ALOWED_HOSTS`) silently land in `extra` — no near-miss warning; (b) `UMBRAL_ENVIRONMENT=prod` (lowercase) fails deserialization — loud, but the error is a generic figment variant error while every hint string says `Prod` | Config typos silently revert security/pool settings to defaults; lowercase `prod` costs a confused boot failure | Warn at load time when an `extra` key is within edit-distance 1-2 of a known field; add `#[serde(alias)]`/case-insensitive deserialization for `Environment` |
| 17 | LOW | Plugin routing | `router.rs:157-159` (`install_router_from_plugin`: silent first-write-wins) | Two router-installing plugins (or a plugin plus `.router(...)`) → the second is silently ignored; the storage registry's equivalent (`storage.rs:374-389`) at least warns | An app combining a tenants plugin with an explicit `.router(...)` runs a different routing policy than one of them assumes, silently | Log a `tracing::warn!` naming both parties when the `set` loses, mirroring `set_storage_named` |
| 18 | LOW | Plugin contract docs | `plugin.rs:372-374` vs `app.rs:986-1008` | `Plugin::static_files` doc says URL-path conflicts surface "with the second registrant losing"; in reality axum's `Router::route` panics at build — loud, but the doc describes silent-lose semantics | Plugin authors design against the wrong conflict behavior | Fix the doc comment to say the build panics |

Positive observations kept out of the table per instructions; notably: no scoped code logs `database_url` or credentials (`PoolConfig::log` at `db.rs:406-418` logs knobs only), `Schema::new` (`router.rs:46-60`) is a solid injection guard, spawned tasks deliberately do not inherit tenant context (`route_context.rs:92-102`), and the plugin toposort (`app.rs:1326-1411`) is deterministic with loud failure on cycles/dupes/reserved names.

## C. Detailed findings (CRITICAL/HIGH)

### C1. Dev-by-default environment gates every production protection (HIGH)

Vulnerable code — `check.rs:199-229` (secret-key check) and `app.rs:1242-1245` (host guard):

```rust
// check.rs — hard error ONLY when the operator self-declares Prod:
if matches!(ctx.settings.environment, Environment::Prod) && insecure { /* Error */ }
// fallback heuristic ONLY fires on a non-loopback bind:
if insecure && !is_loopback_bind(&ctx.settings.bind_addr) { /* Warning */ }

// app.rs — Host validation enforced ONLY in Prod:
let host_policy = crate::hosts::HostPolicy::new(
    &settings.allowed_hosts,
    matches!(settings.environment, crate::settings::Environment::Prod),
);
```

Scenario: a team deploys the scaffolded app behind nginx. `bind_addr` stays the default `127.0.0.1:8000` (correct for a proxy), `UMBRAL_ENVIRONMENT` is forgotten. `is_loopback_bind` returns true, so even the warning is suppressed. The app now serves 10M users with: the publicly known `umbral-insecure-dev-key-change-me` signing key accepted silently (anyone can mint forged session/CSRF material in whatever plugin derives from `secret_key`); any `Host` header accepted (nginx configs commonly pass `$http_host` through — cache poisoning and poisoned password-reset links); dev-mode error templates (which per `app.rs:397-399` receive `error_display`/`error_chain`) rendering internal error chains to the public; and the static handler in dev mode (`app.rs:1074`) serving live source dirs.

Corrected approach (in `settings_required`, `check.rs`):

```rust
fn settings_required(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let insecure = ctx.settings.secret_key == INSECURE_DEV_SECRET_KEY;
    let declared_dev = ctx.settings.environment_was_explicit
        && matches!(ctx.settings.environment, Environment::Dev | Environment::Test);
    if insecure && !declared_dev {
        // Hard error unless the operator EXPLICITLY declared Dev/Test.
        // "I never set the environment" no longer implies "safe".
        return vec![SystemCheckFinding { severity: Severity::Error, /* ... */ }];
    }
    Vec::new()
}
```

(`environment_was_explicit` = track whether the field came from a source vs the serde default; figment profiles or an `Option<Environment>` that defaults late both work.) This inverts the failure mode: forgetting configuration fails loud instead of failing open.

### C2. Default SECRET_KEY: boot only fails in declared Prod, and no strength floor (HIGH)

`settings.rs:55-57` ships `"umbral-insecure-dev-key-change-me"`; `check.rs:201-211` errors only under `Environment::Prod`, and only on exact equality with the known default. A `secret_key = "x"` (or `""` — core does not reject it; the empty/whitespace case is only covered by `umbral-security`'s own `validate_secret_key` tests, i.e. enforcement lives in an optional plugin) boots cleanly in Prod.

Scenario: an operator "fixes" the boot error by setting `UMBRAL_SECRET_KEY=changeme`. Every signature derived from it (sessions, CSRF, signed cookies) is brute-forceable offline.

Corrected snippet (extend `settings_required`, `check.rs:199`):

```rust
if matches!(ctx.settings.environment, Environment::Prod) {
    let key = ctx.settings.secret_key.trim();
    if key == INSECURE_DEV_SECRET_KEY || key.len() < 32 {
        findings.push(SystemCheckFinding {
            check_id: "settings.required",
            severity: Severity::Error,
            location: CheckLocation::Settings,
            message: "secret_key is missing, the dev default, or shorter than 32 chars in Environment::Prod.".into(),
            hint: Some("generate one: `openssl rand -hex 32`, set UMBRAL_SECRET_KEY.".into()),
        });
    }
}
```

### C3. `UMBRAL_DB_*` pool knobs never reach the default pool (HIGH)

Vulnerable code — `db.rs:382-402`:

```rust
impl PoolConfig {
    fn resolve() -> Self {
        match crate::settings::get_opt() {          // <-- ambient OnceLock
            Some(s) => PoolConfig { max_connections: s.db_max_connections, /* ... */ },
            None => PoolConfig { max_connections: 10, /* hardcoded defaults */ },
        }
    }
}
```

The ambient settings are published by `crate::settings::init(&settings)` at `app.rs:824` — phase 3 of `build()`. But `build()` *requires* the default pool to already exist (`BuildError::DefaultPoolMissing`, `app.rs:627-629`), and every documented boot path — the scaffolded main (`scaffold.rs:428-432`), the umbral-cli rustdoc (`lib.rs:23-27`), the getting-started/backends docs — calls `umbral::db::connect(&settings.database_url)` **before** `.build()`. At that moment `get_opt()` is `None`, so the default pool always opens with the hardcoded fallback (10 connections, 30s acquire, …) and every `UMBRAL_DB_*` value is silently discarded for the pool that serves all traffic. Only pools opened after `build()` (runtime tenant pools, `db.rs:266`) honour the settings.

Scenario: load testing shows pool saturation; the operator sets `UMBRAL_DB_MAX_CONNECTIONS=100` and redeploys. Nothing changes: p99 climbs to the 30s acquire timeout under load, then errors. The boot log does print `max_connections=10` (`db.rs:406-418`), but the operator has no reason to distrust their config.

Corrected code (make the config explicit instead of ambient):

```rust
/// Open the pool with an explicit config — no ambient-order trap.
pub async fn connect_with(url: &str, settings: &crate::settings::Settings)
    -> Result<DbPool, sqlx::Error>
{
    let cfg = PoolConfig::from_settings(settings);
    // ...dispatch on scheme exactly as `connect` does, threading `cfg` through
}

impl PoolConfig {
    fn from_settings(s: &crate::settings::Settings) -> Self { /* field copy */ }
    fn resolve() -> Self {
        match crate::settings::get_opt() {
            Some(s) => Self::from_settings(s),
            None => {
                tracing::warn!(
                    "umbral: opening a pool before App::build(); UMBRAL_DB_* pool \
                     settings are NOT applied — use db::connect_with(url, &settings)"
                );
                Self::defaults()
            }
        }
    }
}
```

…and update scaffold/docs to `connect_with(&settings.database_url, &settings)`. Alternatively, `build()` could compare the registered default pool's options against settings and error on divergence. Until the code fix lands, the owned doc pages have been corrected to describe the real behavior and the manual `PgPoolOptions`/`SqlitePoolOptions` workaround (see "Docs updated").

### C4. `Settings.databases` is documented but never consumed → request-path panic (HIGH)

Evidence: the field is deserialized (`settings.rs:273-274`) and tested (`settings.rs:539-549`), and `plugin.rs:262-265` explicitly claims an alias "must have been registered via `AppBuilder::database(alias, pool)` **or `Settings.databases[alias]`**" — but a repo-wide grep for `.databases` shows no consumer outside `self.databases` (the builder's own map, `app.rs:651`) and tests. `App::build()` validates aliases only against builder-registered pools (`app.rs:674-714`).

Scenario: following the settings docs, an operator configures `[databases] analytics = "postgres://…"` and marks a model `#[umbral(database = "analytics")]`. Boot fails with `BuildError::PluginDatabaseAlias` — *if* the model is registered through the checked path. But a `DatabaseRouter` returning `Alias::new("analytics")` at request time bypasses the boot check entirely and hits `pool_for_dispatched` (`db.rs:239-253`), which panics `no database registered under alias 'analytics'` inside the request — a 500 (via catch-panic) on every routed request in production.

Corrected code (in `build()`, phase 1.5 — validate; opening is async so validation is the sync-safe fix):

```rust
// Phase 1.6 — Settings.databases is config the app must have already
// materialised into pools. Fail loudly at boot if it didn't.
for alias in settings.databases.keys() {
    if !self.databases.contains_key(alias) {
        return Err(BuildError::SettingsDatabaseNotRegistered {
            alias: alias.clone(),
            hint: "open it with umbral::db::connect(&url).await and pass \
                   .database(alias, pool) — settings.databases does not \
                   auto-open pools",
        });
    }
}
```

…plus fix the `plugin.rs:263` doc claim. The settings doc page has been corrected with the explicit register-every-alias loop (see "Docs updated").

## D. Blind spots

- **How `secret_key` is consumed** (session signing, CSRF, cookie stores) lives in `plugins/umbral-sessions` / `umbral-security` — outside scope. Severity of findings 1/2 assumes it signs security-relevant material, which the plugin test names (`csrf_signed_config`, `validate_secret_key`, `cookie_store::with_secret`) strongly suggest.
- **Signal emit point vs transaction commit**: `Manager::save` / QuerySet terminals (`orm/`) decide whether `post_save` fires before or after COMMIT and whether `.atomic()` wrapping interacts; not verifiable from `signals.rs` alone. Finding 9's "no transactional coupling" is inferred from the registry offering no commit hook.
- **`Masked<T>` serialization** in signal payloads (plaintext vs ciphertext) — implementation not in scope; finding 10 flags the exposure without asserting which form leaks.
- **sqlx error `Display` contents** on a failed Postgres connect (whether host/user/db name appear in the surfaced error) — not verified; no scoped code was found echoing the raw URL.
- **`hosts.rs` HostPolicy matching logic**, CORS internals, rate limiting, and the error-page render paths (`errors.rs`) — adjacent files referenced for gating logic only; their internals belong to the web auditor.
- **Runtime infra**: TLS termination, container/user privileges, actual proxy configs, and whether any deployment sets `UMBRAL_ENVIRONMENT` — unknowable from the repo.
- Whether the ORM's `resolve_pool` honours `db_for_read` vs `db_for_write` (`RouteOp`) correctly per terminal — the trait surface is sound (`router.rs:75-119`); the call sites are in `orm/` (ORM auditor's scope).

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Add the length floor + non-Prod hard-error path for the default SECRET_KEY (`check.rs:199` — finding 2/C2).
2. Log serde failures in the `let Ok(..) else { return }` arms of `signals.rs` (finding 9).
3. `Once`-gate or cache the unknown-tz warning (`timezone.rs:44` — finding 15).
4. Warn in `install_router_from_plugin` when the set loses (`router.rs:157` — finding 17); fix the `plugin.rs:372` and `plugin.rs:263` doc claims (findings 18, 4).
5. Update the stale "Phase 2" panic text in `db.rs:103-110` (finding 12).

**Short term (< 2 weeks)**
6. Ship `db::connect_with(url, &settings)` + warn in the ambient fallback; migrate scaffold, cli rustdoc, and examples (finding 3/C3).
7. Boot-time validation that every `settings.databases` alias has a registered pool; or open them in an async build path (finding 4/C4).
8. Move sync-handler invocation outside the registry lock in `signals.rs::emit` (finding 8).
9. Hand-write `Debug` for `Settings` with redaction (finding 11).
10. Replace `App::builder()`'s `unsafe set_var` loop with figment-only `.env` merging (finding 6).
11. `serve_with_shutdown` + `db::close()` on the CLI serve path (finding 13).

**Structural (needs design)**
12. Environment-declaration redesign: explicit-or-fail posture so "forgot to configure" fails loud (finding 1/C1) — interacts with dev ergonomics and the scaffold.
13. Consistent ambient-state lifecycle: publish-after-checks, uniform first-write-wins semantics, and a supported rebuild story (finding 7).
14. Alias-aware transactions (`begin_for` / router-consulted `transaction()`) for multi-DB correctness (finding 5).
15. Signal payload privacy (field exclusion) and a post-commit/outbox delivery tier (findings 9, 10).

## Docs updated

1. `documentation/docs/v0.0.1/getting-started/settings-and-env.mdx` — (a) added a warning callout after the settings table: the `db_*` pool knobs are read from ambient settings published during `App::build()`, so the default pool opened before `build()` ignores `UMBRAL_DB_*` and uses built-in defaults; documented the `PgPoolOptions`/`SqlitePoolOptions` workaround and the boot-log verification line. (b) Corrected the `[databases]` section, which claimed named secondary pools are "routed per-model" as if configuring them were sufficient — code never opens pools from `settings.databases`; added the explicit `.database(alias, connect(url).await?)` registration loop and the panic that otherwise results. Reason: page contradicted `db.rs:382-402`/`app.rs:824` and the dead `settings.databases` field.
2. `documentation/docs/v0.0.1/backends/postgres.mdx` — split the Pooling callout: removed the incorrect "Tune all of them from settings (`UMBRAL_DB_*` …)" claim for the default pool; added a warning callout with the boot-order explanation and a concrete `PgPoolOptions` snippet. Reason: contradicted the `PoolConfig::resolve()` ordering (finding 3).
3. `documentation/docs/v0.0.1/backends/sqlite.mdx` — same correction for the SQLite Pooling section ("honours the same `UMBRAL_DB_*` settings" → accurate description + warning callout with the `SqlitePoolOptions` workaround).

Not touched (other auditors' scope): `documentation/docs/v0.0.1/orm/connection-pooling.mdx` — it is the canonical knob-list page and likely repeats the same "tune via `UMBRAL_DB_*`" claim; the ORM auditor should apply the same correction there.
