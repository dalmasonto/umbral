# review_4 · umbral-testing

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 1,016 LOC (`crates/umbral-testing`) |
| **Reviewer** | umbral-testing reviewer (one of ~20) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 1 high · 2 medium · 2 low · 1 info — 6 new, 4 known |

## Summary

- The crate is small and well-factored: `TestClient`/`TestResponse` (in-process axum `oneshot`, cookie jar, default headers), `TempPool` (tempfile SQLite), the `Factory` layer, `create_tables()` and the one-shot `boot()`. Its best idea — deriving the test schema from the same `ModelMeta` the migration engine renders for `migrate`, instead of hand-written `CREATE TABLE` — is sound, behaviourally tested, and closes the drift class gaps3 #78 names.
- The sharpest problem is `boot()`'s contract: after the first call, every later caller's `configure` closure is silently discarded, while the docstring and user docs tell every test to "just say what it needs at the top" (TESTING-1). With parallel `#[tokio::test]`s the winner is scheduling-dependent, so a file's tests can flip between pass and fail by ordering.
- Second: `boot()` builds and drops the `App`, and `App::into_router(self)` is the only router accessor — so the crate's two documented halves (`boot` for schema+models, `TestClient` for HTTP) cannot compose in one test, and a second `App::build()` panics (TESTING-2). The crate's own `tests/factory.rs:39-57` is exactly this workaround, in-tree.
- Third: `TempPool` — the pool `test-client.mdx` tells users to start from — builds its own `SqliteConnectOptions` with no WAL and, sharper, foreign-key enforcement off, while `umbral-core` documents that the ORM emits `REFERENCES` "assuming they're respected" (TESTING-3). A test path through `TempPool` is therefore more permissive than production on exactly the constraint class the ORM relies on.
- Fix first: give `boot` an honest contract that also yields the built app (closes TESTING-1 + TESTING-2 in one stroke — e.g. store the `App` in the `OnceCell` and add a booted-app accessor, plus a docs page showing one shared setup helper per file), and build `TempPool`'s options through the same `sqlite_options` production uses (TESTING-3).

## Findings

### TESTING-1 · HIGH · correctness · self-verified
**After the first call, `boot()` silently discards every later caller's `configure` closure — the exact call the docs tell each test to make — so a test's own app configuration can vanish depending on scheduling.**

- **Where**: `crates/umbral-testing/src/lib.rs:558-585` (function); docstring claim at `lib.rs:528-529`; user docs at `documentation/docs/v0.0.1/testing/setup.mdx:26`
- **Evidence**:
  ```rust
  pub async fn boot<F>(configure: F)
  where
      F: FnOnce(umbral::AppBuilder) -> umbral::AppBuilder,
  {
      static BOOTED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
      BOOTED
          .get_or_init(|| async {
              ...
              configure(builder)
                  .build()
                  .expect("umbral-testing: App::build");
              ...
          })
          .await;
  }
  ```
  `configure` is moved into the initializer future and is only ever polled on the first, winning call; later callers get the cached `()` back and their closure is dropped without being run or observed. The docstring says the opposite ("the first call builds, every later call is a no-op, **so each `#[tokio::test]` can just say what it needs at the top**", `lib.rs:528-529`), and setup.mdx:26 repeats it: "Call it at the top of **every** test in the file — the first call builds, the rest are no-ops."
- **Impact**: concrete scenario — one file, two tests running in parallel (the default): `test_a` calls `boot(|b| b.model::<Note>())` and `test_b` calls `boot(|b| b.plugin(SomePlugin::default()).model::<Task>())`. Which call reaches `get_or_init` first is scheduler-dependent. If `test_a` wins, `test_b`'s plugin is never installed and `Task` is never registered, so `test_b` fails on plugin tables/routes that `create_tables()` never created — and the same `cargo test` passes on the next run when the ordering flips. In the other direction a test can pass for the wrong reason, exercising the app a sibling configured.
- **Fix**: make the one-shot semantics honest and the later callers' needs checkable — store the built `App` in the `OnceCell` (see TESTING-2) and add a `booted()`/`boot_app()` accessor so a later call can assert what is actually registered (or have `boot` return a handle); and correct the docstring + setup.mdx to show one shared setup helper per file that all tests call, instead of each test passing its own closure. Real fix lives in `crates/umbral-testing/src/lib.rs` plus the two doc pages.

### TESTING-2 · MEDIUM · ergonomics/api · self-verified
**`boot()` builds an `App`, drops it, and offers no accessor — so `boot()` and `TestClient` cannot be combined, and getting a router after booting would panic.**

