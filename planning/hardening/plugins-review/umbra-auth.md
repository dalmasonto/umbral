# Holistic review — `umbral-auth`

Read-only review, 2026-06-16. Scope: `plugins/umbral-auth/src/**` + `plugins/umbral-auth/tests/**`, against the expected built-in authentication surface. Cross-referenced against `planning/hardening/reviews/{race-conditions,security}.md` and `docs-audit/migrations-auth-cli-backends.md`; already-filed items are marked **already #N** and not re-counted. NEW findings (no number yet) are marked **NEW**. Severity per the code-review skill: Critical / Important / Minor / Nit.

## Verdict

Solid, well-shaped plugin that proves the M7 contract cleanly. The user-model abstraction (`UserModel` + polymorphic `PrimaryKey`) is genuinely good, password hashing is correct, the bearer-token and session-auth surfaces are coherent, and the test suite is the strongest of any plugin reviewed so far (behavioral round-trips, custom-user and UUID-user coverage). **Completeness is the gap, not correctness:** the plugin covers the *authentication* core but explicitly defers most of the *account-lifecycle* surface (password reset, password change, email verification, lockout/throttle, permissions - the latter lives in the separate `umbral-permissions` plugin). What ships is correct; what's missing is breadth.

**Completeness one-liner:** authentication core is complete and correct; account-lifecycle (reset/change/verify) and abuse-defense (lockout/throttle) are deliberately absent, and the `Identity` boundary drops `is_superuser`.

**Worst finding:** **NEW (Important)** — `Identity` carries only `is_staff`, never `is_superuser`; every auth path in this plugin (`SessionAuthentication`, `BearerAuthentication`, `identity_from_*`) loses the superuser bit at the REST boundary, so a superuser authenticating via REST looks like a plain staff/non-staff user to any permission that reads the `Identity` directly. `session_user.rs:167`, `bearer_auth.rs:106`, `extractors.rs:120,137`.

## Completeness (vs an expected built-in auth surface)

