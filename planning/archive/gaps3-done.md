# Closed gaps - Continued from @gaps3.md

Shipped write-ups for entries opened in `planning/gaps3.md`. Same numbers; the active file keeps a one-line `[x] ... — archived` stub in place.

---

1. [x] REST: `views([...])` means read-only *everywhere* (routes, OPTIONS, OpenAPI, 405)

The `.views([Action::List, Action::Retrieve])` scope already gated request-time access (it 404'd a scoped-out action), but the scope leaked in three places: the `OPTIONS` `Allow` header was hardcoded, the OpenAPI spec always emitted `post`/`put`/`patch`/`delete`, and a blocked write returned `404` (implying the URI doesn't exist) instead of `405` (the URI exists, this method doesn't). Three surgical changes, one design decision.

**Design decision — OPTIONS reflects what's *mounted*, never the permission class.** `Allow` is defined by HTTP as the methods the *target resource* supports — a property of the route, not of who's asking. Folding the permission class into it would hand two callers different `Allow` headers for the same resource (breaking caching/codegen) and conflate two orthogonal mechanisms. So `view_scope` (+ the `.bulk()` opt-in for collection PATCH/DELETE) is the *only* input to `Allow`. When `views()` isn't set, every verb stays advertised (backward-compatible). A resource that wants OPTIONS to advertise only `GET` says so with `.views([List, Retrieve])`, not with a `ReadOnly` permission.

**What changed:**

- `plugins/umbral-rest/src/lib.rs`
  - New `EndpointKind { Collection, Detail }` and `RestPlugin::exposed_methods(table, kind) -> Vec<&'static str>` — the single source of truth for the verb list, honoring `view_scope` and `.bulk()`. OPTIONS is omitted (always present); callers prepend it.
  - `gate(table, action, kind, identity)` gained the `kind` arg. When an action is scoped out it now distinguishes: endpoint still serves some verb → `ApiError::MethodNotAllowed { allow }` (405 + `Allow`); endpoint serves nothing → `404` (the URI genuinely isn't served). All 8 call sites pass the right `EndpointKind` (custom-action dispatch derives it from `ActionScope`; custom actions never hit the 405 branch since `view_exposed` is always true for them).
  - New `ApiError::MethodNotAllowed { allow: String }` variant + early-return in `IntoResponse` that sets the `Allow` header (mirrors the `Throttled` `Retry-After` pattern).
  - `collection_options` / `detail_options` rewritten to build `Allow` from `options_allow(table, kind)` → `exposed_methods`. `detail_options` gained `Path((table, _id))` so it can consult per-table scope.
  - New public `action_exposed(table, &Action) -> bool` (reads `view_exposed` off the ambient `CONFIG`; defaults to `true` when CONFIG is unset, matching `is_exposed`) — the seam OpenAPI consumes.
- `plugins/umbral-openapi/src/lib.rs`
  - `collection_paths` / `item_paths` build their operation maps conditionally on `umbral_rest::action_exposed(...)`, so a scoped-out action emits no operation.
  - New `has_operations(path_item)` helper; the caller skips inserting a path that ends up with no HTTP operations (e.g. `views([List])` leaves the detail URI with only an `id` parameter).

**Tests:**

- `plugins/umbral-rest/tests/options.rs` — added a `views([List, Retrieve])` `Doc` resource; new tests assert the collection and detail `OPTIONS` advertise only `OPTIONS, GET`.
- `plugins/umbral-rest/tests/auth_permission.rs` — the three `opt_in_views_*_returns_404` tests became `*_returns_405_with_allow` (assert 405 + `Allow` lists served verbs, excludes scoped-out ones). Added a `catalog` resource with `views([List])` and two tests: collection POST → 405, detail GET → 404 (serves nothing, no `Allow` header).
- `plugins/umbral-openapi/tests/integration.rs` — added an `oa_readonly` resource scoped to `views([List, Retrieve])`; new test asserts the spec keeps `get` on both paths but omits `post`/`put`/`patch`/`delete`.

**Docs:** new `documentation/docs/v0.0.1/rest/views.mdx` — purpose, one example, the 405-vs-404 split, and the views-vs-permissions distinction.

Behavior change to note: a write to a view-scoped resource now returns `405` (with `Allow`), not `404`, whenever the endpoint still serves another verb. The previous "always 404" was a weaker, less HTTP-correct signal.

---

4. [x] Flash messages no-op without a pre-existing session — resolved (works with SessionsPlugin; was a test-harness misconfig + doc error)

The original framing (logged during the Task 14 review of the auth form-action surface) claimed flash feedback was silently dropped for an anonymous first-visit form failure because `Messages::add` requires a session token and umbral sets cookies explicitly. That was wrong. `session_layer` (mounted by `SessionsPlugin::wrap_router`, default-on) injects a candidate `SessionToken` into every request extension including cookieless ones (the `fresh = true` path). `Messages::from_request_parts` prefers this extension over the raw cookie, so on a brand-new anonymous visitor's first submit: `session_layer` provides the token → `Messages::add` materialises the session row (lazy side-channel write) → `session_layer` emits `Set-Cookie` on the response. Flash feedback for anonymous first-visit failures works end-to-end **when `SessionsPlugin` is mounted** (which any flash-using app has).

The only configuration where it breaks is `AuthPlugin` booted ALONE without `SessionsPlugin` — a degenerate test-harness config, not a real app config. The `form_surface.rs` test used exactly that boot; the fix (commits 60082a7/4ba53f8 on feat/auth-full-surface) mounts `SessionsPlugin` in the test and asserts the session cookie is set on a failed login, and repoints the `form-endpoints.mdx` Callout from CSRF to SessionsPlugin as the session-establishing layer.

---

6. [x] Admin dashboard widget catalog now filters by `widget.permission`

Surfaced by the custom-views (features #76) final review. `GET /admin/api/dashboard/catalog` (`handlers::dashboard::dashboard_catalog`) built its entry list from `state.widget_catalog` unconditionally, so a user without a widget's codename saw it in the "add widget" picker, added it, then got a 403 on the data fetch (the data endpoint IS gated). A UX gap, not a security hole. Fix (commit `0718300`): capture the user from `require_staff` and skip any widget whose `permission` codename the user lacks (`permcheck::has_codename`), mirroring the check `dashboard_widget_data` already enforces. Graceful no-op preserved (absent `PermissionsPlugin` → all shown). Test `test_catalog_filters_by_widget_permission` in `tests/custom_views.rs` (gated widget absent for `cv_staff`, present for `cv_priv`).

---

7. [x] Custom-view paths are validated at build; a bad path no longer panics the router

Surfaced by the custom-views final review. `AdminPlugin::routes()` mounted `GET {base}/{view.path}` per registered view with no validation, so a view whose path was empty, whose first segment shadowed a built-in admin route (`login`/`logout`/`upload-image`/`api`), or that duplicated another view made axum's router `panic!` on a route conflict at boot. Fix (commit `a8f518d`): new `AdminPlugin::resolved_custom_views()` drops such views up front with a clear `tracing::error!`, and `routes()` (widget flatten, gate map, `AdminState.custom_views`, mount loop) plus `route_paths()` all read the resolved list — so a rejected view is absent from the router AND the sidebar, and the rest of the admin keeps serving. Multi-segment paths (`reports/sales`) coexist with the `{table}/` changelist route via axum static-over-param precedence, so only built-in *static* first segments are reserved. Unit test `resolved_custom_views_drops_reserved_and_duplicate_paths` in `lib.rs` (asserts the dropped set + that `routes()` no longer panics). Not fixed: a single-segment view path that shadows a real model table's changelist — that's a soft footgun (static wins, no panic), left as a documented caveat.

---

8. [x] Per-widget permission checks batched (concurrent, deduped)

`view::accessible_widget_sections_json` (the render filter for the dashboard + custom views) resolved each widget's codename with a sequential `has_codename` await, so a page with N permissioned widgets paid N sequential DB round-trips. Fix (commit `aaaa7ef`): collect the DISTINCT codenames across all widgets, resolve them ONCE and CONCURRENTLY via `futures_util::future::join_all` (already a dep, used by the parallel dashboard COUNTs), then filter widgets against the resulting `codename → bool` map. The render is now a single round of concurrent lookups regardless of widget count. Behavior is unchanged — the existing `test_widget_permission_filters_dashboard_render` regression test still passes.

---

11. [x] Auth JSON routes are slash-inconsistent with REST resources → `/api/auth/login/` 404s under the default `SlashRedirect::Append`

Found building web3clubs_fc. `AuthPlugin::with_default_routes()` registered the JSON auth routes WITHOUT a trailing slash (`POST /api/auth/login`, `/register`, `/me`, `/logout`), but `RestPlugin` resources use a TRAILING slash and the `startproject` scaffold turns on `.slash_redirect(SlashRedirect::Append)`, so hitting `/api/auth/login/` (the natural try) 404'd — Append only redirects a no-slash request TO the slash form, which doesn't help when only the no-slash form is registered.

**Fixed** (commit `4f30cc4`): `build_router` now binds every auth route at BOTH the bare and trailing-slash form (option (b) from the write-up), so either path resolves regardless of the app's redirect policy. Test: `both_slash_forms_of_login_resolve` in `plugins/umbral-auth/tests/json_surface.rs` asserts `/api/auth/login` and `/api/auth/login/` resolve to the same handler (neither 404s).

12. [x] `GET /oauth/{provider}/login` returns 500 (not 404) for a provider key that isn't registered

`begin_flow` (login/connect) and `oauth_callback` called `server_error` (500) when `plugin.lookup(provider)` was `None` — but an unregistered/misspelled provider key is a foreseeable client input.

**Fixed** (commit `e6efb7a`): both sites return `404 Not Found` with `unknown or unconfigured oauth provider \`<key>\`` instead. Test: `plugins/umbral-oauth/tests/unknown_provider.rs` drives `/oauth/nonexistent/login` through the mounted route + session layer and asserts 404.

13. [x] SQLite `AlterColumn` (null-flip / type change) fails with `FOREIGN KEY constraint failed` when the table has inbound FKs

The table-recreation dance (CREATE new / copy / DROP old / RENAME) died with SQLite error 787 when the altered table had inbound FKs, because step 3's DROP ran under `foreign_keys=ON` (set by `connect_sqlite`) with child rows still referencing it. This dead-ended the declare→migrate loop for any relational schema.

**Fixed** (commit `a60405a`): new `apply_sqlite_migration_tx` helper applies SQLite's official recipe — on a PINNED connection, `PRAGMA foreign_keys=OFF` OUTSIDE the tx (a no-op inside one), run the dance, `PRAGMA foreign_key_check` before commit (a genuinely orphaning migration still aborts), commit, restore `foreign_keys=ON` even on failure so the pooled connection never returns unsafe. Both the primary (`run_in_sqlite_for_alias`) and tenant SQLite apply loops route through it; Postgres uses native ALTER and is unaffected. Note: my original diagnosis suggested `PRAGMA foreign_keys=OFF` in the *executor*; `defer_foreign_keys` was tried first and STILL failed the commit-time check, so the connection-level `foreign_keys=OFF` recipe is the one that works. Test: `crates/umbral-core/tests/alter_column_inbound_fk.rs`.

14. [x] `update_or_create`'s UPDATE branch emits `bulk_post_save`, not per-row `post_save` — silently bypasses signal/realtime consumers

`umbral-realtime`'s `on_model` bridge (and `umbral-signals` `on_model().post_save`) subscribe to per-row `post_save:<table>`. `update_or_create` emitted NO per-row `post_save` on EITHER branch — the CREATE branch calls `create()` (deliberately signal-free, like `bulk_create`; only `save()` fires per-row signals) and the UPDATE branch uses `update_values` (`bulk_post_save`). So `on_model` consumers silently missed every upsert. (The original write-up mis-stated that the CREATE branch already emitted `post_save` via `self.create()` — it doesn't; both branches were silent.)

**Fixed** (commit `fe200c1`): emit `post_save` explicitly on both branches (the create arm and both `do_update!` update sites). `create()`/`bulk_create` stay signal-free by design — only the higher-level convergent upsert notifies. `bulk_post_save` and `post_save` are distinct signal names, so no single consumer double-fires. Test: `post_save_fires_on_both_branches_of_update_or_create` in `crates/umbral-core/tests/update_signals.rs`.

19. [x] `AuthUser` isn't extensible — CONFIRMED already solved by the swappable `UserModel` mechanism (no new code)

The complaint: to add `display_name`/`color`/`position` to a user, the consumer built a `UserProfile` (unique FK) + a `post_save` on `AuthUser` to auto-create it + an idempotent `ensure_profile` racing the unique FK + a `backfill_profiles` seed + an in-memory left-join on every read.

**Confirmed done** (2026-07-06 audit, no new code needed): the "swappable user model (Django `AUTH_USER_MODEL`)" arm of the proposal already exists as the `UserModel` trait + the generic `AuthPlugin<U: UserModel = AuthUser>` (`plugins/umbral-auth/src/lib.rs:183`, `:356`). A consumer declares their OWN user struct with whatever extra columns they need and implements four required methods (`id`/`username`/`password_hash`/`set_password_hash`; the three flag methods default). `AuthPlugin::<CustomUser>::default()` registers it and `authenticate` / `set_password` / bearer-token / argon2 hash-verify all operate generically over `U`. That eliminates the sidecar-profile apparatus outright — the extra fields live directly on the user model instead of a `UserProfile` + signal + backfill + join. The PK is polymorphic (`<U as Model>::PrimaryKey`), so a `uuid::Uuid`- or `String`-keyed user works too.

Proven by existing tests: `plugins/umbral-auth/tests/custom_user.rs` (9 tests — `CustomUser` carries `display_name` + `tenant_id`, the exact "extra columns AuthUser doesn't have" case; registers via `AuthPlugin::<CustomUser>`, authenticates, rejects wrong-password/inactive, rotates password) and `plugins/umbral-auth/tests/uuid_user.rs` (4 tests — non-`i64` PK). Both green on 2026-07-06.

**Documented caveat** (not a gap): a *fully* custom user model brings its own request-time extractor / `current_user` loader and its own default routes — the built-in extractors, `session_user::current_user`, and the `with_default_routes()` opt-in are `AuthPlugin<AuthUser>`-only by design (they hardcode the `auth_user` shape). The generic auth CORE that #19 was about (define a richer user model, stop rebuilding the profile apparatus) is complete. If a first-class profile *helper* is ever wanted as the second arm, that's a fresh, lower-priority entry — this one is closed on the swappable-model arm.

20. [x] Auth ships no authenticated change-password route, and `set_password` skips the strength policy — shipped (commit 926d1c11)

Default auth routes were login/logout/signup/verify-email/resend/password-forgot/password-reset — no change-password. `umbral_auth::validate_password` was public (the app reinvented an 8-char check) but `set_password` didn't call it.

**Shipped** (commit `926d1c11`): `change_password(user, current, new)` — verify the current password via `verify_password_async` (else `InvalidCredentials`) → run `validate_password` on the new one (else `WeakPassword`) → rotate the hash via `update_values`. Plus a default `POST {prefix}/change-password` route mapping the outcomes (204 success; 401 invalid_credentials; 400 weak_password). TDD: `plugins/umbral-auth/tests/change_password.rs`.

23. [x] No `serve`-only migrate/seed lifecycle — apps hand-roll argv sniffing — shipped (commit aad2c684)

The consumer inspected `std::env::args()` in `main.rs` to avoid `auto_migrate()` firing during the `makemigrations`/`migrate` CLI commands.

**Shipped** (commit `aad2c684`): `AppBuilder::auto_migrate_on_serve()` sets an opt-in flag on the built `App`, read via `auto_migrate_on_serve_enabled()`. The CLI `serve` path runs `migrate::run()` before binding the listener *only* when the flag is set, so the `makemigrations`/`migrate` subcommands never trip an ambient auto-migrate. TDD: `crates/umbral-core/tests/auto_migrate_on_serve.rs`.

24. [x] Adding a `Choices` variant forces a full `AlterColumn` table rebuild (unnecessary churn) — shipped (commit a86967a6)

`fc_payments` migrated `status` to `Choices` then added a `"waived"` variant — each a full `AlterColumn` (whole-table rebuild on SQLite) though the storage type stayed `Text`.

**Shipped** (commit `a86967a6`): `alter_is_choices_only` — the SQLite `AlterColumn` renderer short-circuits to *no DDL* when the only column difference is `choices`/`choice_labels` (choices aren't a DB CHECK on SQLite; `build_column_def_sqlite` emits none, so the rebuild produced a byte-identical table). Postgres, which stores `CHECK (col IN (...))`, still swaps the constraint via its own renderer. The op is still recorded; only the SQLite render is empty. TDD: `crates/umbral-core/tests/choices_only_alter_sqlite.rs`.

26. [x] Admin sheet reads flake under extreme concurrent load — product bug fixed; residual is a test-only artifact (2026-07-06 audit)

Root-caused while chasing the test-suite flake. Two separable things.

**The product bug — FIXED** (commit `3718206c`): `require_staff` (`plugins/umbral-admin/src/auth.rs`) folded a `current_user` LOOKUP FAILURE into a login redirect, silently logging a *staff* user out on a transient DB error (and the admin sheet tests then saw a redirect instead of the fragment). It now distinguishes `Ok(None)` (genuinely not authenticated → login redirect) from `Err(e)` (a DB hiccup → log the real error, return an opaque 500). Verified present in the tree on 2026-07-06.

**The residual — a test-only artifact, NOT eliminated, but production-safe.** #26 also tracked the underlying READ-contention: under artificial `nproc` CPU saturation, `umbral_auth::current_user` (session read + `AuthUser::objects().first()`) can error `database is locked` because a concurrent writer holds SQLite's EXCLUSIVE lock during COMMIT longer than `busy_timeout` while descheduled. `BEGIN IMMEDIATE` (gaps3 #25) is a WRITE-path fix and does not address a read. **Production is unaffected**: `connect_sqlite` opens every real pool in WAL + NORMAL + 5s busy_timeout (`crates/umbral-core/src/db.rs:539-601`), so readers don't block on a writer's commit. The residual only reproduces on the *test* pools some files build with raw `SqlitePoolOptions` that set `busy_timeout` but not WAL, and only under `nproc` CPU saturation (~once per full-workspace sweep).

**Disposition** (user call, 2026-07-06): closed as a known test-only artifact. No product bug remains; chasing a CPU-saturation-only test flake isn't warranted (Postgres-first, production provably safe). If it ever starts flaking normal runs, the fix is the entry's option (a): give the offending test pools WAL at DB-create time (mirroring `connect_sqlite`), not a per-connection PRAGMA on a hammered file DB (observed making `m2m_writethrough` worse).

21. [x] `DecimalField` / money type — CONFIRMED already shipped for Postgres (2026-07-06 audit; SQLite deferred)

The complaint: `Fixture.fee_amount` / `Payment.amount` were `i64` whole-shillings by comment; proposal was a `DecimalField` (PG `NUMERIC`, SQLite `TEXT`-backed rust_decimal value).

**Confirmed done for Postgres** (no new code): `rust_decimal::Decimal` already classifies as `SqlType::Decimal` → `NUMERIC(19, 4)` and is wired end-to-end — derive detection (`umbral-macros/src/lib.rs:2642,2749,3044`), `DecimalCol`/`NullableDecimalCol` predicate columns (`umbral-core/src/orm/column.rs:2739+`), `Option<Decimal>` nullable support (closed gaps2 #70), backup/restore (`backup.rs`), and `inspectdb` reverse-mapping (`inspect.rs:694`). Tests: `crates/umbral-core/tests/decimal_field.rs` (3 pass: classification + nullable; live PG round-trip `#[ignore]`'d). Docs already accurate: `documentation/docs/v0.0.1/orm/column-types.mdx:39-40,575` document it as Postgres-only with `NUMERIC(19,4)`. So a money app on Postgres uses `rust_decimal::Decimal` today; the stale gap text ("consumers fall back to i64") predates that.

**SQLite deferred, deliberately** (user call, 2026-07-06 — "close as Postgres-done"): the SQLite half of the original proposal is intentionally not done. `SqliteBackend::map_type` panics on `Decimal` and the boot system-check (`check.rs:836`) rejects a Decimal field on SQLite with a clear message, because sqlx-sqlite ships no `rust_decimal` Encode/Decode. Supporting it would need an umbral-owned `Decimal` newtype with a TEXT-backed dual-backend codec (plus a decimal-ordering-on-TEXT caveat for range queries) — real work with an API shift, and Postgres-first means money apps test on Postgres. If SQLite money-model dev/test is ever needed, that's a fresh entry: umbral `Decimal` newtype + TEXT codec.

27. [x] audit_2 residual low-severity hardening backlog — all 9 items shipped (2026-07-06)

From a full re-triage of the untouched `planning/audit_2/findings/*.md` against current code: the CRITICAL/HIGH findings were all already fixed (the files just weren't re-annotated). These nine small, no-live-infra items were the genuine residue — each shipped this session, TDD'd, one commit apiece:

- **[authz S3]** — CSRF secret resolved per-request instead of captured at `wrap_router`, closing the build-order gap that could silently pin plain double-submit. Commit `a6f21eda`.
- **[authz P5]** — `has_perm` DB error logged before the fail-closed deny (was `unwrap_or(false)`). Commit `d1973c93`.
- **[admin #6]** — image upload sniffs magic bytes (PNG/JPEG/GIF/WEBP signatures; SVG must be markup) and 415s a content-vs-declared mismatch. Commit `fb4416b1`.
- **[core-web #6]** — `SlashRedirect::alternate_path` refuses `//host` protocol-relative paths (open-redirect guard). Commit `73a03739`.
- **[core-web #7]** — `collectstatic`/`copy_tree` skips symlinks whose target escapes the source root (canonicalize + containment, mirroring `resolve_under_root`). Commit `73a03739`.
- **[macros-cli #7]** — scaffold generates a random per-project dev `secret_key` (OS-seeded RandomState) instead of the shared literal. Commit `a05373ef`.
- **[observability #9]** — Swagger UI carries SHA-384 SRI on the pinned default unpkg assets, omitted when the base is overridden (self-host). Commit `35aee2b9`.
- **[observability #12]** — deleted the stale `m2m_changed` "Deferred past v1" bullet in umbral-signals. Commit `34fb184e`.
- **[realtime #1]** — `cache_page` bypasses the shared cache on `Proxy-Authorization` and on `Vary: Cookie/Authorization/*`. Commit `091f0dcc`.

(The two MEDIUM audit items — `render_str` autoescape `cadc061e` and `SecurityConfig::production_hardened()` `725ee6c3` — shipped earlier the same day.) Remaining audit residue is the big-design / live-Postgres set tracked in gaps3 #28.

30. [x] SQLite `AlterColumn` (inbound FKs + data) → 787 — could NOT reproduce on main; already fixed in 0.0.5 (2026-07-07 investigation)

Reported (from web3clubs_fc) as the "sharper, breaking" version of #24: an `AlterColumn` on a SQLite table with inbound FKs AND existing data aborting with `FOREIGN KEY constraint failed` (787), the proposed fix being SQLite's `PRAGMA foreign_keys=OFF` rebuild recipe.

**Root cause of the report:** that exact recipe already shipped as **gaps3 #13** (commit `a60405a`, in `umbral-core-v0.0.5`, 2026-07-04) — `apply_sqlite_migration_tx` brackets the rebuild with `PRAGMA foreign_keys=OFF` (outside the tx) → rebuild → `PRAGMA foreign_key_check` before commit → `PRAGMA foreign_keys=ON`, on a pinned connection. **web3clubs_fc is on 0.0.4**, which predates it — that's why it 787'd there.

**Verified NOT reproducible on main.** Every migrate entrypoint funnels through the fixed apply function: CLI `migrate` → `run_checked` → `run_in_sqlite_for_alias` → `apply_sqlite_migration_tx` (and `run()`/`run_in` likewise). `connect_sqlite` sets `foreign_keys(true)` (db.rs:604), so enforcement really is on. A new engine-driven test drives the REAL path end to end against a `connect_sqlite` pool, with the exact #30 shape — a `fixture` hub, three inbound-FK children (`attendance`/`goal`/`payment`), seeded rows, and a migration carrying BOTH pending alters (opponent `NOT NULL`→nullable rebuild + `status`→`Choices`, a SQLite no-op per #24): it applies cleanly, the hub row + every child FK row survive, and `opponent` is nullable afterward. No 787.

**What was added:** `crates/umbral-core/tests/alter_inbound_fk_engine.rs`. The pre-existing `alter_column_inbound_fk.rs` (#13) applied the recipe BY HAND — it proved the recipe works but never that the engine *uses* it; this new test closes that gap and guards against a future refactor dropping the FK-off bracket. No production code change was needed.

**Action for the reporter:** upgrade web3clubs_fc from 0.0.4 to 0.0.5+ (or the upcoming 0.0.6) to get the fix; the null-flip migrations it avoided will then apply against a populated local SQLite DB.

31. [x] `#[derive(Choices)]` fields decode as TEXT but pre-0.0.5 migrations made the column VARCHAR → typed reads 500 on Postgres (2026-07-07)

Found in the web3clubs_fc backend (live consumer, umbral 0.0.4). A field `status: FixtureStatus` where `FixtureStatus: Choices` generated a `VARCHAR(20)` column, but the sqlx `Type` impl the `Choices` derive emits reports `TEXT`. On **SQLite** `VARCHAR` and `TEXT` share affinity, so all dev/tests passed. On **Postgres** sqlx is strict:

```
error occurred while decoding column "status": mismatched types;
Rust type `FixtureStatus` (as SQL type `TEXT`) is not compatible with SQL type `VARCHAR`
```

Any **typed** ORM read of a row containing a Choices column 500'd (REST list endpoints escaped — they decode dynamically, not into the typed struct — so the bug hid in custom handlers). In the consumer, `POST /api/fixture/{id}/rsvp` did a typed `Fixture::objects()...` existence check, so **every RSVP 500'd and nothing persisted** — production-down for a core feature, invisible in dev.

**Root cause.** The `Choices` derive (`crates/umbral-macros/src/lib.rs`, `derive_choices`) emitted `Type<Postgres>::type_info()` = `String::type_info()` (TEXT) but never overrode `compatible()`, so it used the trait default (`ty == Self::type_info()`), which rejects any column that isn't exactly TEXT. `String` itself reads from a VARCHAR column precisely because *its* `compatible()` accepts the whole text family (TEXT / VARCHAR / BPCHAR / NAME / citext).

**Shipped:** both `Type<Sqlite>` and `Type<Postgres>` impls the derive emits now override `compatible()` to delegate to `String`, so a Choices enum decodes from any string-family column. This is a **decode-side** fix — existing `VARCHAR` columns (0.0.4 migrations, DR restores) start decoding with **no migration**; the manual `ALTER COLUMN ... TYPE TEXT` prod hotfix is no longer required. (Separately, the current derive already classifies `#[umbral(choices)]` fields as `SqlType::Text`, so *fresh* migrations emit TEXT — the fix rescues the older VARCHAR schemas and makes decode robust to whatever type the column actually is.)

**Test:** `crates/umbral-core/tests/choices_varchar_pg.rs`. The core guard runs with **no database** — `Type::compatible` is pure, and `PgTypeInfo::with_name("varchar")` name-matches the built-in VARCHAR (unlike `with_oid`, which sqlx's `==` soft-eq would mask); it returns `false` before the fix and `true` after (confirmed by reverting the derive change and watching the test fail). A full typed round-trip over a real `VARCHAR` column is behind `#[ignore]` on `UMBRAL_TEST_POSTGRES_URL` — SQLite can't catch this class (VARCHAR ≡ TEXT affinity). Mirrors the "SQLite-only tests miss Postgres type strictness" theme (gaps2 #70, decimal_field).

32. [x] OAuth `begin_flow`'s `set_data(token,…)` on a fresh session emitted no session `Set-Cookie` when a CSRF cookie was present → "no oauth flow in progress" for cookieless clients (2026-07-07)

Found in web3clubs_fc (live consumer, umbral 0.0.4) — social login from a cross-origin Vite SPA. `GET /oauth/{p}/login` (`plugins/umbral-oauth/src/routes.rs::begin_flow`) persists the PKCE/`state` `FlowState` with the free `set_data(token, FLOW_KEY, &flow)` and redirects to the provider. On a fresh (cookieless) client the response came back with only `set-cookie: umbral_csrf_token` — no session cookie — so the flow row was written under a token the client never received. On the callback, `current_session(&headers)` found no session → flow `None` → `400 "no oauth flow in progress"`. It "worked randomly" only for clients that already held a session cookie (an admin already logged in, a prior session-writing request); a fresh SPA failed every time. With umbral's own server-rendered frontend the user usually already had a session, which is why it only surfaced through the API.

**Root cause — session layer, not OAuth.** The session auto-layer already materialises-on-write and has a `read_session` probe that catches side-channel `set_data(token,…)` writes on a fresh session — but its emit was gated on `!response.headers().contains_key(SET_COOKIE)`: "emit the session cookie only if NOTHING already set a cookie." The CSRF layer sets `umbral_csrf_token` on the very first request, so that guard saw a `Set-Cookie` and bailed. Any fresh request that both received a CSRF cookie and wrote the session lost its session cookie — OAuth `begin_flow` was just the consumer that hit it. The existing `state_csrf.rs` test missed it because it wraps ONLY the session layer (no CSRF cookie in the response).

**Shipped — session layer.** New `session_cookie_present(headers)` (`plugins/umbral-sessions/src/lib.rs`) checks specifically for a `umbral_session=` Set-Cookie (the session cookie name), not "any Set-Cookie". Both emit branches (the fresh-session emit and the non-fresh `CookieStore` re-set) now gate on it and `append` the session cookie instead of `insert`-ing it, so it coexists with the CSRF cookie rather than being suppressed by it (old guard) or clobbering it (`insert`). `login_with_request`'s token rotation, which sets its own `umbral_session` cookie, still short-circuits (name match), so session-fixation defense is unchanged. Fixes OAuth AND any other fresh + CSRF + `set_data` endpoint — no OAuth-side change needed.

**Test:** `plugins/umbral-sessions/tests/gaps3_32_session_cookie_beside_csrf.rs` — a fresh request through the session layer with an INNER layer that injects a `umbral_csrf_token` cookie (reproducing the real stack); asserts the response carries BOTH the CSRF cookie AND a `umbral_session` cookie, and that the emitted cookie names the row `set_data` actually wrote. Fails before the fix ("session Set-Cookie missing"), passes after — verified by reverting the guard. Full `umbral-sessions` / `umbral-oauth` / `umbral-auth` suites stay green (login rotation, sliding expiry, cookie-store re-set unaffected).

---

40. [x] **Cross-plugin FK ordering is decided alphabetically, and fails only on a fresh database.**

**Symptom.** The first umbralrs.dev deploy died in the `migrate` container with `ERROR: relation "auth_user" does not exist` / `STATEMENT: CREATE TABLE "accounts_git_hub_account" (... "user" bigint REFERENCES "auth_user"("id") ...)`.

**Root cause.** `App::build()` ordered plugins with Kahn's algorithm and broke ties with a `BTreeSet`, i.e. alphabetically. A plugin that declares no `dependencies()` has in-degree 0, so when *no* plugin declares any, every plugin is immediately ready and the "topological" order collapses to plain alphabetical order. `"accounts"` sorts before `"auth"`. The four other website plugins that FK into `auth_user` (`plugin_directory`, `reviews`, `showcase`, `site_content`) all happen to sort *after* `"auth"` — they were passing on luck, not on contract. Invisible everywhere a database already contained the referenced table, so the whole dev loop, every test, and every incremental deploy passed. Prod-only, first-run-only.

**Shipped — the sort now reads the edges the schema already states.** New `fk_plugin_edges()` (`crates/umbral-core/src/app.rs`) builds a `table -> owning plugin` map from the model registry and walks every column's `fk_target`, emitting one edge per cross-plugin physical FK. `sort_plugins()` unions those edges with `Plugin::dependencies()` before running Kahn's. A plugin author never has to hand-declare an ordering the `ForeignKey<T>` field already spells out.

Two exclusions, both deliberate: same-plugin FKs impose no *plugin* ordering (that's the diff engine's problem), and `#[umbral(db_constraint = false)]` (the cross-database shape, gaps2 #22) renders no `REFERENCES` clause and so creates no DDL ordering obligation. An FK into an app-owned table yields no edge — the implicit `"app"` plugin is already pinned last precisely because app models FK *into* plugin tables.

**Cycle attribution (the entry's "Bonus").** `toposort()` was extracted so it can run twice: once on the combined edge set, and — only if that cycles — once on the declared edges alone. If the declared graph is acyclic, the foreign keys introduced the cycle, and the new `BuildError::ForeignKeyCycle { edges: Vec<FkEdge> }` names each offending `plugin."table" REFERENCES "target"` plus the fix, instead of a bare `PluginCycle` the author never wrote. A declared cycle still reports `PluginCycle` unchanged. Note this is nearly unreachable across crates: `ForeignKey<T>` needs `T` in scope, so mutually-referencing plugin *crates* would be a circular Cargo dependency — Cargo enforces the FK DAG for us. Two plugins defined in one crate can still do it.

**Why not the other two proposals.** (a) `makemigrations` auto-populating `depends_on` and (b) `migrate` ordering off the recorded `depends_on` were both rejected in favour of deriving from the **model registry**, which is strictly better: it works before any migration file exists, needs no archaeology over migration JSON, and cannot drift from the models. `Migration.depends_on` stays written-as-`Vec::new()` and unread; it is now redundant rather than merely unused. (c) The separate boot-time preflight check became unnecessary — with the sort fixed, the only reachable violation *is* an FK cycle, which the build error above already reports before any DDL executes.

**Tests:** `crates/umbral-core/tests/fk_plugin_ordering.rs` — a dependent plugin that sorts alphabetically *first* and FKs a target plugin's table, with neither declaring `dependencies()`, must still order the target first (the exact prod shape); a `db_constraint = false` plugin must NOT gain an edge; an FK cycle reports `ForeignKeyCycle` naming the column; a declared cycle still reports `PluginCycle`. Whole workspace green (2563 tests).

**Consumer note.** The interim fix in `umbral_website` (commit `cae29c75`: five plugins declaring `dependencies() -> &["auth"]`) stays — declaring your own dependencies is still the plugin author's job, and it documents intent the schema can't. It is simply no longer load-bearing for correctness.

---

44. [x] **CSRF token visible in the admin's `hx-headers` body attribute.** (Reported as "this does not look secure"; the screenshot showed `<body hx-headers='{"X-CSRF-Token": "d88e…"}'>` on the admin dashboard.)

**The reported thing is not the bug.** umbral's CSRF is a double-submit scheme (`plugins/umbral-security/src/lib.rs`): the token is written to a deliberately non-`HttpOnly` cookie *and* rendered into the page, and `csrf_valid()` accepts a write only when the two match (constant-time via `subtle::ConstantTimeEq`). htmx has to read the value to put it on the wire, so it cannot be secret. The security property is same-origin: a cross-origin page can read neither the cookie nor the response body, so it cannot produce a matching pair. Tokens are additionally HMAC-signed under `secret_key` (`signed_csrf: true` by default), so a sibling subdomain can't plant a forgeable cookie. Exposure in HTML is by design.

**The real defect the screenshot pointed at: nothing marked authenticated responses uncacheable.** Admin HTML was emitted as a bare `200 text/html` with no `Cache-Control` at all (`plugins/umbral-admin/src/engine.rs`), and the security-header bundle (`lib.rs`, ~340-370) set `x-content-type-options`, `x-frame-options`, `referrer-policy`, HSTS, CSP and friends but never `Cache-Control`. Session `Set-Cookie` responses carried none either. umbral's *own* `cache_page` middleware was already safe (it bypasses on the `umbral_session` cookie, on any `Set-Cookie`, and on `Vary: Cookie`), so the exposure was to every cache umbral does not control: a CDN, a corporate proxy, or the browser's own back/forward cache storing one user's token-and-data-bearing admin page and replaying it.

**Shipped.** `SecurityConfig::private_cache` (default `true`) adds `private_cache_middleware`: when a request is *personalised* it attaches `Cache-Control: no-store, private` to the response. `private` bars shared caches; `no-store` bars the local disk/BFCache, which is what stops Back-after-logout rendering the admin. Personalised means a `umbral_session` cookie, an `Authorization` header, or a `Proxy-Authorization` header — the same predicate `umbral-cache`'s `request_is_personalised` uses. Deliberately **not** a signal: the `umbral_csrf_token` cookie, since every first-time anonymous visitor is minted one and keying on it would mark the whole public site `no-store`. The header is `if_not_present`, so a handler that already declared its caching policy (a fingerprinted static asset served to a logged-in user) keeps it.

`SESSION_COOKIE` is mirrored as a literal rather than imported: `umbral-security` sits below `umbral-sessions` in the plugin graph and must not depend on it. `umbral-cache` mirrors the same literal for the same reason.

**Also fixed:** `plugins/umbral-admin/templates/login.html` claimed the token was "per-session". It isn't — it's per-browser, and only binds to the session id when `SecurityConfig.session_bind_cookie` is set (default `None`). The comment now says what the code does.

**Tests:** `plugins/umbral-security/tests/private_cache.rs` — six cases through the real `Plugin::wrap_router`: session cookie ⇒ `no-store, private`; `Authorization` ⇒ `no-store`; anonymous ⇒ no header (public pages stay cacheable); handler-set `Cache-Control` wins; `private_cache = false` disables it; a lookalike cookie (`not_umbral_session=`) is not a session. `umbral-security` / `umbral-admin` / `umbral-cache` / `umbral-sessions` suites all green.

**Residual, deliberately not fixed:** `cache_page` is not CSRF-token-aware. An *anonymous* page that already has the CSRF cookie (so emits no `Set-Cookie`) can be cached with its `{{ csrf_token }}` embedded and replayed to other anonymous visitors. Low severity: the replayed token won't match the next visitor's own cookie, so their write 403s — an availability papercut, not a forgery vector, and the double-submit design is what makes it so.

---

41. [x] **`on_ready` fires for every CLI subcommand, so seeds run against an unmigrated schema during `migrate`.**

**Symptom.** `cargo run -- migrate` against a fresh database emitted a wall of `seed failed: relation "..." does not exist` (`community_social_link`, `features_feature_category`, `plugin_directory_plugin`, `reviews_review`, `site_content_blog_post`, `showcase_showcase_entry`, …) before the migration engine had created a single table. Observed on the first umbralrs.dev deploy, where it also buried the real failure (#40).

**Root cause.** `App::build()` fired every plugin's `on_ready` as its last phase, and the generated `main.rs` is `let app = App::builder()…build()?; umbral_cli::dispatch(app).await`. The hooks therefore ran before `dispatch` had parsed argv — including when argv said `migrate`. Non-fatal only because the website's seeds log-and-swallow; a plugin that propagated the error made `migrate` unrunnable on a fresh DB, and one that performed a write silently skipped it. This is not only a consumer problem: **`umbral-permissions` writes rows in `on_ready`** (`ensure_standard_permissions`) and hand-rolls `CREATE TABLE IF NOT EXISTS` for its six tables, with a comment admitting it does so "because `on_ready` fires after `App::build` which does not run `migrate`". The framework had worked around its own lifecycle bug.

**Shipped — the entry's second proposal, the lifecycle split.** `AppBuilder::build_deferred()` runs phases 1-6 (pools, registry, router, system checks) and fires nothing. `App::ready()` fires the hooks in topological order and is **idempotent** (an `AtomicBool` swap), so several callers may race to be the one that starts the app. `AppBuilder::build()` is now exactly `build_deferred()? + ready()?`, so its semantics are **unchanged** — every existing test, and every embedder holding an `App`, sees no difference. `App::serve()` calls `ready()` first, so a hand-rolled `main` that serves directly still gets its hooks.

`umbral_cli::dispatch` resolves the subcommand from argv and calls `command_needs_ready()`:

- **Skipped** for the schema commands (`migrate`, `makemigrations`, `showmigrations`, `checkmigrations`, `squashmigrations`, `inspectdb`) and the offline utilities (`typegen`, `maskkeygen`, `dev`, `help`).
- **Deferred** for `serve` and the bare `umbral` that defaults to it — the hooks fire from inside `App::serve()`, i.e. *after* `auto_migrate_on_serve` has applied migrations. This is the "bare `cargo run` → auto-migrate → seed → serve" path the entry required keep working.
- **Fired** for everything else: `dumpdata`, `loaddata`, `importcsv`, and every plugin-contributed command (`createsuperuser`, `worker`, an app's own `seed_orm_data`), all of which run against a database expected to be migrated already.

**Why `dispatch` still takes an `App`, not an `AppBuilder`.** Handing it the builder would have been architecturally tidier ("dispatch owns argv, so dispatch owns the build"), and was implemented and then backed out: the scaffolded `main.rs` runs `auto_migrate()` and `seed::all()` *between* build and dispatch, and both need the published model registry. Taking the builder would have deleted the demo's makemigrations-on-boot. Keeping `dispatch(app: App)` also means **zero** breakage across 318 build sites; the migration is one word in `main.rs`.

**The one wart, made loud.** A binary that still calls `build()?` before `dispatch` keeps the old behaviour (hooks already fired). Nothing can un-fire them, so `dispatch` detects it — `App::ready_already_fired()` — and prints an actionable warning naming the command and the one-word fix. The scaffold template (`scaffold.rs`) now emits `.build_deferred()?`, and `crates/umbral-cli/tests/scaffold.rs` pins that.

**Measured, not assumed.** The first attempt made `build()` itself defer, matching the entry's literal wording. `cargo test --workspace --no-fail-fast` reported **78 failures** across `umbral-storage`, `umbral-realtime`, `umbral-sessions`, `umbral-auth`, `umbral-admin` — every test that builds an app purely for its `on_ready` side effects and never takes the router. That is a *silent* behaviour change for any embedder doing the same, so the approach was abandoned rather than papered over with 78 test edits.

**Tests:** three separate binaries under `crates/umbral-cli/tests/` (one `App` build per process — `db::init` is a `OnceLock` that panics on the second): `on_ready_deferred.rs` (build_deferred defers; `ready()` fires once; a second `ready()` does not re-seed), `on_ready_skips_migrate.rs` (the reported bug), `on_ready_fires_for_live_command.rs` (`dumpdata` still fires them). Mutation-checked: forcing `command_needs_ready` to `true` makes the migrate test fail with `left: 1, right: 0`. Whole workspace green (2586).

**Follow-up, not done here:** `umbral-permissions`'s `CREATE TABLE IF NOT EXISTS` workaround can now be reconsidered — its `on_ready` no longer races `migrate` — but removing it would change what its tests rely on. Left in place deliberately.

---

42. [x] **Do we have proper handling of datetime fields, and does timezone work when a dev declares `DateTime<Utc>`? If a non-UTC datetime is passed in, does it convert?**

**Answer: yes, and there was one real bug underneath the question.**

Storage is UTC everywhere (`TIMESTAMPTZ` on Postgres, ISO-8601 text on SQLite); a timezone exists only at the marshalling boundary. `json_to_sea_value` (`crates/umbral-core/src/orm/write.rs`) is the single JSON→SQL coercion, and the dynamic write path (`DynQuerySet::insert_json`) is what both the admin form-submit and umbral-rest's create handler go through.

**Offset-bearing input already worked.** `2026-07-10T12:00:00+03:00`, `...T04:00:00-05:00`, `...T09:00:00Z` and `...+00:00` all normalise to the same UTC instant, and the project timezone never re-interprets them. Because normalisation happens on write, `filter(AT.gt(t))` and `order_by(AT.asc())` compare *instants*, not the text a value arrived as. Now pinned by `crates/umbral-core/tests/datetime_utc_offsets.rs` (four spellings of one instant; a filter that a lexicographic text compare would get backwards; garbage rejected rather than defaulted).

**Naive input already worked for the ordinary case.** `2026-07-10T12:00:00` with `time_zone = "America/New_York"` stores `16:00Z` (EDT), and the same wall-clock in January stores `17:00Z` (EST) — the offset comes from the zone's rules on that date. `<input type="datetime-local">`'s `YYYY-MM-DDTHH:MM` shape is accepted too.

**The bug: DST transitions were silently corrupted.** `timezone::naive_local_to_utc` returns `None` for an ambiguous local time (clocks back — `2026-11-01T01:30` in New York is *both* `05:30Z` and `06:30Z`) and for a nonexistent one (clocks forward — `2026-03-08T02:30` never occurs). Its own doc said the caller "should surface a validation error rather than silently pick one of the two possible UTC instants." The caller instead did `.unwrap_or_else(|| naive.and_utc())`, storing `01:30Z` — **not one of the two candidates but a third instant, four hours from either**. Textbook `unwrap_or_default()` on a value that is never legitimately absent.

**Shipped.** New `timezone::naive_local_to_utc_checked() -> Result<DateTime<Utc>, LocalTimeError>` preserves *why* there is no single instant (`Ambiguous { earlier, later }` / `Nonexistent`); `naive_local_to_utc` is now `.ok()` of it, so existing callers are unchanged. `json_to_sea_value` maps the two cases to new `WriteError::AmbiguousLocalTime` / `WriteError::NonexistentLocalTime`, carrying the field, the offending value, the tz name and (for ambiguous) both candidate instants. They land in `field_errors()` as an inline admin form error and, because `is_validation()` is a negative match (`!matches!(Sqlx | SerializeFailed | NotAnObject)`), as a REST `400` rather than a `500` — with the machine-readable codes `ambiguous_local_time` / `nonexistent_local_time` so a client can prompt for an explicit offset. Nothing is written on rejection.

**Tests:** two binaries, because `Settings` is published through a process-global `OnceLock` and each needs a different `time_zone`. `datetime_utc_offsets.rs` (tz = UTC): offset normalisation, naive-is-UTC, instant-ordered filter/order_by, garbage rejected. `datetime_project_timezone.rs` (tz = `America/New_York`): naive interpreted per the zone's summer/winter rules, an explicit offset beating the project tz, and the two DST cases rejected with a message naming the field and both candidates. The DST tests were written first and observed failing (the values were silently accepted) before the fix. Whole workspace green (2594).

**Not changed:** the typed path (`Post::objects().create(post)`) binds a `DateTime<Utc>` that is already an instant, so no ambiguity can arise. `Date` and `Time` columns carry no zone. `auto_now` / `auto_now_add` stamp `Utc::now()` (`now_for_column`) and never consult the project timezone.

**Doc:** `documentation/docs/v0.0.1/orm/datetimes-and-timezones.mdx`, including a warning that rows written on a DST-overlap hour by an older version are wrong by the zone's offset and no migration can recover the intent.

---

43. [x] **Do we have proper field data emission — does help text translate to a comment on the field, for something like Postgres or MySQL?**

**Answer: it didn't. Now it does, on Postgres.**

`#[umbral(help = "...")]` already reached the OpenAPI `description`, the admin form hint, and (since #38) the TSDoc on the generated TypeScript. The one audience that never saw it was the person at a `psql` prompt — which is exactly where people go to ask "what is this column for?". `FieldSpec::help`'s own doc comment called it presentational, and `diff_columns` deliberately excluded it as "no DB effect".

**Shipped.** `help` now renders as `COMMENT ON COLUMN "<table>"."<column>" IS '<text>'` on Postgres:

- `CreateTable` appends one comment per documented column, *after* the `CREATE` (Postgres has no inline column-comment syntax, and commenting a column that doesn't exist yet is an error). `AddColumn` appends its own.
- Editing help text on an existing column emits the new `Operation::SetColumnComment { table, column, comment }`. It is a **separate op, not an `AlterColumn`** — an alter is a full table-recreation dance on SQLite and a column rewrite on Postgres, and a docstring edit deserves neither. Emitted after any `AlterColumn` on the same column, since that op replaces the column.
- Clearing the help text emits `IS NULL`, not `IS ''`. Postgres distinguishes "no comment" from "the empty comment", and `\d+` prints the latter as a blank line.
- `classify_operation` returns `OpSafety::Safe`, so a docstring edit can never gate a zero-downtime deploy. `checkmigrations` labels it `COMMENT COL`; a comment-only migration files as `NNNN_comment_<table>_<column>.json` rather than `NNNN_auto.json`.

**Escaping.** `comment_on_column_stmt` doubles single quotes. Help text is prose, prose has apostrophes, and `help = "the note's body"` would otherwise close the SQL string literal. The value originates in a compile-time attribute rather than user input, but migration files are hand-editable, and an unescaped quote would surface as a syntax error at apply time instead of a diagnostic at generate time.

**SQLite renders zero statements.** It has no `COMMENT` facility of any kind. This is *not* the silent-divergence the raw-SQL rule warns about: columns, types, constraints and rows are identical on both backends; only the annotation is absent, because SQLite has nowhere to put it.

**MySQL** — the entry's other example — is not a backend umbral ships. `render_operation_for` panics on any name other than `sqlite` / `postgres`. If MySQL ever lands, its inline `COMMENT '...'` column clause is a different shape from Postgres's separate statement, and `Operation::SetColumnComment` is where that would be handled.

**Verified against a real Postgres**, not just asserted as a string: the rendered DDL was executed in a scratch schema on the local instance, and `col_description()` read back `Don't 'quote' me` and `The note's contents, as Markdown.` intact, with `IS NULL` genuinely clearing the comment. Scratch schema dropped afterwards.

**Tests:** `crates/umbral-core/tests/column_comments.rs` — 10 cases over the real `render_operation_for` and the real `diff`: one comment per documented column and none for undocumented ones; comment ordered after the CREATE; quote escaping; SQLite emits nothing but still creates the table; `AddColumn` carries its comment; a help edit emits exactly one `SetColumnComment` and never an `AlterColumn`; removal emits `IS NULL`; unchanged help emits no migration at all (without which every `makemigrations` after the upgrade would re-emit comments forever); and the op classifies SAFE. Whole workspace green (2604).

**Note for existing projects.** Snapshots written before this change carry no `help`, so the first `makemigrations` after upgrading emits one `SetColumnComment` per documented column. That is correct and cheap — the comments genuinely aren't in the database yet.

---

45. [x] **Can we have read/write permissions enforced at query time — if a user is in group X, they can read but not write? "Framework-enforced permissions." Controversial, since the ORM does not / should not rely on a plugin.**

**Answer: yes, and the entry's instinct was right — the ORM is the wrong layer. The right one already existed and was unusable. It works now.**

## Why not the ORM

Three findings, each independently fatal to a `QuerySet` guard:

1. **It would recurse.** `umbral_permissions::has_perm()` answers "may this user write `post`?" by running `UserPermission::objects()…exists()` and `Group::permissions_contains_any(…)` — *ORM queries*. A guard on the ORM terminals that consulted `has_perm` would re-enter itself on every check. Escaping that needs a "system" bypass scope the permissions plugin must remember to enter, which is a footgun sitting directly on the security boundary.
2. **A denial has no honest home.** `fetch`, `first`, `count`, `exists`, `delete` and `update_values` all return `Result<_, sqlx::Error>`; `create` returns `WriteError`; `get` returns `GetError`; `DynQuerySet` returns `DynError`. There is no umbrella ORM error. A `Forbidden` would have to be smuggled through `sqlx::Error` (a 500, not a 403, and indistinguishable from a real DB fault) or the read terminals' signatures would have to change — a breaking refactor across every consumer. Enforcing only the terminals whose error type happens to be rich enough (`create`, `DynQuerySet`) is worse than enforcing nothing: `delete()` would stay wide open behind a guard that looks total.
3. **It needs a second process-wide global.** An ambient "current user" the ORM reads. `arch.md` allows exactly one intentional global (the `DbPool` `OnceLock`) and says not to let others creep in.

Django reaches the same conclusion: its ORM enforces no permissions; views, admin and DRF do. The `Identity` contract already lives in `umbral-core` (`auth_contract.rs`) so plugins depend inward, and it is deliberately not consulted by any query.

## The layer that *can* do it, and the hole in it

Postgres row-level security. `umbral-rls` already declared policies whose `USING` / `WITH CHECK` expressions read `current_setting('app.user_id')`; `RouteContext::session_vars` already existed; and the Postgres pool's `before_acquire` hook already ran `RESET ALL` followed by `set_config(name, value, false)` per entry (audit_2 C2/R2), so a value cannot leak to the next request on a pooled connection.

**Nothing ever populated the list.** `RouteContext::add_session_var` — whose doc comment reads "used by middleware that augments an already-scoped context" — had **zero callers**, and no such middleware existed. The only other hook, `AppBuilder::route_context`, takes a **synchronous** resolver (`Fn(&Request) -> RouteContext`), while finding the session user requires an async DB read. The wiring the `umbral-rls` docs called "REQUIRED" was therefore not expressible, and the doc's example called a `current_session_user_id(req.headers())` that does not exist.

This was not a soft failure. Verified against a live Postgres: a `SELECT` on an RLS-enabled table whose policy reads an unset `app.user_id` fails with `ERROR: unrecognized configuration parameter "app.user_id"`. Every request 500s.

## Shipped

`AuthPlugin::with_db_session_var("app.user_id")` mounts `db_session_var_layer::<U>`, which resolves the user with `resolve_user::<U>` (session-derived, never a client header, and filtered on `is_active` so a deactivated account carries no identity), clones the ambient `RouteContext`, calls the until-now-dead `add_session_var`, and re-scopes for the rest of the request. Augment, not replace: an outer resolver's tenant and variables survive.

The variable is set on **every** request, to the empty string when anonymous — precisely because an unset GUC is a 500 rather than an empty result. Policies read `NULLIF(current_setting('app.user_id'), '')`. Opt-in, because it costs one session + one user read per request and, unlike `with_user_in_templates`, cannot be lazy: the value must reach the connection before the handler's first query.

`impl Plugin for AuthPlugin<U>` gained the bounds `resolve_user::<U>` needs (`FromRow` on both backends, `HydrateRelated`, `PrimaryKey: FromStr`). Any user model that could not satisfy them was already unusable with the `LoggedIn<U>` extractor; the whole workspace compiles unchanged.

## The recipe, proven on a live Postgres

```sql
CREATE POLICY post_read  ON post FOR SELECT USING (NULLIF(current_setting('app.user_id'), '') IS NOT NULL);
CREATE POLICY post_write ON post FOR INSERT WITH CHECK (
  EXISTS (SELECT 1 FROM permissions_usergroup ug JOIN permissions_group g ON g.id = ug.group_id
          WHERE g.name = 'editors' AND ug.user_id = NULLIF(current_setting('app.user_id'), '')));
```

Executed against the real `permissions_group` / `permissions_usergroup` shapes in a scratch schema: **anonymous** → 0 rows, no error; **a member of `viewers`** → reads 2 rows, and `INSERT` fails with `new row violates row-level security policy`; **a member of `editors`** → reads and writes. Exactly "group X can read but not write", enforced by the database, with the ORM untouched and no plugin dependency added to `umbral-core`. Scratch schema dropped afterwards.

## Docs corrected

Two claims on `documentation/docs/v0.0.1/plugins/rls.mdx` were false and dangerous:

- A `danger` callout said the plugin "does not emit `FORCE ROW LEVEL SECURITY`" and that every policy is therefore silently bypassed. It **does** emit it (audit_2 C2/R1). Rewritten to warn about `BYPASSRLS`/superuser, which `FORCE` genuinely does not cover.
- A second `danger` callout prescribed a hand-rolled `set_config` middleware, called it unsound, and told readers umbral "does not expose per-request connection pinning". The `before_acquire` hook makes pinning unnecessary. Replaced with the one-line builder wiring.

Every `current_setting('app.user_id')` example on that page and in the plugin's rustdoc now uses the `NULLIF` form.

**Tests:** `plugins/umbral-auth/tests/db_session_var.rs` — a logged-in request publishes its id; an anonymous request and an anonymous *session row* both publish the empty string; a deactivated user publishes nothing; without the builder no variable is set at all; and the ambient tenant + an outer resolver's variables survive the layer. Whole workspace green (2609).