- **Where**: `crates/umbral-testing/src/lib.rs:576-578` (App dropped), `lib.rs:153` (`TestClient::new(router: Router)`), `crates/umbral-core/src/app.rs:219-227` (`into_router` is the only accessor), `crates/umbral-core/src/settings.rs:15-17` (second build panics)
- **Evidence**:
  ```rust
  // crates/umbral-testing/src/lib.rs:576-578 — the built App is a statement value, discarded
  configure(builder)
      .build()
      .expect("umbral-testing: App::build");
  ```
  ```rust
  // crates/umbral-core/src/app.rs:226 — consuming, the only way to a Router
  pub fn into_router(self) -> Router {
  ```
  ```rust
  // crates/umbral-core/src/settings.rs:15-17 — what a second App::build() does
  SETTINGS
      .set(settings.clone())
      .expect("umbral::settings::init called more than once");
  ```
  The crate's own Factory test works around this by re-implementing boot's body locally (`tests/factory.rs:39-57`: `App::builder()...build()` + `umbral_testing::create_tables()`), which is the shape a user must copy — there is no way to obtain the router from `boot`.
- **Impact**: a user follows setup.mdx ("boot is all three" of database, built app, tables) and then wants an HTTP round-trip the way test-client.mdx teaches it (`TempPool` + "build the router using pool.handle()", test-client.mdx:28). After `boot()` there is no pool handle either and no router; rebuilding the app panics per the quoted expect, so the whole setup half must be rewritten from `boot` to `TempPool` + manual `App::builder()` + `create_tables()`. The documented journey from ORM test to HTTP test hits a wall.
- **Fix**: stash the built app (or its router clone) in the same `OnceCell` and expose it (`booted_router()`, or make `boot` return `&'static App`); then `TestClient::new(umbral_testing::booted_router())` composes, and TESTING-1's "assert what's registered" also becomes possible. Alternatively add a `boot_with_router` companion documented next to `boot`.

### TESTING-3 · MEDIUM · drift · self-verified
**`TempPool`'s hand-rolled connect options skip the production PRAGMAs — most importantly `foreign_keys`, which is OFF here and ON in every production pool the ORM assumes.**

- **Where**: `crates/umbral-testing/src/lib.rs:72-85`; the production contract at `crates/umbral-core/src/db.rs:660-673` (applied by `sqlite_options`, `db.rs:702`)
- **Evidence**:
  ```rust
  // crates/umbral-testing/src/lib.rs:74-85 — filename + create_if_missing + busy_timeout only
  SqliteConnectOptions::new()
      .filename(&path)
      .create_if_missing(true)
      // ... Mirrors the 5s busy-timeout the framework's real
      // `connect_sqlite` applies to production pools.
      .busy_timeout(std::time::Duration::from_secs(5)),
  ```
  ```rust
  // crates/umbral-core/src/db.rs:661-663 — the full production set
  /// Applies the standard production PRAGMAs to every connection in the
  /// pool: WAL journal, NORMAL synchronous, a 5-second busy-timeout, and
  /// foreign-key enforcement on. Without these, a fresh `SqlitePool` ends
  /// up in `journal_mode = DELETE` + `synchronous = FULL` ...
  ```
  ```rust
  // crates/umbral-core/src/db.rs:673 — why FKs matter
  /// | `foreign_keys` | `ON` | sqlite turns FK enforcement off by default. The ORM emits
  /// `REFERENCES` clauses assuming they're respected — turning it on per connection makes the FK contract real. |
  ```
  The busy-timeout claim in TempPool's comment is true; the other three PRAGMAs are absent, so a TempPool connection runs with journal mode DELETE, synchronous FULL, and foreign keys off.
- **Impact**: concrete scenario — a test seeded through `TempPool` deletes a parent row that child rows still reference (or sets an FK column to an id that no row has via a write path that does not pre-probe). Under TempPool both succeed silently, leaving dangling references; in production the same statement raises a FK constraint error because the connection has `foreign_keys = ON` — which the framework's own docs say the ORM's `REFERENCES` clauses assume. The harness is strictly more permissive than production on the constraint class the ORM depends on, so a green test cannot prove the write is legal in production. The DELETE+FULL journal also makes the multi-connection pool exercise a lock behaviour production never sees (masked by the busy-timeout).
- **Fix**: construct TempPool's options via the same code path production uses (`umbral_core`'s `sqlite_options` / a shared helper) or at minimum add `journal_mode(WAL)` + `foreign_keys(true)` so the harness cannot diverge; keep the contention comment.

### TESTING-4 · LOW · drift
**The manifest leaves sqlx's default features on, re-enabling the `macros`/`migrate` machinery the facade deliberately excludes from every other crate.**

