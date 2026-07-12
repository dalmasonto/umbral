# Seen/Known gaps - Continued from @gaps2.md

1. [x] REST `views([...])` means read-only everywhere (routes, OPTIONS Allow, OpenAPI spec, 405 vs 404) — archived
2. [ ] Push notifications implementations
3. [ ] Can one stream a video
4. [x] Flash messages no-op without a pre-existing session — resolved (works with SessionsPlugin) — archived
5. [ ] We need to offer auto SEO ie if a link lacks something like title, we inject it, if an image lacks alt, we use the image link as title, like how can we auto-magically help in terms of SEO
6. [x] Admin dashboard widget catalog filters by `widget.permission` — archived
7. [x] Custom-view paths validated at build (no router panic on reserved/duplicate paths) — archived
8. [x] Per-widget permission checks batched (concurrent, deduped) — archived
9. [x] REST nested writes are create-only; PATCH/PUT ignores nested child arrays — shipped

   `RestPlugin` supported writable nested children only on `POST`; the `update` handler was flat and ignored `cfg.nested`, so a PATCH carrying `{ "items": [...] }` handed the array to the ORM as an unknown column instead of upserting children.

   **Shipped:** `update` now splits declared nested arrays out of the body and upserts children on ONE `umbral::db::begin()` tx (parent update + child writes commit/roll-back together). **Reconciliation: upsert, no implicit deletes** — item WITH the child pk → update (scoped to this parent via the FK; a cross-parent pk is a 404); WITHOUT a pk → create. Rows absent from the payload are untouched. Full replace-set (delete-the-missing) stays a future opt-in (`ResourceConfig::nested_sync(...)`). Test: `plugins/umbral-rest/tests/nested_updates.rs`. Superseded/extended by #10 (recursion). The `update_json_in_tx`-return-is-not-affected-count footgun found here is captured in `.claude/skills/dynqueryset-update-return-semantics.md`.
10. [x] Nested writes only went one level deep; grandchildren were silently dropped — shipped

   `create_nested`/`update_nested` iterated only the parent's `.nested()` specs and inserted each child flat, so a level-3 array (e.g. `order.items[].components[]`) rode along inside the child object and — because the dynamic insert path iterates the child table's columns and validation doesn't flag unknown keys (`crates/umbral-core/src/orm/validation.rs:83`) — was **silently dropped**: no error, no rows. Silent data loss, the exact anti-pattern CLAUDE.md's "fix, don't patch" calls out.

   **Shipped:** both writers are now recursive (`insert_nested_tree` / `upsert_nested_child` in `plugins/umbral-rest/src/lib.rs`). Nesting is driven per table from `cfg.nested`, so a subtree is written iff its parent's table *also* declared `.nested(...)` — one level per declaration, arbitrary depth. Each level: FK auto-set from the parent's just-inserted pk (create) or ownership-scoped upsert (update); `MAX_NEST_DEPTH = 16` guards cyclic/self-referential declarations with a 400. Test: `plugins/umbral-rest/tests/nested_deep.rs` (3-level create + deep upsert + depth-3 cross-parent 404 rollback).

   **Follow-up (deferred):** declaring `.nested()` on a mid-level table also exposes it as a routed REST resource. If a caller wants deep nesting *without* exposing the intermediate table, we need a declaration that registers nesting without mounting routes (e.g. `ResourceConfig::for_::<T>().nested_only(...)` or a plugin-level nested-map). Log a new gaps3 entry if/when that's needed.
11. [x] Auth JSON routes slash-inconsistent with REST → `/api/auth/login/` 404s — archived (fixed, commit 4f30cc4)
12. [x] `GET /oauth/{provider}/login` 500s for an unregistered provider key — archived (fixed, commit e6efb7a)
13. [x] SQLite `AlterColumn` fails with FK-constraint on a table with inbound FKs — archived (fixed, commit a60405a)
14. [x] `update_or_create` UPDATE branch emits bulk_post_save not per-row post_save — archived (fixed, commit fe200c1)

   **Options:** (a) have `update_or_create`'s update branch fetch-and-`save()` the single row (per-row `post_save`) instead of `update_values`, so the whole API is per-row-signal-consistent; or (b) at minimum document loudly that `on_model`/`post_save` won't see `update_or_create` updates and point consumers at `save()` or an explicit push. **Workaround used:** pushed the payment notification explicitly from the handler with `Realtime::to_user(...)` rather than relying on the `on_model` bridge.

---

_Entries #15–#25 harvested from the web3clubs_fc backend (a live consumer; see [[project_web3clubs_fc_backend]]). Findings verified against umbral 0.0.5's actual surface — the app is on 0.0.4, so a few of its workarounds are already resolved (SQLite alter-with-inbound-FK #13, object-scope reads via `ResourceConfig::owned_by`/`.scope`, and `umbral_auth::validate_password` all now exist)._

15. [x] No `IntoResponse` for ORM errors → every handler re-declares `err500` and sprinkles `.map_err(err500)?` — shipped (commit 0763d0c3)

    In the consumer, all 5 plugins open with an identical `fn err500<E: Display>(e: E) -> (StatusCode, String)` and every ORM terminal is `.map_err(err500)?`. The highest-volume boilerplate in the app. REST already has `impl IntoResponse for ApiError` + `From<WriteError>` (`plugins/umbral-rest/src/lib.rs:2222,2254`), but it's REST-internal — plain axum handlers can't reach it.

    **Proposal:** lift an `ApiError` (with `From<WriteError>`/`From<sqlx::Error>` + `IntoResponse`, safe-by-default opaque 500 like WEB-5) to `umbral-core` and re-export from the facade, so a plain handler returns `Result<Json<T>, umbral::ApiError>` and uses bare `?` on ORM calls.