| Capability | Status | Notes |
|---|---|---|
| Custom user model | **Complete** | `UserModel` trait, `AuthPlugin<U>` generic, polymorphic `PrimaryKey`. Cleaner than a settings-driven swappable-user-model approach. Tested for `i64` + `uuid::Uuid` (`tests/uuid_user.rs`, `tests/custom_user.rs`). |
| `User` / `OptionalUser` extractors | **Complete** | `session_user.rs:203,216`. 401 / fail-open `None`. |
| `LoggedIn<U>` / login-required | **Complete** | Extractor + tower layer (`login_required.rs`), API-401 and HTML-302 shapes. Good. |
| Password hashing (argon2) | **Complete** | `hash_password`/`verify_password`, random per-password salt, constant-time verify, identical `InvalidCredentials` for unknown-user vs wrong-password. |
| `createsuperuser` | **Complete** | `CreateSuperuserCommand`, interactive + `--noinput` + `UMBRAL_SUPERUSER_PASSWORD`, no-echo prompt, confirm-match. |
| `with_user_in_templates` | **Complete** | Builder + `wrap_router` + `user_context_layer`; recursive relation expansion (gap2 #14). The docstring-to-method contract the CLAUDE.md cites as the canonical "fix don't patch" example is honoured — the method exists and the middleware mounts. |
| Default `/auth` routes | **Complete (AuthUser-only)** | register / login / logout / me, OpenAPI paths, route specs. Deliberately gated to `AuthPlugin<AuthUser>` at the type level. |
| Bearer tokens | **Complete** | Hash-at-rest, `umbral_` prefix, per-user named tokens, `ON DELETE CASCADE`, best-effort `last_used_at`. Standard hashed-token-API shape. |
| **Password validators** | **MISSING** | No `validate_password` / min-length / common-password / numeric checks. `register` and `createsuperuser` accept any non-empty string. A configurable password-validator suite is a standard baseline. Not stubbed - simply absent. |
| **Password reset flow** | **MISSING (deferred)** | `auth_routes.rs:30` documents it as "couples to a mail crate; lands as its own plugin." No token model, no endpoints. |
| **Password change flow** | **MISSING** | `set_password<U>` helper exists (`lib.rs:669`) but there is no HTTP endpoint and no "old password required" check. A built-in password-change view is the expected surface. |
| **Email verification on register** | **MISSING (deferred)** | `auth_routes.rs:31` "workflow varies per app." |
| **Account lockout / login throttle** | **MISSING (deferred)** | `auth_routes.rs:32` "production hardening; wrong layer." No failed-attempt counter, no `axes`-equivalent. The `login` handler (`auth_routes.rs:297`) has no rate limit — unbounded credential-stuffing surface. |
| **Remember-me** | **MISSING** | Cookie TTL is fixed at `DEFAULT_TTL_SECONDS`; no per-login "keep me signed in" toggle that extends/shortens `expires_at`. |
| **Permissions / groups** | **Out of scope (separate plugin)** | `umbral-permissions` owns this; `lib.rs:56` notes the deferral. Correct architecturally. |
| `last_login` bump | **Complete** | `login_with_request` best-effort updates it. |
| Session fixation defense | **Complete** | Delegated to `umbral_sessions::login_user_id` (destroys anon session, mints fresh token). |

No `todo!()`, no `unimplemented!()`, no `// TODO`/`// FIXME` in `src/`. The "deliberately missing" items in `auth_routes.rs:28-34` are honestly documented as deferred, not faked.

## Findings (net-new)

### Important

1. **NEW — `Identity` drops `is_superuser` at the auth boundary.** `session_user.rs:167`, `bearer_auth.rs:106`, `extractors.rs:120,137`. Every authenticator builds `Identity::user(...).with_staff(user.is_staff)` — there is no `.with_superuser(...)` and `Identity` (`umbral-rest/src/auth.rs:55-69`) has no superuser field. So a superuser arriving via REST is indistinguishable from a non-superuser staff member at the `Identity` level. **Bounded, not a bypass:** `umbral-permissions`' REST permission re-loads the `AuthUser` row and reads `is_superuser` directly (`umbral-permissions/src/rest.rs`, per `reviews/security.md`), so the superuser path still works *for that consumer*. But a third-party permission that trusts the `Identity` (the documented extension point) cannot see superuser. The contract is incomplete. **Fix:** add `is_superuser` to `Identity` + `with_superuser`, populate it in all four auth paths. Touches `umbral-rest` (the field) and `umbral-auth` (the populate). → **NEW gap (file)**.

2. **NEW — no password-strength validation anywhere.** `lib.rs:565` (`create_user`), `auth_routes.rs:266` (`register`), `lib.rs:804` (`resolve_password`). The only check is `is_empty()`. A user can register with password `"a"`. A configurable password-validator suite (min length, common-password list, numeric-only, user-attribute-similarity) is a security baseline, not a nicety. **Fix:** a `PasswordValidator` trait + a default `MinLength(8)` + common-password set, run in `create_user`/`register`/`createsuperuser`/`set_password`. → **NEW gap (file)**.

3. **NEW — `register`/login endpoints have no throttle; credential stuffing is unbounded.** `auth_routes.rs:297` (`login`), `:266` (`register`). Cross-refs the deferral note at `auth_routes.rs:32`, but the *deferral* is the finding: a framework that ships a built-in `/api/auth/login` with zero rate limiting ships a credential-stuffing endpoint by default. Even a coarse per-IP counter (or a documented "mount a throttle layer") closes it. **Fix:** ship a throttle middleware or, at minimum, a boot-time `check.rs` warning when `with_default_routes()` is mounted without one. → **NEW gap (file)**.

4. **already #75 — `password_hash` is serde-`Serialize`d, guarded only by the block-list.** `lib.rs:222,233-234`. `AuthUser` derives `Serialize` with no `#[serde(skip_serializing)]` on `password_hash`; only `auth_user` being in `DEFAULT_BLOCKED_TABLES` keeps it off the wire. One `.expose(["auth_user"])` without `.hide("password_hash")` dumps argon2 hashes. The `#[umbral(noform)]` attribute keeps it off *forms* but not off *serialization*. Already filed.

5. **already #75 — argon2 params are implicit `Argon2::default()`.** `lib.rs:531,543`. Currently meets OWASP but unpinned; a crate bump could silently regress cost. Already filed.

### Minor

6. **NEW — `register` classifies UNIQUE violations by substring-matching the Display string.** `auth_routes.rs:277-282`: `format!("{e}").to_lowercase().contains("unique")` → 409, else 400. The `AuthError::Write(WriteError::UniqueViolation{..})` variant is already structured (`lib.rs:482`); matching on it directly is correct, the string-grep is fragile (a localized/reworded sqlx message silently becomes a 400). **Fix:** match the `WriteError` variant, not its text.

7. **NEW — `/me` 500s on a custom-user-shaped session instead of 401, and the comment admits it.** `auth_routes.rs:358-364`: when `id.user_id` doesn't `parse::<i64>()`, it returns 401 "session user id does not match the AuthUser PK shape." That's defensible, but the route is only ever mounted on `AuthPlugin<AuthUser>`, so a non-i64 id here means a *different* AuthUser-keyed plugin wrote the session — an operational misconfiguration surfaced to the end user as an auth failure. Minor; the route is AuthUser-gated so the path is mostly unreachable. Noting for completeness.

8. **NEW — `login_with_request` swallows a serialization failure into `Value::Null`.** `session_user.rs:120`: `serde_json::to_value(chrono::Utc::now()).unwrap_or(serde_json::Value::Null)`. `Utc::now()` serialization never legitimately fails, so this is a `unwrap_or_default()`-flavored mask (the CLAUDE.md anti-pattern) that would write `last_login = NULL` on the impossible path. Cosmetic — but `.expect("Utc::now always serializes")` documents the invariant honestly instead of hiding a NULL write. Nit-adjacent.

### Nit

9. **NEW — `pk_json_key` / `json_value_to_pk_string` duplicated from `umbral-core::orm::dynamic`.** `session_user.rs:486,498`, by the author's own admission in the doc-comment (`:479-485`: "Mirrors the `pk_json_key` helper in `umbral-core::orm::dynamic` — kept local"). Ties into the dedup theme in **backlog #77**; fold in.

10. **NEW — `urlencoded` in `login_required.rs:135` is a hand-rolled 7-char percent-encoder.** Covers the query-string metacharacters but not the general set (e.g. `#`, non-ASCII). For a `?next=<uri>` redirect it's adequate (the uri is server-controlled), but `percent-encoding` (already a transitive dep) would be more correct and less surprising. Minor.

## Tests

Coverage is strong and behavioral — the best of the plugins reviewed. `tests/integration.rs` does real round-trips (create → authenticate → set_password → re-authenticate), `tests/custom_user.rs` + `tests/uuid_user.rs` exercise the generic `UserModel` path against a non-`AuthUser` and a UUID PK (proving the polymorphic-PK claim isn't vaporware), and `tests/login_required.rs` drives the tower layer end-to-end through a real router (401 API / 302 HTML / config-flows-into-extractor / anonymous-rejected / deref / serialize). `createsuperuser` is dispatch-tested incl. the `--noinput`-without-password error path.

**Gaps in coverage (all NEW):**
- No test that an inactive user is rejected by the **session/bearer** read path (`current_user`/`resolve_identity` filter `is_active=true`); `authenticate` has the inactive test but the extractor/identity paths don't. Worth one round-trip: deactivate a logged-in user, assert `current_user` → `None`.
- No test for the `Identity` `is_staff` flag actually flowing through `SessionAuthentication`/`BearerAuthentication` into a permission decision (the staff bit is set but never asserted end-to-end here).
- No test for bearer-token `revoke()` → subsequent lookup returns anonymous (the lifecycle's last step). `token.rs` unit-tests generation/digest but not the revoke round-trip.
- No test for `with_user_in_templates` relation-expansion depth cap / cycle detection (the `expand_relations` recursion is non-trivial and untested at the plugin level).
- No negative test for the `register` 409-on-duplicate path (the substring classifier in finding #6 is untested).
