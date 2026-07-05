# audit_2 — Secure-by-default posture reversals (design for approval)

Status: **proposal, awaiting approval.** These four items were deferred from the top-down hardening pass because each changes a framework-wide default: it alters how *every* consumer app behaves at boot or on the request path, and in some cases hard-fails an existing build. Unlike the contained fixes (C2, H3, H10, H16/H17, H21, H23, H24 — all shipped), these are not mine to decide unilaterally. This doc states the problem, the proposed change, the blast radius, and a migration path for each, and ends with the specific decisions I need from you.

The related tenant work has its own doc: [`DESIGN_rls_tenant_isolation.md`](./DESIGN_rls_tenant_isolation.md). C3 below is the session-binding half of it.

---

## C3 — Bind the tenant to the authenticated session (server-side)

**Problem.** `umbral-tenants` resolved the active tenant from a client-supplied `X-Tenant` header. C2's partial fix turned that header OFF by default and made db-per-tenant routing fail closed on an unknown tenant. What remains: there is still no *positive* binding between the authenticated user and the tenant they may act as. An app that re-enables a tenant header (or derives the tenant from any client input) has no framework primitive that says "this user belongs to this tenant."

**Proposed change.**
1. Add a `TenantMembership` contract: a plugin-provided resolver `fn tenant_for(user) -> Option<TenantKey>` (default impl: none). The tenant middleware calls it and sets `RouteContext` from the *server-side* answer, never from a header, unless the header value is validated to equal the user's membership.
2. Ship a default `UserTenant` model (`user_id → tenant_id`) + a `current_tenant()` extractor that reads the bound tenant, mirroring `current_user()`.
3. Keep the fail-closed routing from C2: no membership → no tenant → default/`public` only, never a guessed tenant.

**Blast radius.** Additive for single-tenant apps (they never set a resolver → unchanged). Multi-tenant apps that today rely on `X-Tenant` must register a membership resolver. This is a new opt-in surface, not a default flip — so **low** blast radius, but it does gate the "real" multi-tenant story on the new model + resolver existing.

**Migration path.** New model → autodetected migration. Existing tenant apps add one `.plugin()`/resolver call.

**Recommendation.** Build it as described — it's additive and it's the missing keystone under C2. **Open decision:** should `current_tenant()` require an authenticated user (fail closed for anonymous requests), or return `None` silently? I recommend **fail closed** for any route that has already resolved a user.

---

## H7 — Session revocation works on every store

**Problem.** `SessionStore` is only half-wired: `active_store()` is consulted in exactly one place (`session_layer` load/save). `revoke_user_sessions` (called by password-reset), `destroy_session`/logout, and `read_session` hit the SQL `session` table directly. Under the documented `RedisStore`/`CookieStore`, those are no-ops against an empty SQL table — **a password reset does not invalidate a stolen Redis/Cookie session** (up to the 14-day TTL). `CookieStore` also only *logs* on an empty `secret_key` unless the optional `umbral-security` plugin is present.

**Proposed change.**
1. Add `SessionStore::destroy_user(user_id)` to the trait; route `revoke_user_sessions` / `destroy_session` / `read_session` / `set_data` through `active_store()`.
2. `CookieStore` refuses to boot on an empty `secret_key` regardless of `umbral-security` (move the guard into the store itself).
3. `CookieStore` is stateless, so server-side revocation is impossible without a denylist or token rotation. Ship a **short-TTL + rotation** default for `CookieStore` and document that "log out everywhere" needs `DbStore`/`RedisStore`, OR add an optional revocation denylist. (This is the real design fork — see decision.)

**Blast radius.** `DbStore` (the default) is unaffected — it was invisibly correct. Apps on `RedisStore`/`CookieStore` change behavior (revocation starts actually working; an empty-key CookieStore starts failing the boot it should already fail). Adding a trait method is a breaking change for any third-party `SessionStore` impl (they must implement `destroy_user`) — mitigate with a default method that returns an explicit `Unsupported` for stateless stores.

**Migration path.** Trait gets a defaulted method (no break for existing impls). Add an index on `session.user_id` (autodetected migration) so `destroy_user` isn't a full scan at 10M rows.