16. [x] REST has read scoping (`owned_by`) but no owner-*injection* on create (`perform_create`) — shipped (commit 4746e946)

    `ResourceConfig::owned_by("col")` / `.scope(...)` filter reads/updates to the caller's rows (audit_2 H1/P2), which the consumer didn't have on 0.0.4 (it hand-rolled `GET /api/me/*`). But there is still no way to *fill* an owner FK from the authenticated identity on **create** and reject a body-supplied value — so every "the member comes from the token, never the body" write (RSVP, chat post, payment record) bypasses REST for a bespoke handler.

    **Proposal:** `ResourceConfig::owner_field("member")` — on create, set the FK from the identity; ignore/reject a client-supplied value. Collapses most of the app's bespoke write handlers back into declarative REST.

17. [x] No lightweight typed current-user extractor — handlers parse `identity.user_id: String → i64` (~8×) — shipped (commit d84e91e2)

    `LoggedIn<AuthUser>` exists but does a DB fetch; the token-only `Identity` gives `user_id: Option<String>` (the PK-LCD), so every scoped handler repeats `let uid: i64 = identity.user_id.parse().map_err(|_| (UNAUTHORIZED, ...))?`.

    **Proposal:** `Identity::user_pk::<T: FromStr>() -> Result<T, _>` and/or a `CurrentUserId<T>(pub T)` extractor (no fetch, 401 on parse failure) generic over the app's PK type.

18. [x] No permission-gated extractor for plain handlers — `require_staff` copy-pasted across plugins — shipped (commit c44c8a0c)

    REST `Permission` types (`IsStaff`, etc.) can only gate viewsets, so the app re-declares an identical `require_staff(&Identity) -> Result<i64, ApiErr>` in `fc-teams` and `fc-payments`.

    **Proposal:** a `Require<P: Permission>` extractor (403s on failure) usable on any axum handler, plus a `RequireStaff(pub i64)` convenience that returns the parsed uid.

19. [x] `AuthUser` isn't extensible — confirmed already solved by the swappable `UserModel` / `AuthPlugin<U>` mechanism — archived

20. [x] Auth ships no authenticated change-password route + `set_password` strength policy — archived

21. [x] `DecimalField` / money type — already shipped for Postgres (`rust_decimal::Decimal` → `NUMERIC(19,4)`); SQLite deferred — archived

22. [x] No permission combinators / common preset — the app's main gate is 7 lines of `Box::new(..) as Box<dyn Permission>` — shipped (commit 55ca0cdc)

    `And(IsAuthenticated, Or(ReadOnly, IsStaff))` is the app's most-used gate (fixtures, attendance, announcements, chat, teams) and reads as verbose dyn-boxing. **Proposal:** ship a named `IsAuthenticatedOrReadOnly` (DRF-style) and/or `.and()`/`.or()` combinators on `Permission` so consumers stop hand-boxing.

23. [x] No `serve`-only migrate/seed lifecycle (auto_migrate_on_serve) — archived

24. [x] Adding a `Choices` variant forces a full `AlterColumn` table rebuild — archived

25. [x] ORM SQLite write transactions used `BEGIN DEFERRED` → SQLITE_BUSY under concurrent writes — shipped `BEGIN IMMEDIATE` (commit 7a03c196)

    Root-caused while fixing the test-suite flake: `m2m.rs` (and `db::begin*`) use `pool.begin()`, i.e. sqlx `BEGIN DEFERRED`. Under concurrent writes on a file DB with >1 connection, a deferred read→write lock upgrade returns SQLITE_BUSY *immediately* (deadlock-avoidance path the `busy_timeout` handler is never consulted for). The test suite worked around it with `max_connections(1)` (commit cbbd1571), but real SQLite apps with concurrent writers can hit it.

    **Proposal:** issue `BEGIN IMMEDIATE` for SQLite write transactions (acquire the write lock at BEGIN, so `busy_timeout` applies and writers wait instead of erroring). Postgres unaffected. SQLite is test-first here, so lower priority — but it's the correct fix.

    **Minor (same source):** roster/payment endpoints do `AuthUser::objects().fetch()` into an in-memory id→username map (a manual join) because there's no `.values()`/annotate-join to pull just `(id, username)` — a scale trap the ORM could close.
26. [x] Admin sheet read flake — product bug fixed; residual is a test-only read-lock artifact, production unaffected — archived

27. [x] audit_2 residual low-severity hardening backlog — all 9 items shipped (2026-07-06) — archived

