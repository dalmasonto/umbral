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
