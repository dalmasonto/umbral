# umbral-permissions — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbral-permissions/src/{lib,models,perm,membership,middleware,rest}.rs` + `tests/{integration,middleware}.rs`. Citations are `file:line` against real code. Nothing modified. NET-NEW findings are marked **NEW**; already-filed items (#71 `set_user_groups`/`add_user_to_group`, #72 serial-perm-query storm, #75 inactive-superuser) are noted as **ALREADY FILED** and not re-counted.

## Verdict

**Solid and substantially complete.** The Django auth-permissions surface is faithfully reproduced: ContentType / Permission / Group / UserGroup / UserPermission models, per-model auto-permissions (add/change/delete/view) seeded at boot, direct + group-mediated `has_perm` / `user_perms`, an M2M-shaped membership API, a `permission_required` tower layer, and an opt-in REST permission bridge. The PK-agnostic `user_id: String` design is a deliberate, well-reasoned choice that makes the plugin work with i64/UUID/slug user models. The gaps that remain are mostly the *framework-contract* ones the plugin can't fix alone (#59 auto-wiring, #60 cross-cutting middleware) plus a handful of net-new correctness/robustness items below. No SQL injection, no auth-bypass for an active user, dependency arrow is clean.

**Completeness one-liner:** ~90% of Django's perm surface ships and works end-to-end; the missing 10% is object-level perms (Django doesn't core-ship these either), `has_perms` (plural), and the "register the plugin → perms automatically gate every app" auto-wiring (#59/#60, already-open framework gaps).

## Completeness

Mapping against Django `django.contrib.auth` permissions:

| Django capability | umbral-permissions | Status |
|---|---|---|
| `ContentType` (app_label, model) | `ContentType` model, `unique_together` | ✅ shipped |
| `Permission` (codename, content_type, name) | `Permission`, PK = composite codename string | ✅ shipped (codename-as-PK is a deliberate divergence, gap #60) |
| `Group` + group→permission M2M | `Group { permissions: M2M<Permission> }`, auto-junction | ✅ shipped (replaced explicit `GroupPermission`) |
| user→group M2M | `UserGroup` explicit join + membership API | ✅ shipped |
| user→permission M2M (direct) | `UserPermission` explicit join + membership API | ✅ shipped |
| Per-model auto-permissions (add/change/delete/view) | `ensure_standard_permissions` in `on_ready` | ✅ shipped |
| `user.has_perm("app.codename")` | `has_perm(user_id, perm)` free fn | ✅ shipped |
| `user.has_perms([...])` (plural) | — | ❌ **missing** (trivial to add: loop + AND; see Findings) |
| `user.get_all_permissions()` | `user_perms(user_id) -> HashSet<String>` | ✅ shipped |
| Superuser bypass | `has_perm_for_superuser` + middleware/REST bypass | ✅ shipped (but inactive-superuser leak — #75) |
| `@permission_required` view decorator | `permission_required` / `permission_required_html` tower layer | ✅ shipped |
| Object-level permissions | — | ⚪ not shipped (Django also defers to django-guardian; reasonable) |
| Admin RBAC management UI | — | ⚪ deferred (gap 19 / lib.rs:63) |
| Auto-gate-every-app middleware (`.enable_permissions()`) | — | ⚪ **deferred — gap #59 / #60** (framework-contract, not a plugin bug) |
| REST viewset permission gate | `WithPermissions` + `HasPermission` (feature = "rest") | ✅ shipped |

No `todo!()`, no `unimplemented!()`, no `// TODO`/`// FIXME` in `src/`. The one "skip-with-grace" path (`ensure_standard_permissions` returns `Ok(())` when `permissions_contenttype` isn't migrated yet, `lib.rs:212-218`) is documented and intentional, not a stub.

The M2M membership API (`add_user_to_group`, `set_user_groups`, `grant_user_permission`, `groups_for_user`, `is_in_group`, `direct_permissions_for_user`, `has_direct_user_permission`, `group_ids_for_user`, plus the revoke/remove pair) is the documented substitute for `AuthUser { groups: M2M<Group> }`, which is structurally blocked by the dep arrow (can't reverse `permissions → auth` without a cycle). This is the right call and well-explained (`membership.rs:1-30`).

## Findings

### NEW — Correctness

- **NEW · Important · `lib.rs:194-281` (`ensure_standard_permissions`)** — the boot seed uses `get_or_create` per row with **no surrounding transaction and no UNIQUE backstop on `(content_type_id, codename)`**. The `Permission` table has its PK on `codename` (string), and ContentType has `unique_together` on `(app_label, model)`, so a *duplicate* row can't land — but two app instances booting concurrently (blue/green deploy, or a multi-process test harness) each run `get_or_create`, and `get_or_create` is itself SELECT-then-INSERT with no tx (the documented #71 family weakness). Worst case is a spurious `UniqueViolation` surfaced as `sqlx::Error::Protocol("permissions seed permission: …")` that **aborts the entire boot** (`lib.rs:274-276` maps any write error to a fatal `?`). Fix: catch `WriteError::UniqueViolation` in the seed loop and treat as already-seeded (idempotent), the same posture `add_user_to_group` should adopt under #71. → fold into **#71** (same root cause: get_or_create/idempotent-write under concurrency).

- **NEW · Important · `lib.rs:294-300` (`table_app_label`)** — app_label is derived by splitting the table name at the *first* underscore. Two different plugins whose table prefixes collide at the first segment (e.g. a `blog_*` plugin and a hypothetical `blog2`-prefixed-as-`blog_v2_*` table, or any model whose `plugin` name contains an underscore) get the **same** app_label, so their `add_<model>` permission codenames can collide into one `Permission` row pointing at the wrong ContentType. More concretely: a bare table `post` and a plugin table `app_post` both map to a permission under app_label `app` → codename `app.add_post` for two distinct models. The composite-codename PK then silently merges them (`get_or_create` matches the existing row). Django avoids this because `app_label` is the real registered app name, not a string-split of the table. Fix: thread the real plugin/app name from `ModelMeta` (the plugin contributing the model knows its own name) instead of reconstructing it from the table string. → **NEW gap** (file as a permissions-correctness entry).

- **NEW · Optional · `perm.rs:101-106` (`has_perm` malformed-string)** — `has_perm` returns `Ok(false)` when the perm string has no `.`. That's the documented contract, but it means a typo'd codename (`"blogpublish_post"` missing the dot) silently denies rather than erroring — a footgun for call sites that expect a real check. Django's `has_perm("typo")` also returns False, so this matches Django; noting only because combined with the middleware `unwrap_or(false)` (`middleware.rs:246`) a *malformed config string* in `permission_required("typo")` makes the gate permanently 403 with no diagnostic. Fix: a debug-level log on the no-dot branch. → minor, fold into a doc/log note.

### NEW — Robustness

- **NEW · Important · `lib.rs:150-163` + `middleware.rs` / `rest.rs` — three divergent on_ready/async-bridge patterns across the plugin family.** `PermissionsPlugin::on_ready` uses `block_in_place` + `Handle::current().block_on` with a fallback to a fresh `Runtime` (correct, tolerates tokio-test). The sibling `umbral-rls` uses bare `Handle::current().block_on` (panics under `#[tokio::test]`, as its own docstring admits, `rls/lib.rs:43-46`). The permissions plugin's own comment (`lib.rs:147-149`) explicitly calls out that rls does it the panicking way. This is a real inconsistency: the *correct* bridge (the one permissions uses) should be a framework helper (`umbral::plugin::block_on_ready(fut)`) so every plugin's `on_ready` gets the runtime-tolerant form for free. As-is, each plugin re-implements it and one of them (rls) is wrong. → **NEW gap** (framework: a shared sync→async `on_ready` bridge helper; would also fix the rls finding in its report).

- **NEW · Optional · `lib.rs:189-192` (`seed_standard_permissions_for_tests`)** — `#[doc(hidden)] pub`. It reads the ambient pool via `pool_dispatched()` and re-runs the seed. It's only needed because `on_ready` fires before `migrate` in the boot order, so a fresh DB never gets seeded on first boot — the user must boot **twice** (`lib.rs:185-187` documents "seeds on the second boot"). This double-boot requirement is a real UX wart: a brand-new `cargo run -- migrate && cargo run -- serve` leaves zero standard permissions after the *first* serve, because `migrate` (a separate process) created the tables but the seed only runs inside `App::build`'s `on_ready`, which in the `migrate` process skip-graced (tables didn't exist yet at build time) and in the `serve` process now sees the tables and seeds. So it does work by the second process — but a single-process `App::build` that runs migrate inline never seeds. Not a bug in the documented flow, but the seed-timing coupling is fragile. → noting; fold into #59 (the broader "permissions auto-wiring" rework should move seeding to a post-migrate hook).

### NEW — Performance

- **NEW · Optional · `perm.rs:184-214` (`user_perms`) + `rest.rs:117` — `user_perms` runs 2-3 serial queries per authenticated REST request** (UserPermission fetch, UserGroup fetch, `permissions_union_for`). `WithPermissions::authenticate` calls it on **every** authenticated request to populate `Identity::extras["permissions"]`. For a high-traffic API this is the per-request analogue of the admin changelist storm (#72). The set isn't cached/memoized within a request and there's no short-TTL cache. Distinct surface from #72 (that's the admin changelist; this is the REST auth decorator), but same family. → fold into **#72** as a third surface (REST `WithPermissions` per-request perm-set load).

### ALREADY FILED (confirmed, not re-counted)

- **#71** — `set_user_groups` non-transactional DELETE+INSERT (`membership.rs:77-97`); `add_user_to_group`/`grant_user_permission` don't catch `UniqueViolation` → spurious error under concurrent idempotent re-add (`membership.rs:40-52, 130-142`). Confirmed exactly as filed.
- **#72** — ~12-14 serial permission queries per admin changelist (the admin-side `AdminPerms::load`). Confirmed; the `user_perms` REST path above is an additional surface.
- **#75** — inactive-superuser permission bypass on a live session (`rest.rs:97-105`, `middleware.rs:263-272`). Confirmed: `WithPermissions::authenticate` (`rest.rs:104`) reads `u.is_superuser` but never `u.is_active`; `is_superuser_safe` (`middleware.rs:270`) same. A deactivated superuser keeps full bypass until their session expires.

### Plugin-contract assessment (clean)

- **Facade-only imports:** ✅ All `src/` files import via the `umbral` facade (`umbral::orm`, `umbral::plugin`, `umbral::migrate`, `umbral::web`, `umbral::db`) or sibling plugin crates (`umbral_auth`, `umbral_rest`). No `umbral_core::` internal path anywhere.
- **Owns its migrations:** ✅ Contributes 5 models via `Plugin::models()` (`lib.rs:116-128`); the 6th table (`permissions_group_permissions`) is the framework-emitted M2M junction. No hand-rolled DDL in `src/` — the historical SQLite-only `CREATE TABLE IF NOT EXISTS` bootstrap was correctly retired (`lib.rs:208-211`).
- **`umbral-permissions → umbral-auth` dep:** ✅ Clean, not a cycle risk. The arrow is intentional and correct (permissions sit above auth in the layering; `Cargo.toml:14-18` documents it). `has_perm`/`user_perms` are deliberately *free functions taking `user_id: &str`* precisely to avoid the reverse arrow that would create a cycle (`perm.rs:1-9`, `lib.rs:29-41`). The only `umbral-auth` consumers are `middleware.rs` (`current_session_user_id`, `AuthUser` for the superuser probe) and `rest.rs` (`AuthUser`). Both are AuthUser-specific by construction.
- **`rest` feature gating:** ✅ `umbral-rest` is an *optional* dep behind `features = ["rest"]` (`Cargo.toml:30-40`), so a REST-free app depending on permissions does not drag in the serializer crate. Honors the "REST-free app compiles with zero serializer code" contract.
- **No raw `sqlx::query` in `src/`:** ✅ Verified — every row read/write goes through the ORM (`.objects()`, `.create()`, `.bulk_create()`, `.delete()`, `.exists()`, `.fetch()`, `get_or_create`, the macro-emitted M2M helpers). Raw SQL appears only in `tests/` (the allowed test exception).

## Tests

**Coverage: good for the happy path, thin on the net-new edges.**

`tests/integration.rs` (601 lines) covers: standard-perm auto-creation, ContentType seeding, `has_perm` false-on-empty, malformed-string false, direct grant → true, group-mediated grant → true, superuser bypass, non-superuser fall-through, `user_perms` union, `has_perm_scoped`, and the full membership round-trip suite (add/remove, `set_user_groups` replace + empty-clear, grant/revoke idempotency, the post-refactor group path). `tests/middleware.rs` (128 lines) pins the layer config shapes and unauth 401/302 responses.

Gaps in test coverage vs the findings above:

1. **No concurrency test** for `set_user_groups` (#71) or the seed loop — the empty-membership window and the spurious-UniqueViolation are untested. (Hard to test deterministically, but a two-task interleave on the same `user_id` would catch the seed-loop abort.)
2. **No inactive-superuser test** (#75) — `superuser_always_passes_has_perm` (`integration.rs:316`) only exercises the `has_perm_for_superuser` bool flag, never the `is_active=false` + live-session path through `rest.rs`/`middleware.rs`.
3. **`table_app_label` collision** (NEW finding) — the unit tests (`lib.rs:306-325`) cover the happy cases but not the `post` vs `app_post` → same `app.add_post` collision.
4. **`permission_required` layer** — `middleware.rs` tests are config-shape only; the actual *Service* `call` (superuser bypass, `has_perm` → 403, inner-call on allow) is **not** exercised end-to-end (the test file itself notes this is "exercised by the live admin in derive-demo", `middleware.rs:3-5`). No CI coverage of the deny path or the superuser-bypass path through the real layer.
5. **REST `WithPermissions`/`HasPermission`** — `rest.rs:193-262` unit-tests `HasPermission::check` against hand-built identities (good), but `WithPermissions::authenticate`'s DB-loading path (the `is_active` gap, the `is_superuser` probe, the `user_perms` population) has **no** integration test.

The PG-only nothing here (permissions is backend-agnostic, runs on SQLite in tests). Test infrastructure is sound (real SQLite engine + migration run + ORM round-trips, matching the "behavioral tests, not random asserts" convention).