28. [ ] audit_2 deferred findings — big-design or live-Postgres-gated (verified open 2026-07-06)

    Genuinely-open findings that need a design decision or infra the local env can't provide. Recorded so they're tracked, not lost. See `planning/audit_2/findings/` for full write-ups.

    - [x] **[authz P1]** ✓ ADDRESSED (2026-07-07): the boot-time audit of ungated mutating routes (audit_2 H19) already *warned*; shipped `AppBuilder::deny_ungated_mutations()` to promote that finding to a hard `BuildError::UngatedMutatingRoutes` — opt-in "gated by construction" for app `.routes(...)`. Gates on the recorded permission (core's `Routes::route_gated`, applied by umbral-permissions' `require_permission`), so a properly gated POST is not a false positive. Tests: `deny_ungated_mutations_rejects.rs` (ungated POST → build error) + `deny_ungated_mutations_allows_gated.rs` (gated POST → builds). A *global* default-deny router primitive (every route deny-by-default, including plugin routes) stays deferred (Group B) — it's a future-major posture change, not a pre-submission edge case.
    - [x] **[authz R5]** ✓ FIXED (2026-07-07): RLS policies were append-only across boots — a policy removed from the builder stayed live (and Postgres policies are PERMISSIVE/OR-combined, so it kept granting). `apply_policies` now reconciles: `drop_undeclared_policies` queries `pg_policies` per RLS-managed table and DROPs every policy not in the declared set. New pub `RlsPlugin::apply_to(pool)`; `#[ignore]` PG test `undeclared_policy_is_dropped_on_reapply`. **[authz R4]** (non-ignored two-tenant *enforcement* test) DEFERRED — Group B, doubly-blocked: (1) it needs a throwaway **superuser** Postgres to CREATE a non-owner ROLE + tables + FORCE RLS + set the GUC per-connection — the local PG on :5432 holds the user's real app data and has no role I can safely use, so this is CI-container infra; (2) more fundamentally it can't meaningfully pass until **R2** (request-scoped, connection-pinned GUC setter with guaranteed reset — the finding's "structural / needs design" item, NOT a #28 checkbox) ships, because that IS the enforcement mechanism the test would exercise. Writing the test now would encode assumptions about an unfinished mechanism. Blocked on R2 first, then CI.
    - [x] **[authz P2]** ✓ FIXED (2026-07-07): shipped the object (row-level) permission primitive. New `ObjectPermission` model (`permissions_objectpermission`, grant triple `(user_id, permission_id, object_pk)`) + `has_object_perm(user, perm, object_pk)` — the instance-aware check that does NOT fall back to a model-level grant (closes the IDOR-by-design gap where a model-level holder could act on *any* row), `has_object_perm_for_superuser`, `objects_with_perm(user, perm)` (list-view filter), and `grant_object_permission` / `revoke_object_permission` / `revoke_object_permissions_for` (row-delete cleanup — the grant carries a stringified `object_pk`, not an FK, so deletes don't cascade). 6 behavioral tests in `plugins/umbral-permissions/tests/integration.rs` (per-row scoping, model-grant-doesn't-satisfy, set filter, idempotent grant/revoke, multi-grantee cleanup, superuser). Doc page updated. Adds one table → consumers run `makemigrations`/`migrate`.
    - [x] **[admin #5]** ✓ ADDRESSED (2026-07-07): the primary CSRF defense is the session cookie's `SameSite=Lax` **default** (blocks cross-site forged POST/PUT/DELETE; tested in `same_site_cookie.rs`), so the default admin posture isn't forgeable. Residual risk is an explicit `SameSite=None` (cross-origin SPA) config — admin `on_ready` now **warns** to mount a CSRF middleware in that case (reads new pub `umbral_sessions::configured_same_site()`; reliable via topological `on_ready` order). The hard `"security"` dep + per-handler CSRF self-verify stay deferred (Group B) — they break every non-security-mounting consumer / are a large multi-handler sweep.
    - [x] **[orm #3 / macros #2]** ✓ ADDRESSED (2026-07-07): the recommended core `server_managed` flag is `#[umbral(privileged)]` — deny-by-default on `insert_json`/`update_json`/admin-form, re-enabled per-write via `DynQuerySet::allow_privileged` (tested in `privileged_field.rs`). Built-in `AuthUser` marks `is_staff`/`is_superuser` privileged + `password_hash` noform; regression-guarded by `plugins/umbral-auth/tests/privileged_fields.rs`. A *full* deny-everything writable allowlist (every field opt-in) stays deferred (Group B, larger design).
    - [x] **[realtime #2]** ✓ FIXED (2026-07-07): shipped `MessageContext::publish(group, event, data)` — authorizes the sender via `GroupPolicy::can_send` then broadcasts, dropping unauthorized frames (safe-by-default over raw `to_group().send()`); plus `MessageContext::can_send`. Docs teach `ctx.publish`. Test `tests/publish_authz.rs`. **[realtime #5]** ✓ FIXED (2026-07-07): `dispatch_presence` now sends the `presence:sync` roster ONLY to the joining user's connection(s), not re-broadcast to the whole group on every join — existing members already track the roster from the `presence:join`/`presence:leave` deltas they receive, so a join storm is O(N), not O(N²). The wire messages are unchanged (same three event types/shapes; the bundled client + any delta-tracking client handle the narrower recipient set transparently), so this is NOT a protocol change after all — the earlier "alters the shipped wire protocol" read was wrong (the protocol is the message shapes, not who receives sync). Test `tests/presence_sync_scope.rs`.
    - [x] **[oauth OAU-4]** create-user + create-social now atomic — `create_user_with_social` runs both inserts in one tx with a *fresh tx per username-retry attempt* (sidesteps the PG "constraint violation poisons the tx" problem without savepoints). Enabling ORM fix: `QuerySetTx::create` now classifies constraint violations (was opaque `Sqlx`). Test `policy.rs::social_insert_failure_leaves_no_orphan_user`. (2026-07-07)
    - [~] **[supply-chain SC-3 / SC-5]** SC-5 ✓ FIXED (2026-07-07): `notify 6 → 8.2.0`, no code change (the watcher API livereload uses is stable across majors); the old `inotify 0.9`/`bitflags 1.3`/`mio 0.8` transitives drop out (collapses SC-4), plus a Dev-only "Production" doc note. **SC-3 DEFERRED** as a dedicated architecture task, not rushed pre-submission: gating the sqlx sqlite/postgres drivers behind cargo features requires `#[cfg]`-ing the entire `DbPool` dispatch across ORM/migrate/backend (hundreds of touch points + a CI feature-combo matrix); the markdown/timezone/pg-extra-types gating is more contained. It's binary-bloat/attack-surface, not a functional edge case a user hits — wants a focused PR with sign-off.

29. [x] Boilerplate reduction — what can move into the framework. Audited against the live consumer `web3clubs_fc`; all 7 items shipped (per-row create/delete signals, `ResourceConfig::under`, `order_by_annotation` + `fetch_annotated_as`, `#[derive(Validate)]` + `Valid<T>`, `#[derive(Dto)]`; items 6 and 7 already existed). The residual debt is the discovery path, not the feature list — archived

30. [x] SQLite `AlterColumn` (inbound FKs + data) → 787 — could NOT reproduce on main; already fixed in 0.0.5 (repro was on 0.0.4); engine-level regression test added — archived

31. [x] `#[derive(Choices)]` fields decode as TEXT but pre-0.0.5 migrations made the column VARCHAR → typed reads 500 on Postgres — fixed: the derive's `Type::compatible` now delegates to `String` (accepts the whole text family), so existing VARCHAR columns decode with no migration. Test `choices_varchar_pg.rs` (no-DB `compatible` guard + `#[ignore]` live-PG round-trip) — archived

32. [x] OAuth `begin_flow`'s fresh-session `set_data` emitted no session `Set-Cookie` when a CSRF cookie was present → "no oauth flow in progress" for cookieless clients — root cause was the session layer's emit guard (`!contains_key(SET_COOKIE)`) being tripped by the unrelated `umbral_csrf_token` cookie; fixed: guard now checks for the `umbral_session` cookie specifically and `append`s it (coexists with CSRF). Fixes all fresh+CSRF+`set_data` endpoints, not just OAuth. Test `gaps3_32_session_cookie_beside_csrf.rs` — archived

33. [x] Auth: a user could register twice differing only by case (`dalmasogembo@gmail.com`/`dalmasonto` AND `Dalmasogembo@gmail.com`/`Dalmasonto`) — usernames/emails were stored and matched case-sensitively. FIXED (2026-07-07): `umbral_auth::normalize_username`/`normalize_email` (trim + lowercase) applied at every write boundary (`insert_user` behind `create_user`/`create_superuser`/`create_user_with_flags`) and every lookup boundary (`authenticate`, `verify_email`, `start_password_reset`, both resend-verification routes) + the oauth policy (rule-3 email-link lookup and rule-4 auto-create). Because every stored row is canonical, the existing `#[umbral(unique)]` on `username`/`email` now enforces case-insensitive uniqueness with no schema change. 3 behavioral tests in `plugins/umbral-auth/tests/integration.rs`. **Residual — now CLOSED by #34 (2026-07-08):** direct AuthUser writes through the generic admin form / REST create path previously did NOT normalize (they go through `DynQuerySet`, not the auth helpers). #34 added `#[umbral(trim, lowercase)]` and applied it on the dynamic write path; `AuthUser.username`/`email` now carry it, so admin + REST writes canonicalize too. Every write surface is covered.

34. [x] Framework: no pre-save field-normalization hook — FIXED (2026-07-08) via option (a): declarative `#[umbral(trim)]` / `#[umbral(lowercase)]` field attributes. String-only (compile error otherwise), combinable (trim then lowercase). Plumbed FieldSpec → Column (serde-default, non-schema-affecting like `auto_now`) → applied in the four `DynQuerySet` write builders (`insert_json`/`update_json`/`insert_form`/`update_form`) via `normalize_str` / `normalize_json_for_col`. **Dynamic-path-only** by design (matches the `auto_now` precedent; the typed `.create()` path stays caller-controlled) — chosen with the user over option (b) `Model::pre_save(&mut self)`. Built-in `AuthUser.username`/`email` now carry `#[umbral(trim, lowercase)]`, closing the #33 residual: admin form-submit + REST create/update canonicalize too, so the existing `#[umbral(unique)]` gives case-insensitive uniqueness on every write surface. Tests: `crates/umbral-core/tests/normalized_fields.rs` (4 behavioral: insert/update/form normalize, case-only dup collides) + `plugins/umbral-auth/tests/privileged_fields.rs::auth_user_identifier_columns_normalize_on_dynamic_writes`. Doc page `orm/normalized-fields.mdx`. Option (b) (a general `pre_save` mutation hook for arbitrary/cross-field logic) remains unbuilt — deferred until a real case needs more than declarative trim/lowercase.

35. [x] Case-insensitive columns — the DB-level counterpart to #34's `lowercase`. Where `lowercase` normalizes the stored value (original casing lost), `#[umbral(case_insensitive)]` makes the COLUMN itself case-insensitive (=/UNIQUE/ORDER BY fold case) while PRESERVING the original casing — the Django `CIText` experience. FIXED (2026-07-08): string-only (compile error otherwise); Postgres renders the column as `citext` and the migration auto-emits `CREATE EXTENSION IF NOT EXISTS citext` before the CREATE TABLE; SQLite gets `COLLATE NOCASE`. Schema-affecting but, like `unique`, scoped to CREATE TABLE (the `column_shape` diff doesn't watch it — toggling on a live column needs a hand-written migration). Boot warning `field.case_insensitive.sqlite_ascii` flags that SQLite NOCASE folds ASCII A–Z only (Postgres citext folds per collation). Plumbed FieldSpec → Column (serde-default) → `PostgresBackend::map_column` (Text→citext) + `render_operation_postgres` (extension prepend) + `build_column_def_sqlite` (COLLATE NOCASE). Tests: `migrate.rs::case_insensitive_column_renders_per_backend` (pure DDL render, both backends incl. citext+extension) + `crates/umbral-core/tests/case_insensitive_field.rs` (behavioral on SQLite: case-insensitive UNIQUE collision + case-insensitive lookup + case-preserving storage). Doc `orm/normalized-fields.mdx`. Closes the read/write-data chapter (#33/#34/#35): developers now have both `lowercase` (canonicalize) and `case_insensitive` (preserve-case) for case-insensitive uniqueness/lookup.


36. [x] REST/JSON responses carry no `Cache-Control` — shipped: default `no-store`, `RestPlugin::cache_control(..)` + per-resource `ResourceConfig::cache_control(..)` override, applied by one layer so custom actions are covered — archived
37. [x] Discoverability — shipped `documentation/docs/v0.0.1/idioms.mdx` ("stop hand-rolling these": RequireStaff/RequireAuth over a hand-gate, `db::transaction` for multi-model writes, `#[umbral(trim, lowercase)]` over hand-normalising, + a 10-row "you're about to write X → reach for Y" table). Root cause was worse than navigation: `RequireStaff` was **undocumented anywhere**, and `RequireAuth` did not exist — both now shipped/documented — archived
38. [x] Kikosi (web3clubs_fc) architectural roadmap — ALL SEVEN items resolved: #1/#2/#5 shipped, #3 resolved as the wrong tool (groups, not tenancy → membership scoping), #4/#6/#7 verified largely already-shipped with the real remainders extracted to gaps3 #48-55 and built (typed enqueue, schedules admin, thumbnails, upload allow-list, soft-delete cascade, audit trail, author stamping). Only #52 (presigned S3 upload) remains, deliberately deferred. Full closure table in `planning/building/kikosi.md` — archived
39. [x] The admin plugin issues - The admin plugin sidebar plugin titles are huge, in development they look okay, in production, they are broken. Next the same appears in the titles within the dashboard page and the text for the details and edit dialog. The same details/edit dialog sheet from the right is not full height, it tends to take the height of the content in production. This is more of how the static  or tailwind css is handled whenever we use this in prod - Version 0.0.6 fixed this.

40. [x] Cross-plugin FK ordering is decided alphabetically, and fails only on a fresh database — archived - Why alphabetical though? What if we have an fk on auth ie profile which can't exist alphabetically before auth?

41. [x] `on_ready` fires for every CLI subcommand, so seeds run against an unmigrated schema during `migrate` — archived

42. [x] Datetime/timezone handling: offsets convert correctly; DST-ambiguous and nonexistent local times were silently stored as a third instant — now rejected — archived
43. [x] Field help text now emits a Postgres `COMMENT ON COLUMN` (SQLite has no comment facility; MySQL is not a backend) — archived
44. [x] CSRF token visible in the admin `hx-headers` attribute — by design (double-submit); the real fix was `Cache-Control: no-store, private` on personalised responses — archived
45. [x] Framework-enforced read/write permissions by group — not in the ORM (recursion, no denial error type, second global); shipped via `AuthPlugin::with_db_session_var` + Postgres RLS — archived
46. [x] Detect models instead of a `.model::<T>()` per model — shipped `AppBuilder::auto_models()`: `#[derive(Model)]` self-registers into a link-time slice (inventory), merged + de-duplicated with explicit registration. Opt-in, per the `02-plugin-contract.md` posture: a model in a *library* crate can be dropped by the linker and would then silently vanish from `makemigrations` — archived
47. [x] Landing page showed "why you should use Umbral" value-props — replaced with a **"How Umbral works"** section: the real declare → migrate → get-everything loop as three numbered steps with actual code (model with attributes → makemigrations/migrate → AdminPlugin + RestPlugin + gen-client). A claim asks the reader to take our word for it; the loop shows them, and it answers both "what is it" and "what do I type" in one pass. `render_home` green — archived
48. [x] Name-keyed enqueue — shipped: `#[task]` generates a typed handle (`SendWelcome::enqueue(payload)`), plus `enqueue_task::<T>` and `TasksPlugin::periodic_task::<T>` — archived
49. [x] `PeriodicTask` has no admin model — shipped `umbral_tasks::periodic_admin_model()` (schedule / next_run / last_run / enabled + an Enable-disable bulk action) — archived
50. [x] No built-in image processing — shipped `umbral_storage::thumbnails(..)` behind the `images` feature (derived keys via the new `Storage::store_at`, never upscales, aspect preserved) — archived
51. [x] No upload content-type policy — shipped `StoragePlugin::accept(..)` / `.accept_images()`, enforced as a storage decorator (every save path) and sniffing the BYTES, not the spoofable declared type — archived
52. [ ] No direct-to-S3 presigned **upload**. `plugins/umbral-storage/src/s3.rs` has `presign_get` (`:501`) and server-side `put_object` (`:167`) but no `presign_put`/`presign_post`, so every uploaded byte transits the Rust process. Matters for large files and at scale; not day-one.
53. [x] Soft delete does not cascade — shipped (cascade + cascade-aware restore + boot check) — archived
54. [x] No model-level audit trail — shipped as `#[umbral(audited)]` (both write paths, field-level diff, NULL actor for unauthenticated writes) — archived
55. [x] No `created_by` / `updated_by` auto-stamping — shipped as `#[umbral(auto_user_add)]` / `#[umbral(auto_user)]` — archived (per-user data export / retention remains open)
56. [x] **`DynQuerySet::filter_eq_string` failed OPEN: an uncoercible value dropped the predicate, so a by-id DELETE became a whole-table DELETE.** Found while building `ResourceConfig::under` (#29 item 2), and it turned out to be far worse than first logged — archived.

    The first write-up called this "matches every row instead of none" on a *read*. Writing the test showed the real blast radius: `DynQuerySet::for_meta(&m).filter_eq_string("id", "abc").delete()` lowered to `DELETE FROM widget` **with no WHERE**, and the test emptied the table (8 rows, expected 0). That is reachable from a plain `DELETE /api/widget/abc` against **any model with an integer primary key** — no auth bypass needed beyond whatever delete permission the caller already has for one row. `UPDATE` rewrote every row; reads returned an arbitrary row instead of none.

    A dropped predicate does not narrow a query, it WIDENS it. Fixed by failing closed: `filter_eq_string` now pushes `never_matches()` (`1 = 0`) when `typed_eq_condition` returns `None` — an unknown column or a value that cannot be coerced. `typed_eq_condition`'s doc now states that a `None` OBLIGES the caller to produce no rows. Five regression tests (`uncoercible_filter_fails_closed.rs`), each verified failing before the fix. Whole suite green (2739) with no caller depending on the old behaviour.

    REST's query-string filter path (`parse_filters`) was already correct — it 400s on a malformed value. The hole was confined to the by-id path, which is precisely why it survived: nobody types a garbage primary key by accident.

57. [x] **The examples taught the boilerplate they exist to prevent — and it was an information leak, not just noise** — archived.

    #29 concluded the bottleneck was discovery: `ApiError` shipped days before the consumer's last commit and it hand-wrote ~72 call sites anyway. Half true. Fixing it surfaced **two hard reasons `ApiError` did not fit an HTML handler**, and a reason not to use something beats any amount of documentation telling you to:

    1. `umbral::templates::render(...)` — the single most common fallible line in an HTML handler — had **no `From` impl on `ApiError`**, so `?` did not compile. `ApiError` literally could not be that handler's error type. Fixed: `From<TemplateError> for ApiError`.
    2. The error-page middlewares printed `ApiError`'s **JSON body verbatim** as the page's message, so a browser got a styled 500 reading `{"code":"database_error","error":"internal server error"}`. Fixed: `humanize_error_body` pulls the message (or flattens `field_errors`) out of a JSON error body.

    Those two pushed every author to the hand-rolled alternative — and **that alternative leaks**. `fn internal_error(e) -> (StatusCode, String) { (500, e.to_string()) }` hands the database's own error text to the browser: `no such table: shop_product`, a column name, a constraint. The framework's own examples did it, and **`startproject` generated it into every new app on day one**. The scaffold was teaching the leak.

    Shipped: `internal_error` deleted from `examples/shop`, `examples/derive-demo` **and the scaffold** (whose test now asserts the helper is NOT generated — the assertion is inverted from what it was, because the old one pinned the bug). `Identity::pk::<T>()` + `IdentityPkError` replace the `.parse::<i64>()` that `Identity.user_id`'s **doc-comment used to instruct** — documentation that hands you a snippet decides your code, and that one was dictating the boilerplate it should have replaced. New idioms section (#4) leads with the leak. 3 tests; whole suite green (2742); both examples build clean.

58. [x] **`umbral_website`'s `internal_error` pattern — CORRECTED: it was NOT a live production leak.** Archived, with the original claim retracted.

    **What I said:** "every 500 on the deployed site hands the visitor the database's own error text." **What is actually true:** it did not, and I should have verified that before calling it an information disclosure. Two independent guards were already in place, and I checked neither:

    1. `AppBuilder::default_error_pages` defaults to **`true`**, so `render_500_middleware` is always mounted (unless an app calls `disable_default_error_pages()`), and it re-renders any plain-text 500 through the 500 template.
    2. `build_500_context` **blanks `error_display` outside dev mode** — so even a template that printed the message would show an empty string in production. `umbral_website/templates/500.html` does not print it in any case.

    Verified end to end, not by grep: booted the site in `Prod` against an EMPTY Postgres so every DB-touching handler 500s, and hit `/`, `/plugins`, `/community`, `/features`, `/showcase`, `/blog`. All six returned 500 with **no table name, no SQL, no driver text** on the wire — the visitor gets "Something went wrong / We've been notified", while the server log carries the real cause (`relation "plugin_directory_plugin" does not exist`). That is the correct shape, and the pre-fix code would have produced it too.

    **What the change is actually worth** (kept, and still right):
    - The `(StatusCode, String)` + `err.to_string()` pattern is **one config change away from leaking**: `disable_default_error_pages()`, or a custom `error_template(status, ...)` page for a non-500 status (that path renders the body text and is NOT dev-gated). `ApiError` cannot leak regardless of how the app is configured, because it never puts the cause in the body at all. Defence in depth, not a live fix.
    - It leaks in **dev mode** (intended there, but it is the same code path).
    - It removes the pattern from the teaching surface, alongside #57.

    Framed honestly: this is a **hardening + consistency** change, not a security fix. The security framing was mine and it was wrong.

62. [x] **`ApiError` could not express 401 / 403 / 429, which is what pushed handlers onto the leaky tuple** — archived. Core's `ApiError` had `NotFound` / `BadRequest` / `Validation` / `Database` / `Internal` and nothing else. A handler needing any other status had to abandon `ApiError` entirely and hand-roll `(StatusCode, String)` — whose 500 arm is `err.to_string()`, i.e. the leak in #57 and #58. **A missing variant is not a cosmetic gap; it decides which path people take.** Added `Unauthorized` / `Forbidden` / `TooManyRequests` + constructors. These three DO send their message (a developer wrote "Please log in to post a note."), unlike `Database`/`Internal` whose cause is logged and never surfaced — the test pins both halves.

59. [x] **Hardcoded `i64` primary-key assumptions in the PLUGINS.** CLOSED — the schema half shipped too (`admin_audit_log.object_id` and `umbral-logs.user_id` are now TEXT), verified by migrating `examples/shop`'s REAL database: 2 existing audit rows preserved, `object_id` `1` → `'1'`, zero rows lost across 48 tables. The ORM's PK refactor lifted the i64 assumption end-to-end (typed + dynamic paths, relations, M2M, joins) — but the *plugins* never followed. Audited repo-wide 2026-07-12.

    **SHIPPED (no schema change needed):**
    - **`umbral-permissions/src/rest.rs:98` — SECURITY, the worst of them.** `WithPermissions::authenticate` did `identity.user_id.parse::<i64>()` and, on `Err`, set `(is_active, is_superuser) = (false, false)`. A custom `UserModel` keyed by `String`/`Uuid` NEVER parses as i64, so *every* such request landed there: `extras["is_active"] = false` makes `HasPermission::check` return Forbidden before it looks at a single codename, AND the `if is_active && !is_superuser` guard then skipped populating `extras["permissions"]` entirely. Net: **every REST route gated by a permission 403'd every non-i64-keyed user, permanently and silently — superusers included.** The comment above it even claimed "the codename grants below still work"; they did not, because that line had already switched them off. Root cause is collapsing THREE states into two: "the row says inactive" and "this is not an `AuthUser` at all" are different facts. Now: unknowable flags leave `is_active` ABSENT (which `check` already documents as benefit-of-the-doubt), `is_superuser` stays `false`, and codenames are always populated — so the codename check, whose job that is, governs. `middleware.rs::auth_user_flags` had this right all along. 2 regression tests, verified failing against the old line.
    - **`umbral::orm::typed_json_value(col, &str)`** — NEW. The JSON twin of `typed_eq_condition`: asks the COLUMN its type instead of guessing from the value's shape.
    - **`umbral-rest` `inject_parent` + `inject_owner_field`** — both did `match s.parse::<i64>() { Ok(n) => number, Err(_) => string }`, which is a guess about the column's type made from the shape of the value. A `Uuid` pk fell to the string arm and worked by luck; a **`String` pk whose value is numeric** (`"12345"`, an external ref) was written as a JSON *number* into a TEXT column, and a zero-padded `"007"` became `7` — **stamping the row with the wrong owner**, on an authorization-bearing column. Both now use `typed_json_value`.
    - **`coerce_csv_cell`** used the raw `col.ty`, not `fk_effective_type`, so a CSV import of an FK pointing at a String-keyed target coerced a numeric-looking value to a number.
    - **Doc-comments that TAUGHT the bug**: `ActionContext.pk` ("Parse with `.parse::<i64>()`"), `sessions::current_user_id_str` ("`.parse::<i64>().ok()` round-trips it back" — on the one API built to be PK-agnostic). Both now point at the model's own `PrimaryKey` / `Identity::pk::<T>()`.

    **STILL OPEN — needs a schema change + a migration, so it needs the operator's consent:**
    - `admin_audit_log.object_id` is `INTEGER` / `Option<i64>` (`umbral-admin/src/models.rs:370,407,485`). The admin object-history page **400s** for every row of a String/Uuid-keyed model (`handlers/history.rs:42`), and every admin write logs `object_id = NULL` for such a model (crud.rs:704/879/935, inline_edit.rs:192, sheet.rs:521 — the password-change audit — and actions.rs:109/261). The audit trail structurally cannot address a non-i64 row.
    - `umbral-logs`: `LoggedUserId(pub i64)` / `RequestLog.user_id: Option<i64>` (`lib.rs:83,357,363`) — a Uuid-keyed user's requests are recorded unattributed.

    **Both SHIPPED**, storing the PK as TEXT exactly as the session table already does. Proven against `examples/shop`'s live database (the operator confirmed it was not running): `makemigrations` → `migrate` applied the `AlterColumn`, the 2 existing audit rows survived with `object_id` `1` → `'1'`, and a row-by-row diff of all 48 tables shows **nothing lost**. Doing it on a real DB with real rows is what surfaced #60 and #61 below — a fresh database would have proven nothing.

60. [x] **`DROP TABLE` / `DROP M2M TABLE` were not idempotent, so a drifted ledger made a database permanently unmigratable** — archived. `DROP INDEX` and `DROP VIEW` have always rendered `IF EXISTS`; `DropTable` and `DropM2MTable` did not, on either backend. Found migrating `examples/shop`: a pending `DropTable permissions_grouppermission` for a table that had *already* been dropped out of band could never be applied, and the only escape an operator can see at that point is deleting the database. A migration states the DESIRED END STATE ("this table should not exist"); erroring because the world already agrees with you is not a safety property. Both drops now render `IF EXISTS`.

61. [x] **`migrate` reported "Applied 19 migration(s)" against an in-memory database and persisted nothing** — archived. Two compounding bugs, both found only by running against a real app:

    - **A `UMBRA_`-prefixed env var is INVISIBLE.** The framework was renamed `umbra` → `umbral` and the settings prefix moved with it. `warn_on_near_miss_keys` only sees `UMBRAL_`-prefixed keys, so a leftover `UMBRA_DATABASE_URL` is not reported as unmapped — it never reaches figment at all. `examples/shop`'s **entire `.env` had been dead since the rename** (database url, bind addr, secret key, allowed hosts, environment — all five silently falling back to defaults). New `warn_on_legacy_umbra_prefix()` names the variables and the fix.
    - **The default `database_url` is `sqlite::memory:`.** So the app above ran against a database that evaporates on exit, and `migrate` cheerfully printed "Applied 19 migration(s)" having written to nothing. **Success against nothing is worse than an error, because the operator will now trust it.** `migrate` now REFUSES an in-memory database and explains why; `--allow-in-memory` is the explicit opt-in for tests and CI (ephemeral migrates are legitimate — saying so out loud is the point).

63. [x] **NOT A GAP — rejected.** I logged this as "AuthPlugin serves the form POSTs but not the GET pages, so every app hand-writes its own login/signup page." That framing was wrong, and the correction is a principle worth keeping:

    **If we serve `GET /auth/login`, we have designed your HTML.** The moment the framework renders the login form, the app inherits our markup, our class names, our layout, our copy and our idea of where the "forgot password" link goes — and the first task becomes overriding all of it.

    The split we actually want is the one that already exists: the framework owns the half where a mistake is a **vulnerability** (password hashing, throttling, enumeration-safe errors, the session cookie, CSRF, the open-redirect guard on `?redirect=`), and the app owns the half where a mistake is a **preference** (the page). `POST /auth/{login,signup,logout}` is the security-bearing surface, and it is complete.

    Shipped instead: `documentation/.../auth/login-and-signup-pages.mdx` — how to point your own template's `<form>` at the endpoints, with the three things that actually trip people up (`{{ csrf_input | safe }}`, the `flash` loop, and `?redirect=`). And the rescued orphan test was **rewritten** rather than kept `#[ignore]`d: `plugins/umbral-auth/tests/form_routes_surface.rs` now covers the POST surface that DOES exist, and asserts `GET /auth/login` is a **405** — so if the framework ever starts serving the page, the test fails and tells us we have begun deciding somebody's HTML.

64. [x] **`umbral startproject` produced a project that does not compile — in the SHIPPED 0.0.6 release** — archived. `Environment` derived only `Clone, Debug, Deserialize`, while the scaffold generates the single most obvious thing anyone writes with it:

    ```rust
    if umbral::settings::get().environment != Environment::Dev { ... }   // seed/credentials.rs
    ```

    No `PartialEq` → `error[E0369]: binary operation != cannot be applied to type umbral::Environment`. Verified against the **published** `umbral-cli-0.0.6` source in the registry: it emits that exact line, and published `umbral-core-0.0.6`'s `Environment` has no `PartialEq`. So **every `umbral startproject` on the current release yields a broken project on the very first `cargo build`** — the framework's front door. Fixed: `#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]`.

    Found only by actually scaffolding a project and building it. No test covered it, because the scaffold's *output* was never compiled — the CLI tests assert on the generated file CONTENTS (`views_mod.contains("re-export")`), which cannot catch "this code does not build". A generator whose output is never compiled is a generator you are guessing about.

    Also fixed alongside: `Html` was missing from the prelude (`Json` was there — an odd asymmetry for a framework whose scaffold renders templates), so the generated project emitted an unused-import warning on a brand-new build.

65. [ ] **The scaffold pins the LAST RELEASE while generating code for `main`'s API.** `scaffold.rs` uses `env!("CARGO_PKG_VERSION")` — the CLI's own version, which during development is the last published one (0.0.6). So `cargo run -p umbral-cli -- startproject foo` from a HEAD checkout writes `umbral = "0.0.6"` into the new project's Cargo.toml and then generates code against **main's** API. Today that fails on `From<TemplateError> for ApiError` (#57, unreleased) and `AppBuilder::build_deferred` (unreleased).

    Self-healing at release (the scaffold and the libs ship together), and `--local <repo>` path-deps correctly — so contributors have a working path and end users of a *published* CLI are unaffected by the skew itself. But the default invocation from a checkout is silently broken, which is exactly how #64 survived: nobody builds what `startproject` emits.

    Fix: **compile the scaffold's output in CI.** `startproject --local` into a temp dir, `cargo build`, assert zero errors AND zero warnings. That one test would have caught #64, #65, and the unused-import wart in a single run. (A content-assertion test cannot.) Consider also warning when `startproject` runs from a source checkout without `--local`.

66. [x] **`{{ static(...) }}` rendered `href="&#x2f;static&#x2f;css&#x2f;app.css"` — every page, every app** — archived. `static()` and `media_url()` returned a plain `String`, which minijinja autoescapes in HTML context, so every generated URL came out with its slashes as `&#x2f;`.

    It **worked anyway** — browsers decode character references inside attribute values, so the stylesheet loaded and the pages looked right. That is exactly why it survived: nothing was broken enough to notice, and anyone who read the page source would reasonably have concluded that static serving was broken.

    Fixed with `templates::safe_url(url) -> minijinja::Value`, used by `static()` and `media_url()` in core AND in `umbral-admin`, which had its own copy of both functions (and so its own copy of the bug).

    **The fix has to hold both ends, and marking the URL unconditionally safe would have been an XSS hole.** `media_url(key)` takes a key that came from an *uploaded filename* — user-controlled — and a key containing `"` closes the `href` attribute: `a" onerror="alert(1)`. So `safe_url` only marks a URL safe when it carries no HTML-special character. A path a template author writes by hand (`css/app.css`) never does; a hostile filename does, and it keeps its armour. Three tests pin both halves.

    Worth recording how the test nearly lied: written with `minijinja::render!`, which uses an *unnamed* template — and minijinja decides autoescaping from the template's file extension, so autoescape was OFF and the XSS assertion passed no matter what `safe_url` did. The test only became real once it rendered through a template named `t.html`.

67. [x] **The admin claimed `v0.0.1` on every page — a hardcoded literal, wrong since 0.0.2** — archived. `base.html` and `login.html` carried the string `v0.0.1` verbatim. The workspace has been at 0.0.6 for a while; nothing tied the two together, so it would have gone on being wrong forever. A hardcoded version is not a value, it is a claim nobody is checking.

    Now derived from the crate (`env!("CARGO_PKG_VERSION")`), so it cannot rot. And two controls, because *whose* version an admin shows is a product decision, not the framework's:

    - `AdminPlugin::show_version(false)` — hide it. Arguably the better default for anything public-facing: a login page that announces your framework version is free reconnaissance for anyone matching it against a CVE list.
    - `AdminPlugin::version(concat!("MyShop v", env!("CARGO_PKG_VERSION")))` — show YOUR version instead of ours. The operator of a shop is not shipping umbral; they are shipping their shop.

    `.version(...)` implies showing it, so it wins after a `show_version(false)` — the caller said what they wanted second. 5 tests, including a source-level guard that fails if any admin template hardcodes a version literal again (a builder test cannot see a template, and the bug was *in* a template).

68. [x] **`umbral_website` had NO mobile navigation at all** — archived. The header's `<nav>` is `hidden lg:flex`, and no hamburger existed anywhere in the markup. So below `lg` (every phone, every tablet) the entire navigation — Product, Resources, Docs, Plugins, Sponsor, Log in — was **unreachable**. Not cramped, not ugly: gone. A visitor on a phone could reach the page they landed on and nothing else.

    Shipped: a hamburger (`lg:hidden`, morphing to an X while open) and a full mobile panel mirroring every desktop link, with `aria-expanded` / `aria-controls`, Escape-to-close, close-on-link-click (needed under `hx-boost`, where no full navigation occurs), and close-on-resize-past-`lg` so an orphaned open panel cannot reappear when the user resizes back down.

    Also fixed the cramping the hamburger exposed: the header's inner `gap: 2.6rem` was a desktop measurement applied at every width, which squeezed the CTA until **"Sign up" wrapped onto two lines** on a 390px phone.

    Verified in a real browser at 390px and 768px: hamburger visible, panel opens with 11 links, Escape closes it, and it is hidden again at 1280px.
