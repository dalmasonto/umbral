# umbral — Full System Production Audit (Pass 2)

**Date:** 2026-07-02 → 2026-07-03
**Method:** 16 parallel component auditors (Opus 4.8 on security-critical scopes, Fable 5 on mechanical scopes) over `crates/` + all 21 `plugins/`. Every finding cites `file:line` from code the auditor actually read. Per-component reports live in `planning/audit_2/findings/<slug>.md`; this file is the cross-system synthesis and re-ranking.

**Scale assumption:** ~10M active users, sensitive PII via `Masked<T>`, Postgres primary / SQLite tests, axum runtime, multi-tenant.

---

## A. Executive summary

**Overall risk posture: NOT production-ready as a default assembly.** The framework is *capable* of a secure deployment, but almost every protection is opt-in and off by default, and three isolation/encryption guarantees that the docs present as working are effectively non-functional. A team that wires every security plugin correctly, declares `Environment::Prod`, and hand-scopes every REST/CRUD queryset can run safely; a team that follows the "batteries-included" promise ships an app that is clickjackable, has no security headers, leaks its entire API surface, stores "encrypted" PII in plaintext, and — if multi-tenant — lets any logged-in user read any other tenant's data.

**The 3 most urgent issues:**
1. **`Masked<T>` PII is written in plaintext** on the dynamic `insert_json`/`update_json` path — which is exactly what REST create/update and admin form-submit use. Encrypt-at-rest is silently bypassed on the primary write path (`core-orm` C1).
2. **Multi-tenant isolation is not enforced.** The tenant is selected from a client-controlled `X-Tenant` header with no user↔tenant binding (`plugin-oauth-tenants` TEN-1), and the RLS backstop that should contain this is non-functional: it uses `ENABLE` (not `FORCE`) RLS while the app connects as the table owner (owner-exempt), and nothing anywhere sets the `app.user_id` GUC the policies depend on (`plugin-authz` R1/R2). Net: cross-tenant read/write with one header.
3. **Secure-by-default is inverted.** `Environment` defaults to `Dev`; a prod deploy behind a loopback reverse proxy that forgets `UMBRAL_ENVIRONMENT=prod` runs with the *publicly known* default `secret_key` accepted, no Host validation, dev error pages, no security headers, playground/openapi/livereload exposed, and forgeable sessions/CSRF (`core-app-config` #1/#2, `core-web` H1, `plugin-observability` #1/#6).

**Cross-cutting weakness classes** (each seen in ≥3 components): (a) secure-by-default inversion; (b) mass-assignment "deny nothing" defaults; (c) context-blind autoescaping → XSS; (d) trust of client-forgeable headers (`X-Tenant`, `X-Forwarded-For`, `X-Umbral-User-Id`); (e) secrets reachable via `Debug`/logs/signal payloads; (f) unbounded work as a DoS primitive.

**What could NOT be assessed:** live Postgres RLS enforcement (the only PG RLS test is `#[ignore]`d and asserts policy *existence*, not enforcement); `cargo audit`/RUSTSEC results (`cargo-audit` is not installed, so all CVE-class dependency claims are unverified); runtime infra (TLS termination, WAF, network policy, reverse-proxy config); actual deployment templates / env wiring; and whether shipped models mark privileged fields (`is_superuser`) as non-writable.

---

## B. Consolidated findings — CRITICAL & HIGH

Severity is re-ranked at the system level; per-component files hold the full MEDIUM/LOW rows. `†` = auditor rated lower; elevated here with rationale in §C.

| # | Sev | Area | Component · Location | Finding | Impact |
|---|-----|------|----------------------|---------|--------|
| C1 | CRITICAL ✅FIXED | Crypto-at-rest | core-orm · `dynamic.rs` insert_json/update_json; sinks `umbral-rest/src/lib.rs:2662,2714` | `Masked<T>` was sealed only in serde `Serialize`/sqlx `Encode`, never on the raw-JSON bind path used by REST + admin writes | PII the operator believes is encrypted at rest was stored plaintext on the primary API write path. **FIXED:** `seal_masked_json` now seals masked columns on the dynamic JSON + form write paths (fails closed w/o keyring); derive forces the `masked` widget so the signal can't be overridden. Test: `masked_roundtrip::dynamic_insert_json_seals_masked_column`. |
| C2 | CRITICAL ✅FIXED | Tenant isolation | plugin-authz · `umbral-rls` (`ENABLE` not `FORCE`) + no `app.user_id` GUC set anywhere | RLS is owner-exempt and its policy predicate variable is never set → **no row isolation as shipped**, yet `pg_policies` shows it configured | The tenant/user data-isolation backstop enforces nothing on Postgres. **FIXED (17c5d468/aeeb761b):** `apply_policies` now runs `ENABLE` **and** `FORCE ROW LEVEL SECURITY` per table (owner no longer exempt); the policy GUC is set per-request via a PG pool `before_acquire` hook that runs `RESET ALL` + `set_config` from `RouteContext::session_vars` (no cross-request leak); RLS on SQLite fails the boot closed under `Environment::Prod`. Live 2-tenant PG enforcement test still `#[ignore]`d (blind spot #1). |
| C3† | CRITICAL ✅FIXED | Tenant isolation / IDOR | plugin-oauth-tenants · `umbral-tenants/src/lib.rs` | Tenant chosen from client `X-Tenant` header (was on by default); no user↔tenant binding | Any authenticated user reads/writes any other tenant's data with one header. **FIXED:** `X-Tenant` OFF by default (opt-in), db-per-tenant routing fails closed on unknown tenant (TEN-2/3, prior pass), AND the missing user↔tenant binding now exists: a pluggable `TenantMembership` guard (`TenantsPlugin::membership(...)`) is checked server-side after the tenant resolves — a non-member request fails closed with the SAME 404 as an unknown tenant (no enumeration oracle). The guard extracts the caller's identity itself, keeping tenants decoupled from any auth plugin. Added `current_tenant()` so handlers read the *bound* tenant, not client input. Tests: `member_proceeds_non_member_rejected_404`, `no_guard_proceeds`, `current_tenant_reads_the_scoped_context`. Doc: `plugins/tenancy.mdx`. |
| H1 | HIGH ⏸DEFERRED | Authz / IDOR | plugin-rest · `lib.rs`; `permission.rs` | Built-in `retrieve`/`update`/`destroy` scope lookups to the pk only; no `get_queryset`/object-permission hook | Any caller past the table gate reads/mutates any row by id. **DEFERRED (needs design):** the fix is a framework primitive — a per-resource queryset-scoping / object-permission hook on the CRUD path. Doc now states plainly that built-in CRUD cannot be object-scoped (use `.views()` + custom `.action()`). |
| H2 | HIGH ✅FIXED | Mass assignment / authz | plugin-rest · `insert_nested_tree`/`upsert_nested_child` | Nested child bodies skipped `strip_hidden_for_write` AND the child's own permission class | A parent-writer could set hidden fields (`is_superuser`, `password_hash`) on nested children and write child tables they can't write directly. **FIXED:** each nested child now runs `strip_hidden_for_write` + `permission_for(child).check(Create/Update)`; tests `nested_write_guard.rs` + `nested_child_permission.rs`. |
| H3 | HIGH ✅FIXED | DoS / mass assignment | plugin-rest nested arrays (fixed); core-orm `insert_json`/`update_json` (fixed) | (rest) nested arrays had no total-node cap; (orm) deny-nothing-by-default write path | **rest FIXED:** whole-tree cap `MAX_NEST_NODES=1000` → 400 before commit. **orm FIXED:** new `#[umbral(privileged)]` field flag — the dynamic JSON **and** admin-form write paths default-DENY privileged columns (`is_superuser`/`is_staff`/ownership FKs); a caller re-authorizes per-write with `DynQuerySet::allow_privileged`. Built-in `AuthUser.is_staff`/`is_superuser` now `privileged` (+ `default = "false"`); admin lets only a superuser toggle them. Tests: `privileged_field.rs` (6, JSON + form, deny + authorize). Doc: `orm/privileged-fields.mdx`. |
| H4 | HIGH ✅FIXED | XSS (default surface) | core-templates-forms · `templates/defaults/default_404.html` | Request path interpolated into an inline `onclick` JS-string context; HTML autoescape doesn't cover JS context; renders in prod by default | Link-click reflected XSS on every 404 of a default-configured app. **FIXED:** the copy button reads the inert `textContent` of the path element instead of interpolating the path into JS; same fix on the 500 page + form-field attribute escaping + form re-render error leak. Test: `default_404_copy_button_does_not_interpolate_path_into_js`. |
| H5 | HIGH ✅FIXED | XSS (admin) | plugin-admin · filter-dialog + editable cells/filenames | Model/user data interpolated into `on*` handler JS strings; `?search/?sort/?order` reflected, filenames/cells stored | Reflected + stored XSS in the authenticated admin origin. **FIXED:** new `escapejs` filter (\uXXXX-escapes JS/attr-dangerous chars) applied to every reflected + stored inline-handler sink; hand-built `inline_edit` sink double-encoded. Test: `authz_web7`/`escapejs_filter_*`. |
| H6 | HIGH ✅FIXED | Authz / info leak | plugin-admin · `palette_search` / `filter_dialog` / `history` | Returned rows across **every** model with only `require_staff` — no per-model `view_<model>` check | Cross-permission label/PK disclosure. **FIXED:** added the per-model `view_<model>` gate (mirrors WEB-7) to palette_search (filter), filter_dialog, and history. Test: `authz_web7_read_endpoints.rs`. |
| H7 | HIGH ✅FIXED | Session revocation | plugin-sessions · `active_store()` used only in `session_layer` | `revoke_user_sessions`/logout/read hit the SQL table directly → no-ops under `RedisStore`/`CookieStore` | Password reset does not invalidate a stolen Redis/Cookie session. **FIXED (`SessionStore::destroy_user`):** `revoke_user_sessions` / `destroy_session` / `read_session` now route through `active_store()`. New `destroy_user(user_id)` deletes from wherever sessions live — `DbStore` (DELETE by user_id), `RedisStore` (new `umbral:user-sessions:<uid>` index maintained on save → SMEMBERS+DEL on revoke). A stateless `CookieStore` can't enumerate, so the trait default returns `SessionError::RevocationUnsupported` and the password-reset caller (`challenge.rs:470`) logs it LOUDLY instead of silently no-op'ing. Added `#[umbral(index)]` on `session.user_id` (scan→index at scale, finding #4). CookieStore empty-key boot-fail was already a hard Prod fail in the sessions plugin (H8). Tests: `cookie_store_destroy_user_is_unsupported` + the existing `revoke_user_sessions` reroute. **Deferred (LOW):** `set_data`'s out-of-request raw-SQL fallback (finding #6). |
| H8 | HIGH ✅FIXED | Secrets / auth bypass | plugin-sessions · `CookieStore` | Empty/insecure-dev `secret_key` only logged + booted | Forgeable/tamperable sessions if `umbral-security` isn't mounted. **FIXED:** new `SessionStore::requires_ambient_secret()`; `SessionsPlugin::on_ready` hard-fails `App::build()` in Prod when the active store needs the ambient secret and it's empty or the dev default. Test: `cookie_store_boot_check.rs`. |
| H9 | HIGH ⏸DEFERRED | Brute force | plugin-auth · `auth_routes.rs` | All throttle buckets keyed on client-forgeable leftmost `X-Forwarded-For`, no trusted-proxy validation | Rotate the header per request → bypass all rate limiting. **DEFERRED (needs design):** a correct fix needs a framework-level trusted-proxy allowlist config (how many `X-Forwarded-For` hops to trust) touching core settings — not a contained plugin change. (Auth also fixed the enumeration-timing oracle, error leaks, and mailer secret printing — see plugin-auth.md.) |
| H10 | HIGH ✅FIXED | Security headers | core-web · `app.rs` wiring + `check.rs:655` | No X-Frame-Options/HSTS/nosniff/CSP by default; they live only in optional `SecurityPlugin`. **FIXED (4c016cf7):** core now ships `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin` by default, set-if-absent so a handler / `SecurityPlugin` overrides without duplication; toggle via `App::builder().default_security_headers(false)`. HSTS/CSP stay opt-in (context-dependent). |
| H11 | HIGH ✅FIXED | DoS / memory | core-web · `multipart.rs:100`, no `RequestBodyLimitLayer`/`TimeoutLayer` in `App::build` | No request body-size limit; `parse_multipart` buffers whole bodies; `TooLarge` is dead code; no default timeout | Memory-exhaustion + slowloris DoS. **FIXED:** `App::build` installs a default 32 MiB `RequestBodyLimitLayer` (413) + 30s `TimeoutLayer` (408), both opt-out-able; `parse_multipart` now enforces a cap (wires the dead `TooLarge`). Test: `request_limits.rs`. |
| H12 | HIGH ✅FIXED | Prod exposure | plugin-observability · `umbral-openapi/src/lib.rs` | `OpenApiPlugin::routes()` mounted Swagger UI + full JSON spec unconditionally (no `Environment` gate) | Entire API surface served unauthenticated in prod — recon goldmine. **FIXED:** gated off in `Environment::Prod` (mirrors playground), opt in via `OpenApiPlugin::new().allow_in_prod()`. Test: `openapi::prod_gating`. Playground + livereload were already correctly gated. |
| H13 | HIGH ✅FIXED | DoS / memory | plugin-observability · `umbral-logs/src/lib.rs` | Every logged request pushed a `JoinHandle` into a global `PENDING` Vec drained only by test-only `flush()` | Unbounded memory growth → OOM at 10M-request volume. **FIXED:** `track_handle` reaps finished handles (`is_finished`) on each insert, bounding the list to in-flight concurrency. Test: `logs::track_handle_reaps_finished_tasks_and_stays_bounded`. |
| H14 | HIGH ✅FIXED | Config / secure-default | core-app-config · `settings.rs:419` + `check.rs:199` | Prod protections all key off explicit `Environment::Prod`; default `Dev`; loopback heuristic exempts reverse-proxy topology | Proxy-fronted deploy that omits the env var runs with known dev SECRET_KEY, no Host validation, dev error pages — zero warnings. **FIXED:** `Environment::default()` is now profile-aware — **debug builds → `Dev`, release builds → `Prod`** (an explicit `UMBRAL_ENVIRONMENT` always wins). So a release binary that omits the var boots locked down (secret-key/Host/error-page protections on). Also fixed the `is_loopback_bind` IPv6 `::1` misparse (findings #16). The secret-key entropy floor, `allowed_hosts` wildcard, and SQLite-in-prod checks were already in the catalogue. Tests: `environment_default_is_profile_aware` (correct in both profiles) + IPv6 loopback cases. Doc: `getting-started/settings-and-env.mdx`. **Deferred:** a pending-migrations-at-boot check needs the sync/no-DB `check.rs` framework to gain async + pool access — logged as a follow-up. |
| H15 | HIGH ✅FIXED | Config / secrets | core-app-config · `check.rs` | No entropy/length floor on `secret_key` (`"x"` booted in Prod); default key only rejected when Prod explicit | Forgeable session/CSRF/token signatures. **FIXED:** the Prod boot check now hard-errors on a `secret_key` shorter than 32 chars, not just the exact dev default. Test: `check::prod_rejects_weak_and_default_secret_keys`. (The Dev-default-detection / env-default hardening in app-config #1 is still open.) |
| H16 | HIGH ✅FIXED | DB pool | core-app-config · `db.rs:382-402`, `app.rs:824` | `UMBRAL_DB_*` pool knobs ignored for the default pool (settings published in `build()`, pool opened before `build()`) | Operator sets `MAX_CONNECTIONS=100`; pool still opens with 10 → saturation + 30s stalls while config appears correct. **FIXED (67d863bd):** `PoolConfig::resolve()` re-reads the knobs from the env (same figment parse) when settings aren't published yet, so the pre-`build()` default pool honors `UMBRAL_DB_*`. |
| H17 | HIGH ◐PARTIAL | Routing panic | core-app-config · `settings.rs:273`, `db.rs:239` | `Settings.databases` is documented + deserialized but never consumed to open pools | A model routed to a `settings.databases`-only alias panics `no database registered` on first query → request-path 500. **PARTIAL (67d863bd):** a model routed to such an alias already trips `PluginDatabaseAlias` at boot; `build()` now also warns loudly for every `settings.databases` alias with no registered pool, so the dead config is visible. **Auto-opening pools from `settings.databases` still DEFERRED** (needs async pool-open in `build()`). |
| H18 | HIGH | RLS divergence | plugin-authz · SQLite RLS path | SQLite silently skips all RLS | Isolation tests on the stated test backend pass vacuously; dev/test behavior diverges from prod |
| H19 | HIGH ✅FIXED | Authz default | plugin-authz · `umbral-permissions` | Permissions are default-**allow**: a route is protected only if the dev adds `permission_required`; no default-deny, no boot audit | One forgotten annotation = open endpoint. **FIXED:** (P3, `860eeb18`) the perm layer denies DEACTIVATED accounts before the superuser bypass + perm check (pure `pre_perm_check` + tests). (boot audit) `App::build()` now logs a Warning listing the app's own mutating routes (POST/PUT/PATCH/DELETE) with no *recorded* permission. (gated builders) new `permission: Option<String>` on `RouteSpec` + core `Routes::route_gated` + the `umbral-permissions` `RoutesPermExt` (`post_gated`/`delete_gated`/… ) apply the `permission_required` layer AND record the permission so the audit sees the gate — resolving the tower-layer-opacity + core/plugin dependency-inversion constraints. Warning-everywhere (not a Prod hard error) + opt-in builders per the approved decision; default-deny-by-construction deferred to a future major. Tests: `audit_tests` (selection) + `routes_ext` (recording). Doc: `plugins/permissions.mdx`. |
| H20 | HIGH ✅FIXED | Migrations / DDL | core-migrate · `migrate.rs:4078` | FK drop+re-add hardcodes `REFERENCES target("id")` instead of resolving the real PK column | Altering an FK to a String/Uuid-PK target (e.g. `Permission.codename`) aborts mid-deploy, or silently attaches to the wrong column (referential corruption) |
| H21 | HIGH ✅FIXED | Migrations / SQLite | core-migrate · `migrate.rs:3305,3902` | Combined alter + add/drop on one table emits ops whose `INSERT…SELECT` references not-yet/already-gone columns | A routine model edit produces a migration that can't apply on SQLite; deploy blocked / file must be hand-edited. **FIXED:** the `AlterColumn` op now carries `new_columns` shaped like the OLD table (previous column set, current defs applied to survivors, to-be-dropped columns kept) instead of `current.fields` — the recreation `INSERT…SELECT` only references columns that exist, and the following `DropColumn`/`AddColumn` ops (native SQLite ALTERs) finish the change. Test `combined_alter_add_drop_sqlite.rs` applies a real alter+add+drop to a populated table and reads rows back; verified red without the fix (`no such column: old_flag`). |
| H22 | HIGH ✅FIXED | Backup / recovery | core-migrate · `backup.rs:159-213,342` | `load` inserts in alphabetical (not FK-topological) order, no transaction, no PG sequence reset | Restore fails when a child table sorts before its parent; partial restore on mid-load failure; "successful" PG restore then throws duplicate-PK on first insert — recovery unreliable when needed |
| H23 | HIGH ✅FIXED | Migrations / data loss | core-migrate · `migrate.rs:2839,3345` | Rename heuristic auto-pairs an unrelated dropped+created model with matching column shapes, warning only via `eprintln!` | Two lookup models silently become a `RenameTable`: the new one inherits the old rows, the intended drop never happens — silent data mis-association in prod. **FIXED:** the column-shape rename heuristic no longer guesses — `diff` fails closed with `MigrateError::AmbiguousRename` unless the operator sets `UMBRAL_MIGRATIONS_ASSUME_RENAMES=assume` (auto-pair → RenameTable) or `=independent` (drop + create, no row transfer). The struct-name rename (strong signal) still auto-applies. Tests: `rename_detection.rs` (fail-closed default) + `rename_intent_env.rs` (both modes). Doc: `migrations/renames.mdx`. |
| H24 | HIGH ✅FIXED | Supply chain | supply-chain · `rust-s3 0.35.1` (storage `s3` feature) | Unmaintained; drags EOL `rustls 0.21.12` + `hyper 0.14` onto the object-storage TLS path | Legacy, unpatched TLS on a network-facing path in the expected prod media config. **FIXED:** ran `cargo audit` (11 vulns → 3). Bumped `rust-s3` 0.35 → 0.37.2 — drops EOL `rustls 0.21` / `hyper 0.14` / `rustls-webpki 0.101` entirely (only modern `rustls 0.23` / `hyper 1` remain; s3 feature still builds). Also patched the default-build advisories the audit surfaced: `ammonia` 4.1.2 → 4.1.3 (mXSS RUSTSEC-2026-0193), `time` → 0.3.47, `quinn-proto` → 0.11.16, `anyhow` → 1.0.103. Residual 3 are tracked+justified in `.cargo/audit.toml` (`rsa` phantom / no-fix; `quick-xml` DoS blocked by `syntect→plist`'s `^0.38` pin, trusted-input-only) and CI now gates via `.github/workflows/audit.yml`. |
| H25 | HIGH ✅FIXED | Stored XSS | plugin-storage-tasks · `s3.rs` | `S3Storage::store` kept client `Content-Type` and skipped the `.html/.svg/.js→.txt` rename local storage applies | CDN/public bucket served uploaded `evil.html` inline. **FIXED:** shared `media::neutralised_upload` (sanitise + active-content rename + force `text/plain`) now applied on the S3 `store`/`put`/`store_stream` paths too. Also added a 25 MiB default upload cap (H-adjacent) + media symlink guard. Test: `active_content_tests::neutralised_upload_*`. |
| H26 | HIGH ✅FIXED | Cache poisoning | plugin-realtime-comms · `cache_page.rs` | Shared-cache decision keyed only on absence of the `umbral_session` cookie; ignored `Authorization`/`Cache-Control: private` | Token/`Authorization`-auth apps got per-user responses cached and served cross-user. **FIXED:** bypass on `Authorization` header + on `Cache-Control: private`/`no-cache` (added to `no-store`). Doc corrected. Tests: `cache_page::{authorization_header_makes_request_personalised, private_and_no_cache_responses_bypass_cache}`. Residual (custom cookie names / Vary) documented. |

*(Full MEDIUM/LOW inventory — ~60 MEDIUM, ~55 LOW — is in the per-component files. High-signal MEDIUMs worth pulling forward: no cross-process migration lock at multi-replica deploy (`core-migrate` #7); `Settings`/`AppContext` `Debug` leaks `secret_key`+DB password (`core-app-config` #11); signal payloads carry full rows incl. `Masked`/PII to every subscriber (`core-app-config` #10); `X-Umbral-User-Id` header trusted for log attribution → audit forgery (`plugin-observability` #7); analytics ships full request paths incl. reset tokens to a third party (`plugin-observability` #4); scaffold seeds `admin`/`admin` superuser (`core-macros-cli`); NOT-NULL tighten generates a migration that aborts on existing NULLs (`core-migrate` #5); destructive `DropTable` applied by `migrate` with no confirmation (`core-migrate` #6).)*

---

## C. Detail on the CRITICALs and the elevation

Full vulnerable/corrected snippets live in the per-component files; condensed here.

### C1 — `Masked<T>` plaintext on the dynamic write path (core-orm)
Sealing is implemented on `Serialize` and sqlx `Encode`, but REST create/update and admin form-submit go through `DynQuerySet::insert_json`/`update_json`, which bind the raw JSON value directly. `Masked` crypto itself is sound (ephemeral keypair + random nonce, authenticated, fails closed) — it's simply not invoked on the API write path. **Fix:** seal `Masked`-typed columns inside the JSON→bind conversion (`json_to_sea_value`) so every write path encrypts, not just the typed struct path. Until then, `orm/masked.mdx` has been corrected (was claiming "plaintext never leaves via serde").

### C2 — RLS enforces nothing (plugin-authz)
Two independent defects, either of which alone defeats RLS: (1) the plugin issues `ALTER TABLE … ENABLE ROW LEVEL SECURITY` but never `FORCE`, and umbral's single-`DATABASE_URL` app connects as the table **owner**, which Postgres exempts from non-forced RLS; (2) a repo-wide grep shows nothing ever sets the `app.user_id` GUC the policies reference. The only PG RLS test asserts policies *exist*, never that they *enforce* — which is how this shipped. **Fix:** `FORCE ROW LEVEL SECURITY`, connect as a non-owner role, and set/reset the GUC per request/transaction (needs a connection-scoped hook). `plugins/rls.mdx` now carries danger callouts.

### C3 — Tenant from client header, elevated to CRITICAL
Auditor rated HIGH; elevated because the impact is unauthenticated-of-boundary cross-tenant **read and write** at the stated 10M multi-tenant scale, the header is on by default and cannot be disabled, and the RLS backstop (C2) is itself non-functional — so there is no compensating control. `umbral-tenants` also fails **open**: an unknown tenant falls through to the default/`public` DB rather than failing closed (TEN-2/TEN-3). **Fix:** resolve tenant server-side and bind it to the authenticated session; fail closed on unknown tenant; do not honor a client tenant header without validating it against the user's membership.

*(The 26 HIGHs are documented with code + scenario + corrected snippet in their component files — e.g. H2's fix is to call `strip_hidden_for_write(child_table, …)` and enforce the child resource's `permission_for(...).check(...)` inside `insert_nested_tree`/`upsert_nested_child`.)*

---

## D. Blind spots (not verifiable from the artifacts)

1. **Live RLS/tenant enforcement** — the PG isolation tests are `#[ignore]`d; C2/C3 are reasoned from code + a policy-existence-only test, not observed enforcement.
2. **Dependency CVEs** — `cargo-audit`/`cargo-deny`/`cargo-outdated` are not installed; no RUSTSEC IDs confirmed. H24 and all supply-chain CVE-class claims are "unverified, needs `cargo audit`." (`rsa 0.9.10` appears in `Cargo.lock` but not the compiled graph — a phantom, not a live finding.)
3. **Runtime/infra** — TLS termination, reverse-proxy trust config (which makes or breaks H9/H14), WAF, network segmentation, secret injection, container/user hardening, and the actual deploy templates (whether `UMBRAL_ENVIRONMENT=prod` is enforced) are all outside the code.
4. **Cross-plugin wiring at boot** — whether `SecurityPlugin` (CSRF + headers) and a throttle are actually mounted in real apps; several HIGHs degrade to MEDIUM if it is, and vice-versa.
5. **Shipped model definitions** — whether `umbral-auth`'s `User` marks `is_superuser` as `noform` (would blunt H3); not in scope of the ORM auditor.
6. **`Masked` serialize form inside signal payloads** — whether the full-row signal fan-out (`core-app-config` #10) emits sealed or plaintext masked values was not verified.

---

## E. Prioritized action plan

### Quick wins (< 1 day each)
- Gate `OpenApiPlugin`, `PlaygroundPlugin`, `LiveReloadPlugin` behind an explicit prod opt-in; default off in `Prod` (H12, `plugin-observability` #6).
- Add `RequestBodyLimitLayer` + `TimeoutLayer` defaults in `App::build` (H11).
- Enforce a `secret_key` length/entropy floor and hard-fail on the known default regardless of environment (H15).
- Stop pushing `JoinHandle`s into `PENDING` in non-test builds (H13).
- Hand-implement `Debug` for `Settings` to redact `secret_key`/DB password/`extra` (`core-app-config` #11).
- Resolve log/throttle identity from the authenticated session, never `X-Umbral-User-Id`/`X-Forwarded-For` without a trusted-proxy allowlist (H9, `plugin-observability` #7).
- Fix the S3 `Content-Type`/active-content rename to match local storage (H25).
- Escape the JS-string context in the default 404 (and admin `on*` handlers) or drop the inline handler (H4, H5).

### Short term (< 2 weeks)
- Call `strip_hidden_for_write` + child `permission_for().check()` inside the nested-write recursion, and cap total nested nodes (H2, H3, `plugin-rest` H3) — **this touches the code just shipped**.
- Seal `Masked` in the JSON→bind path so REST/admin writes encrypt (C1).
- Ship default security headers (or make `SecurityPlugin` a hard requirement that boot-fails if absent) (H10).
- Make `UMBRAL_DB_*` actually apply to the default pool; consume or reject `Settings.databases` at boot (H16, H17).
- Wire session revocation through `active_store()` so logout/password-reset work on Redis/Cookie stores (H7); boot-fail on empty `CookieStore` secret (H8).
- Add a shared-cache key that honors `Authorization`/`Cache-Control: private` (H26).
- Fix the migration FK-PK-column resolution and the combined alter+add/drop SQLite ordering; wrap `backup load` in a transaction with FK-topological order + PG sequence reset (H20, H21, H22).

### Structural (needs design)
- **Secure-by-default reversal:** default `Environment` to `Prod` (or fail-closed when unset in a release binary); make authz default-**deny** with a boot audit of unprotected routes; ship a conservative default throttle (H14, H19, `plugin-rest` M-5).
- **Object-level authorization:** add a per-resource queryset-scoping / object-permission hook applied to every CRUD + admin lookup — the missing primitive behind H1, H6, and the IDOR class.
- **Tenant isolation redesign:** server-side tenant resolution bound to the session, fail-closed routing, and a *working* RLS story (`FORCE` + non-owner role + per-connection GUC) — C2/C3 together (`plugin-authz` R1–R3, `plugin-oauth-tenants` TEN-1/2/3).
- **Migration safety:** cross-process advisory lock for multi-replica deploys; explicit acknowledgment for destructive `DropTable`/rename at apply time; snapshot-hash drift detection (`core-migrate` #4/#6/#7/#9).
- **CI supply-chain gate:** add `cargo audit` + `cargo deny` to CI and re-run this audit's H24/supply-chain items against real RUSTSEC data; plan `rust-s3`/`rustls 0.21` replacement.
- **Signals contract:** stop fanning full rows (incl. PII/`Masked`) to every subscriber; add a post-commit/outbox seam if audit-grade delivery is a goal (`core-app-config` #9/#10).

---

## Appendix 1 — Per-component report index

| Report | Model | C | H | M | L |
|--------|-------|---|---|---|---|
| core-orm | Opus 4.8 | 1 | 3 | 3 | 3 |
| core-web | Opus 4.8 | 0 | 2 | 3 | 3 |
| core-migrate | Fable 5 | 0 | 4 | 9 | 5 |
| core-templates-forms | Fable 5 | 0 | 1 | 1 | 4 |
| core-app-config | Fable 5 | 0 | 4 | 9 | 5 |
| core-macros-cli | Opus 4.8 | 0 | 0 | 2 | 6 |
| supply-chain | Opus 4.8 | 0 | 1 | 2 | 5 |
| plugin-auth | Opus 4.8 | 0 | 1 | 5 | 4 |
| plugin-sessions | Opus 4.8 | 0 | 2 | 3 | 2 |
| plugin-authz | Opus 4.8 | 2 | 2 | 7 | 5 |
| plugin-rest | Opus 4.8 | 0 | 3 | 2 | 3 |
| plugin-admin | Opus 4.8 | 0 | 2 | 3 | 2 |
| plugin-oauth-tenants | Opus 4.8 | 0 | 2† | 4 | 3 |
| plugin-storage-tasks | Fable 5 | 0 | 1 | 5 | 6 |
| plugin-realtime-comms | Fable 5 | 0 | 1 | 3 | 4 |
| plugin-observability | Fable 5 | 0 | 2 | 6 | 4 |

*(`†` one HIGH elevated to CRITICAL at system level — see C3. Component counts are as each auditor rated them.)*

## Appendix 2 — Documentation corrected inline during the audit (15 pages)

`auth/oauth.mdx` (open-redirect claim), `auth/users-and-passwords.mdx` (re-hash + XFF claims), `backends/postgres.mdx` + `backends/sqlite.mdx` (pool config behavior), `cli/management-commands.mdx` (`maskkeygen` key handling), `getting-started/settings-and-env.mdx` (pool/env), `orm/masked.mdx` (plaintext-at-rest gap), `plugins/cache.mdx` (cache key + auth caveats), `plugins/permissions.mdx` (superuser/active/PK caveats), `plugins/rls.mdx` (owner-bypass + GUC danger), `plugins/sessions.mdx` (revocation + secret_key), `plugins/storage.mdx` (S3 content-type), `realtime/transports.mdx` (inbound authz), `rest/permissions.mdx` (object-scoping), `templates/rendering-html.mdx` (autoescape is HTML-context-only). All edits make the docs match current code behavior. No source/config was modified by any auditor.

## Appendix 3 — Round-2 plugin & component findings sweep

A second pass fixed the open MEDIUM/LOW items in the per-component findings that weren't in the consolidated CRITICAL/HIGH table above. Each shipped with tests + docs.

| Finding | Sev | Fix | Commit |
|---|---|---|---|
| plugin-authz P4 | MEDIUM | Perm layer is PK-agnostic (resolves the caller as a raw string via `current_user_id_str`); AuthUser probe parses to i64 first so UUID/String PKs aren't locked out. | ee6b517d |
| plugin-authz P6 | LOW | `user_perms` fetch bounded (10k ceiling) against pathological input. | ee6b517d |
| observability #5 | MEDIUM | Analytics outbound sends bounded by a 64-permit semaphore (permit acquired before spawn; drop-on-full). | c8215cb3 |
| observability #9 | MEDIUM | Swagger UI asset base pinned to an exact version + configurable (`swagger_asset_base`) for self-hosting + crossorigin/referrerpolicy. | 7dad8135 |
| observability #10 | LOW | Async signal subscribers bounded by a 30s timeout so a hung one can't stall the ORM write path. | f86b1270 |
| realtime #2 | MEDIUM | `GroupPolicy::can_send` hook (defaults to `can_join`) + `Realtime::can_send` for `MessageHandler` send-authz; docs use it. | d6878afc |
| realtime #4 | MEDIUM | Default connection cap (10k, `unlimited_connections()` opt-out) + per-connection inbound message-rate cap (100/s). | 8d4cfe96 |
| core-app-config #13 | MEDIUM | Graceful shutdown on SIGTERM/SIGINT + DB pool drain. | b8a43a6e |
| core-app-config #16 | LOW | Boot warning for misspelled `UMBRAL_` keys (edit-distance near-miss). | 3fc863c6 |

Plus stale-status items confirmed already fixed earlier this pass: plugin-authz **P1** = H19 (boot audit + gated builders, `549f8dd1`), plugin-authz **P3** = deactivation gate (`860eeb18`), observability **#6** = H14 Prod-in-release default (`6fcbffa8`), observability #8 (stored-XSS in logs admin) = confirmed already safe (admin templates autoescape as HTML globally).

**Still deferred (documented, not silently dropped):**
- **plugin-authz P2** (MEDIUM) — object/row-level permissions. A model-level perm still authorizes any row; genuine object-permission scoping is a large new primitive (an `ObjectPermission` table + per-row checks + a `has_object_perm` API), not a contained fix. Documented loudly in `rest/permissions.mdx`; the convention today is an in-handler ownership check.
- **plugin-authz P5** (LOW) — no action: `has_perm` failing closed on a DB error is correct (a positive), noted only so it isn't mistaken for a bug.
- **core-app-config #4 / H17** — replica pools from `settings.databases` aren't auto-opened; the request-path panic is already a boot error/warning. Auto-opening needs async pool-open in `build()` (deferred).
- **observability #5** full PostHog `/batch` batching (fewer requests) — the concurrency bound lands the self-DoS fix; batching is a further optimization.