- **Where**: `crates/umbral-testing/Cargo.toml:24` vs `crates/umbral/Cargo.toml:29-41`
- **Evidence**:
  ```toml
  # crates/umbral-testing/Cargo.toml:24
  sqlx = { version = "0.8", features = ["sqlite", "postgres", "runtime-tokio"] }
  ```
  ```toml
  # crates/umbral/Cargo.toml:41 (comment above it explains the exclusion)
  sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "derive", "sqlite", "postgres", "chrono", "uuid", "json"] }
  ```
  sqlx 0.8's defaults are `["any", "macros", "migrate", "json"]`, so this crate (which downstream apps pull in as a normal dependency for their test builds) puts `sqlx-macros` and the migrate machinery back into the unified feature graph on every `cargo test`, undoing the facade's stated choice.
- **Impact**: build-surface drift, not a behaviour bug — consumers' test builds compile sqlx-macros/migrate for no use, and any future `cfg(feature = "macros")` decision in the framework would see an inconsistent graph. No DATABASE_URL requirement is introduced by merely enabling the feature.
- **Fix**: mirror the facade: `default-features = false` with the minimal explicit features this crate uses (`sqlite`, `postgres`, `runtime-tokio`, and `derive` only if its tests need the derive — the example models in `tests/` write `sqlx::FromRow`).

### TESTING-5 · LOW · docs
**The `Factory` docstring claims its bound set is "exactly what `#[derive(Model)]` already provides" — the derive provides none of `Serialize`/`FromRow`; they are sibling derives on the model.**

- **Where**: `crates/umbral-testing/src/lib.rs:440-446`
- **Evidence**:
  ```rust
  /// The model this factory produces. The bound set is exactly what
  /// `#[derive(Model)]` already provides on every model (the ORM's
  /// `create` path needs `Serialize` + `FromRow` + `HydrateRelated`), so
  /// in practice you only ever write `type Model = YourModel;`.
  ```
  `#[derive(Model)]` emits the `Model`/`HydrateRelated` impls only — its own crate documents models as `#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]` (`crates/umbral-macros/src/lib.rs:20,40,313`) and its diagnostics refer to the "SIBLING `#[derive(sqlx::FromRow)]`" (`lib.rs:1840-1844`). The `Model` trait itself (`crates/umbral-core/src/orm/model.rs:194`: `pub trait Model: Sized + Send + Sync + Unpin + 'static`) carries no such supertraits either.
- **Impact**: for the standard model shape the practical advice ("you only write `type Model = ...`") still holds, but a user whose model lacks the sibling derives (or who is diagnosing why the factory impl does not satisfy the bounds) is told the derive already provides them and looks in the wrong place; the compile error lists `serde::Serialize`/`sqlx::FromRow` bounds with no hint they are separate derives. Polish-grade documentation issue.
- **Fix**: reword to "plus the sibling derives every model carries (`sqlx::FromRow`, `Serialize`)" and, since the doc example is `ignore`d, make it compile-tested so wording drift is caught.

### TESTING-6 · INFO · ergonomics
**Where**: `crates/umbral-testing/src/lib.rs:221-240` — `request()` builds headers by appending (`http::request::Builder::header` is append, not insert), so `set_default_header(CONTENT_TYPE, …)` followed by `post_json`/`put_json` sends two `Content-Type` headers; axum reads the first, so the JSON helper can silently post under the default's media type (e.g. a 415 from a form-encoded default). One-line polish: overwrite rather than append the content-type the helper sets itself.

## Strengths

- **`create_tables` is the crate's best idea, and its docstring says why.** The schema is derived from the same `ModelMeta` the migration engine renders for `migrate` (`create_tables_for_tests`, `crates/umbral-core/src/migrate.rs:2278`), so test DDL is structurally identical to production DDL instead of a second source of truth; the docstring names the exact failure mode it removes ("PASSES WITH THE WRONG ANSWER", `lib.rs:497-501`), which is the right way to document a harness.
- **The risky claims are behaviourally proven, not asserted.** `tests/schema.rs` loops every registered model through `pragma_table_info` and asserts schema == models plus UNIQUE/default/FK enforcement and `create_tables` idempotence; `tests/plugin_tables.rs` proves the "a plugin's own models get their tables, you don't list them" claim end-to-end by booting `AuthPlugin` and using `auth_user` with no schema in the test file. That is the "real rows, real public path" test standard this sweep's rubric asks for.
- **The orphan-rule marker design is right and explained where it lives** (`lib.rs:404-410`): `impl Factory for PluginFactory` on a local zero-sized type, with the reason (both the model and the trait are foreign downstream) written down rather than left for a reader to re-derive.
- **`Factory::create` goes through the real ORM write path** (`umbral::orm::Manager::<Self::Model>::default().create(...)`, `lib.rs:470-472`) — no test-only insert shortcut, so factories exercise event hooks and validators like production writes do.
- **`TestClient`'s cookie handling has the right semantics for auth flows**: same-name cookies are replaced (`retain` then `push`, `lib.rs:118-124`), `Set-Cookie` values are harvested and replayed on the next request, and `body_json` panics with the raw body in the message on decode failure — a diagnostic that answers "what did the server actually return" without a debugger.
- **The cookie jar's limits are stated, not hidden** ("not RFC 6265 compliant (no domain, path, or expiry tracking)", `lib.rs:102-106`) — honest scoping for a test helper.
- **Small-machine hygiene done on purpose**: `FactoryError` with `From<WriteError>` so `?` flows, `create_batch` uses `Vec::with_capacity(n)`, `seq()` is an atomic counter (Relaxed is correct — uniqueness needs atomicity, not ordering).