**Recommendation.** Do items 1 and 2 unconditionally. For item 3, I recommend `CookieStore` **cannot** offer "log out everywhere" and should say so at the type level (its `destroy_user` returns `Unsupported`, and the password-reset flow warns loudly when the active store can't revoke) rather than pretending. **Open decision:** ship a denylist for `CookieStore` (real revocation, adds a small stateful table — contradicts the point of CookieStore) or accept `Unsupported` + rotation. I recommend **Unsupported + rotation + loud warning**.

---

## H14 — Secure-by-default environment

**Problem.** Every prod protection keys off `Environment::Prod`, and the default is `Dev`. A proxy-fronted deploy that forgets `UMBRAL_ENVIRONMENT=prod` runs with the known dev `SECRET_KEY`, no Host validation, and dev error pages — with zero warnings. Related gaps the boot-check catalogue misses: a 1-char `secret_key` passes (only equality-with-default is checked), `allowed_hosts = ["*"]` passes, SQLite-in-prod isn't flagged, pending migrations at boot aren't flagged.

**Proposed change (two options).**
- **(A) Fail-closed default.** In a release binary with `UMBRAL_ENVIRONMENT` unset, refuse to boot (or boot as `Prod`) rather than silently running as `Dev`. Dev keeps working because `cargo run` / debug builds default to `Dev`.
- **(B) Keep `Dev` default, harden the checks.** Add: `secret_key` min-length/entropy floor (not just equality), wildcard-host Warning→Error in prod, SQLite-in-prod Warning, pending-migrations-at-boot Warning.

**Blast radius.** (A) is the big one: any deploy that relied on the implicit `Dev` default and never set the env var will now fail to boot (or run locked-down). That's the *point*, but it will surprise existing deploys on upgrade. (B) is low blast radius — stricter warnings/errors only bite genuinely-misconfigured apps.

**Recommendation.** Do **(B) now** (pure hardening of the check catalogue, low risk, high value) and **(A) as a separate, well-announced change** — probably "release/`--release` builds default to `Prod`, debug builds default to `Dev`, and an explicit env var always wins," which makes the safe thing automatic without a hard boot failure. **Open decision:** do you want (A) at all, and in which form (fail-closed vs. Prod-by-default-in-release)?

---

## H19 — Authorization is default-deny

**Problem.** Authorization is default-**allow**: a route is protected only if the developer attaches `permission_required(...)`. One forgotten layer = a fully open endpoint. There is also no object/row-level scoping (P2 — a model-level perm lets a user act on *any* row; IDOR by design), and the perm layer skips the `is_active` check for non-superusers (P3).

**Proposed change.**
1. **Boot-time audit (low-risk, do first).** At `App::build()`, walk every registered mutating route (POST/PUT/PATCH/DELETE) and log — or in `Prod`, error — any that carry no permission layer and aren't explicitly allow-listed as public. This surfaces the "forgotten annotation" without changing request behavior.
2. **Default-deny router (opt-in first, default later).** A `App::builder().gated_by_construction()` mode where a route with no attached permission is denied (403) unless marked `.public()`. Ship opt-in; consider making it the default in a future major.
3. Fix P3 (`is_active` check for non-superusers) — contained, ship with #1.

**Blast radius.** #1 as a Warning is zero-behavior-change (just logs). #1 as a Prod Error fails boots that have unguarded mutating routes — high blast radius but exactly the holes we want found. #2 default-deny would break every app that relies on implicit-allow — major-version material. #3 is contained.

**Recommendation.** Ship **#1 as a boot Warning (Prod: configurable to Error)** + **#3** now. Ship **#2 as opt-in** (`gated_by_construction()`), default-deny deferred to a major. **Open decision:** should the boot audit be a hard Error in `Prod` by default, or Warning-everywhere with opt-in escalation?

**Decision (recorded):** #3 (P3, `is_active`) shipped in `860eeb18` (`pre_perm_check` + tests). User chose **Warning-everywhere + opt-in gated router**.

**Implementation constraints discovered while building #1/#2 (must be resolved in the design before coding):**

1. **Tower-layer opacity.** Gating today is `.layer(permission_required("perm"))` — an opaque tower layer. `RouteSpec` (`crates/umbral-core/src/routes.rs`) records only `{ path, methods }`, so the framework cannot tell a gated route from an ungated one after assembly. An always-on "warn on every ungated mutating route" therefore **false-positives on every `.layer()`-gated route**, which trains developers to ignore the warning. The audit is only accurate for routes whose permission is *tracked* in metadata.

2. **Core/plugin dependency inversion.** `Routes` lives in `umbral-core`; `permission_required` lives in the `umbral-permissions` *plugin*. Core cannot call the plugin, so a tracked `Routes::require_permission("perm")` that both records the perm AND applies the layer can't live entirely in either crate. The seam that works:
   - `umbral-core`: add `permission: Option<String>` to `RouteSpec` + a public setter on `Routes` (e.g. `.gated_with(perm)` that only records the string — no layer).
   - `umbral-permissions`: a helper / extension that reads the recorded perm and applies the matching `permission_required` layer (or a `Routes` ext-trait `.require_permission(perm)` that calls the core setter *and* layers).
   - `App::build` (core): walk `RouteSpec`; for each mutating method (POST/PUT/PATCH/DELETE) with `permission.is_none()` and not on an explicit public allow-list, emit the Warning.

   With this, the audit is accurate for the tracked API, and the message can tell authors that `.layer()`-only gating isn't boot-visible → prefer the tracked form.

**Recommended next step for #1/#2:** build the `RouteSpec.permission` seam + `.require_permission()` tracked builder first (makes gating introspectable — also feeds OpenAPI), then layer the boot Warning on top. Skipping the seam and warning purely on `RouteSpec` as it exists today would ship a false-positive-heavy audit.

---

## Decisions I need

1. **C3:** build the `TenantMembership` resolver + `UserTenant` model + `current_tenant()` (fail-closed for anon)? [recommend yes]
2. **H7:** `CookieStore` revocation = `Unsupported` + rotation + loud warning, or add a denylist? [recommend Unsupported]
3. **H14:** (B) harden checks now — yes? And do you want (A) secure-default env, as Prod-by-default-in-release-builds or not at all?
4. **H19:** boot audit of ungated mutating routes — Warning-everywhere, or hard Error in Prod? And ship the opt-in default-deny router now or defer?

Once you answer, I'll write each item up as its own focused change (code + tests + docs) the same way the contained fixes shipped.
