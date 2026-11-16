# review_4 · umbral-permissions

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 3461 LOC (`plugins/umbral-permissions`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 0 medium · 2 low · 1 info — 3 new, 0 known |

## Summary

- `umbral-permissions` ships the RBAC data model (ContentType/Permission/Group/UserPermission/UserGroup, string-keyed on `user_id`), the query layer (`has_perm`/`user_perms`/object permissions), the `permission_required` tower layer, the REST bridge (`WithPermissions`/`HasPermission`), and `*_gated` route builders that record gates for the boot audit.
- The audit_2 P-series fixes all hold at this commit; specifically, the routed lead is answered: the compensation for the raw `current_user_id_str` primitive holds at every entry point traced, so a String/UUID-keyed user is neither locked out nor admitted without a codename check (see Clean areas).
- Fail-closed direction is preserved on every error path read; the residue is operator visibility — three sibling probes still discard their errors silently where the middleware's own `has_perm` path warns (PERM-1).
- The crate's rustdoc has drifted from the code in several load-bearing spots (i64-era signature claims, a shipped control still listed as deferred, a removed bootstrap block still documented) — cheap to fix, currently misleading (PERM-2).
- No raw SQL, no `unsafe`, no reachable panics; tests cover the middleware, REST, non-i64 PKs, and the gated builders.

## Findings

### PERM-1 · LOW · correctness/observability (drift from the P5 pattern)
**Three DB probes still swallow their errors silently (`_ => None`, `.ok().flatten()…unwrap_or((false, false))`, `if let Ok(perms)`), while the sibling `has_perm` path warns and denies — so transient failures that deny users are invisible to operators.**

- **Where**: `plugins/umbral-permissions/src/middleware.rs:285-301` (`auth_user_flags`), `src/rest.rs:109-129` (flags probe), `src/rest.rs:151-159` (perm-set load); the pattern to match: `middleware.rs:231-241`.
- **Evidence**:
  ```rust
  Ok(Some(u)) => Some((u.is_active, u.is_superuser)),
  _ => None,
  ```
  ```rust
  .ok()
  .flatten()
  .map(|u| (u.is_active, u.is_superuser && u.is_active))
  // The id IS an i64 but the row is gone — the user vanished between
  // authenticate and now. Deny-by-default, as before.
  .unwrap_or((false, false)),
  ```
  ```rust
  if let Ok(perms) = crate::user_perms(&identity.user_id).await {
  ```
  vs the shipped P5 shape:
  ```rust
  Err(e) => {
      tracing::warn!(
          error = %e,
          perm = %config.perm,
          "umbral-permissions: has_perm lookup failed; denying (fail-closed)"
      );
  ```
- **Impact** (traced): a brief DB error during `WithPermissions::authenticate` is indistinguishable from "row gone"; the `.unwrap_or((false, false))` path writes `is_active = false` and the request 403s with zero log output. The perm-set load has the same shape (a transient error drops `permissions` → `HasPermission` 403s), and `auth_user_flags`' `_ => None` swallows both `Ok(None)` and `Err` — the downstream behaviour differs between those two cases, and neither is recorded. Direction stays fail-closed throughout (not an auth bypass), but a wave of 403s after a pg failover produces no diagnostics pointing at the DB, and the asymmetry is the "`.ok()` that silently discards a secondary error" class CLAUDE.md names a workaround. LOW, not MEDIUM: the security posture is intact; the cost is debuggability.
- **Fix**: log a `warn!` on the error arm of each probe (`Err(e) => { tracing::warn!(error = %e, …); None }`), keeping every deny. One shared helper would keep the three from re-drifting.

### PERM-2 · LOW · docs drift
**Rustdoc/code mismatches from the i64→String PK migration and the P-series changes: `has_perm` is documented as taking `i64`, the shipped `permission_required` layer is still listed under "Deferred", and a removed `CREATE TABLE IF NOT EXISTS` bootstrap is still documented.**

- **Where**: `src/lib.rs:37-40`, `src/perm.rs:5` (`has_perm` takes `user_id: i64`); `src/lib.rs:61-66` (deferred list including `permission_required`); `src/lib.rs:154-162` vs the in-function comment `188-197`; `src/models.rs:203-207` vs `232`, and the example at `models.rs:44-50`.
- **Evidence**:
  ```
  //! The clean resolution: `has_perm` / `user_perms` are free functions in
  //! this crate that take `user_id: i64`. …
  ```
  actual signature: `pub async fn has_perm(user_id: &str, perm: &str) -> Result<bool, PermError>` (`perm.rs:110`); `//! has_perm takes `user_id: i64` rather than a `&impl UserModel` receiver` (`perm.rs:5`). The deferred list still says "`permission_required(perm)` tower layer / extractor (gap 26 follow-on)" although it ships (`middleware.rs:307+`, used by `rest.rs`). `lib.rs:157-161` says "we emit `CREATE TABLE IF NOT EXISTS` for each permissions table" while `188-197` records that block is gone now that the ORM's `get_or_create` lets the code skip-with-grace. `models.rs:205-207` says "`user_id` is a plain `i64`" directly above `pub user_id: String` (`232`), and the example at `models.rs:44-50` repeats the same `has_perm(&user.id().to_string(), …)` line three times.
- **Impact**: a reader porting a UUID-keyed app concludes `has_perm` needs an i64 (it takes `&str`), or that the tower layer doesn't exist yet and hand-wires checks — the exact drift audit_2 P1 chased. Each is a small tax; together they make the crate's doc surface actively wrong about its own API. Fix: one rustdoc sweep alongside the i64→String change.

### PERM-3 · INFO · hygiene
- **Where**: `plugins/umbral-permissions/src/lib.rs:180` (`async fn ensure_standard_permissions(_pool: &umbral::db::DbPool)`).
- The parameter is unused (the body resolves the ambient pool) while both callers pass a concrete `DbPool` (`148`, `176-177`). Behaviour is identical in single-pool apps, but the `_` hides that the explicit-pool contract the call sites imply is not honoured — drop the parameter or use it.

## Strengths

- PK-agnostic end to end: perm rows store `user_id` as TEXT (capped, indexed — `models.rs:222-232`), and every entry point resolves via the raw-string primitive — `current_user_id_str` in the middleware (`middleware.rs:208-211`), `WithPermissions`/`HasPermission` on the REST side (`rest.rs:109-129`, `190-243`).
- The three-state REST logic (gaps3 #59) is implemented carefully and explains itself in the code (`rest.rs:90-134`): unknowable flags assert nothing, explicit `false` denies, and the codename check governs — with `tests/non_i64_user_pk.rs` as the regression suite.
- Precedence is pure and tested: `pre_perm_check` makes deactivation outrank superuser, with the enum and unit tests (`middleware.rs:256-278`).
- Object-level authorization primitive (audit_2 P2) ships with a superuser variant, list filtering, and cleanup helpers (`has_object_perm`/`objects_with_perm`/`revoke_object_permissions_for`).
- `*_gated` builders record the permission into the RouteSpec so the boot audit (and `AppBuilder::deny_ungated_mutations`) can see hand-wired vs. recorded gates (`routes_ext.rs:1-11`; `crates/umbral-core/src/app.rs:986`, `1469-1477`).
- Fail-closed everywhere with fetch caps: `PERM_FETCH_LIMIT = 10_000` on the perm/group fetches (`perm.rs:48`, `269`, `296`).

## Clean areas checked

- **Compensation for the raw `current_user_id_str` primitive** (routed lead): verified held at every entry point read — the middleware denies anonymous/session-error before any check (`middleware.rs:208-211`), REST `HasPermission::check` returns `Unauthenticated` on no identity and `Forbidden` on absent/false `is_active` (`rest.rs:190-243`), and the `*_gated` builders layer the same `permission_required` middleware. No entry point trusts the primitive without a per-request permission lookup.
- **Default-closed posture** (historic review_1 item): all error paths read deny; the superuser bypass is gated *after* the inactive check, and a custom user model never gets an implicit allow.
- **Custom-PK deactivation fall-through**: documented by design (`documentation/docs/v0.0.1/plugins/permissions.mdx:238-241`) and covered by `tests/non_i64_user_pk.rs` — a documented limitation, not a defect.
- **Raw SQL / unsafe / panics**: `grep sqlx::query src/` empty; zero `unsafe`; no `unwrap`/`expect` on request paths read.
- **Tests**: `integration.rs`, `middleware.rs`, `non_i64_user_pk.rs`, `routes_ext.rs` — behavioural, exercised through the public entry points.

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2 P1` — default-allow gating surface `[fixed/verified held]` — boot warn for ungated mutating routes + opt-in hard fail shipped (`crates/umbral-core/src/app.rs:986`, `1469-1477`); `P2` (object permissions) `[fixed/verified held]`; `P3` (inactive re-check) `[fixed/verified held]` (`pre_perm_check`); `P4` (i64-only layer) `[fixed/verified held]` (`current_user_id_str`); `P5` (silent DB error) `[fixed/verified held]` (`middleware.rs:231-241`); `P6` (unbounded fetches) `[fixed/verified held]` (`PERM_FETCH_LIMIT`).
- `gaps3 #59` — REST three-state flags `[fixed/verified held]`
- `review_1 WEB-7` — admin bulk actions / FK pickers permission checks `[fixed/verified held]`
- `review_1` — "permissions are default-closed (REST and admin fail closed on error)" `[fixed/verified held]`

## Out-of-scope notes

- `plugins/umbral-sessions` — `current_user_id_str` is the session primitive this crate consumes; its error semantics (anonymous vs. store error) are that crate's contract, verified satisfactory from this side — do not double-report (see the sessions review).
- `plugins/umbral-auth` — `is_active`/`is_superuser` semantics live on `AuthUser`; the custom-user-model fall-through is a deliberate cross-crate limitation documented in permissions.mdx.