## Clean areas checked

- **`cfg` divergence**: no `cfg(test)` / `cfg(not(test))` in `src/` — the harness never changes shape between test and production builds of itself, and `boot`'s build path uses the same `App::builder()` as production. (`audit_2` SC-7 recorded the same for the earlier revision; re-checked.)
- **Raw SQL**: none in `src/`; `sqlx::query` appears only in `tests/` (integration-style test code seeding rows), which is the allowed side of the ORM-not-raw-SQL rule for tests, not plugin code.
- **`unsafe`**: 0 hits.
- **Panics**: every `expect` in `src/` is boot/config-time with a message naming the failing stage (pool, settings, `App::build`, schema); nothing reachable from a request path panics except `body_json` on a malformed body, which is documented and the intended diagnostic.
- **Locks across await**: `TestClient::request` takes the `default_headers`/`jar` guards only in synchronous header-building and `Set-Cookie` harvesting blocks; every temporary guard is dropped before `oneshot(req).await`, so the future stays `Send` and no lock is held across an await.
- **Race safety of the one-shot build**: `OnceCell::get_or_init` serializes concurrent `boot` calls safely — exactly one initializer runs (the semantic problem is TESTING-1, not unsoundness).
- **`seq()` correctness**: `AtomicU64::fetch_add(…, Relaxed)` gives unique values under any interleaving; ordering is irrelevant to its contract.
- **Manifest**: `publish = true` is justified in-line (a user-facing dev-dependency crate, not scaffolding), deps are all normal leaf crates, and `umbral-auth` is dev-only so no plugin leaks into consumers' release graphs.
- **Doc accuracy spot checks**: TempPool busy-timeout vs production (true, and cited here for the rest of the divergence), `create_tables` idempotence (asserted by `tests/schema.rs`), and the crate docs' claim that TestClient replays cookies and default headers (matches `request()`).

## Known issues already tracked elsewhere (not counted in the totals)

- `review_3/codex 14` — "Global OnceLock State Limits Multi-App And Test Isolation" (STATUS 2026-08-16: BY-DESIGN; recommendation was to document the one-app-per-process assumption). This is the tracked ancestor of the single-global design; TESTING-1 (silently dropped closure + the doc claim built on it) and TESTING-2 (dropped `App`) are distinct defects that survive that decision and are not recorded there.
- `features #79` — "Factory testing library" (SHIPPED) — the write-up that introduced `boot`, `create_tables`, `Factory`; it repeats the "later calls are no-ops, so each test just says what it needs" framing, so it is context for TESTING-1 rather than a fix.
- `audit_2 SC-7` — testing crate publishable with deps in `[dependencies]` (done; verdict: no code change required, helpers are inert).
- `gaps3 #78` — migrating the 266 hand-written test schemas onto `create_tables()` (open follow-up noted in features #79).

## Out-of-scope notes

- `crates/umbral-core/src/db.rs:686` — `connect_sqlite("sqlite::memory:")` is backed by a process-wide temp file (`/tmp/umbral_mem_<pid>_<seq>.sqlite`, documented at `db.rs:675-687`); core-owned, but the user-facing testing docs never mention that `boot` leaves an on-disk temp file for the process lifetime.
- `documentation/docs/v0.0.1/testing/setup.mdx:26` — the user-facing copy of boot's dropped-closure claim (TESTING-1); `test-client.mdx:22-29` — the TempPool+manual-router path that is the documented workaround for TESTING-2 and the entry point for the TempPool divergence in TESTING-3. Doc and harness fixes must land together.
- `crates/umbral-testing/tests/factory.rs:39-57` — the crate's own test for the Factory layer re-implements `boot` locally instead of calling `umbral_testing::boot`; cited inside TESTING-2 as the in-tree workaround, and it is why no test proves the documented `boot` + `Factory` pairing.
