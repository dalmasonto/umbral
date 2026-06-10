# Security — auth / session / authorization

Scope: `umbra-auth`, `umbra-sessions`, `umbra-permissions`, `umbra-security`, `umbra-rls`, plus auth-related middleware/settings in `umbra-core`.

---

## AUTH-1 — Flagship `examples/shop` ships with NO CSRF protection and NO security headers
**Severity: high** · **Verified** (`grep -rn "SecurityPlugin\|umbra_security" examples/shop/` → no hits)

- **File:** `examples/shop/src/main.rs:69-193` (plugin list); the missing piece is `plugins/umbra-security/src/lib.rs:105-127`
- **Evidence:** The shop — the documented "real consumer" reference — wires `AuthPlugin`, `SessionsPlugin`, `PermissionsPlugin`, `AdminPlugin`, `RestPlugin`, etc., but never registers `SecurityPlugin`. `SecurityPlugin` is the **only** thing that mounts the CSRF middleware and the `X-Frame-Options`/`nosniff`/`Referrer-Policy` headers.
- **Attack path:** Every state-changing POST in the shop — `/contact`, admin create/update/delete/actions/inline-edit/change-password, REST mutations — is reachable cross-site. With no `X-Frame-Options`, the admin can be framed for clickjacking. The session cookie is `SameSite=Lax`, which is the only backstop and doesn't cover every case.
- **Fix:** Make security-by-default real: auto-register `SecurityPlugin` in `App::builder()` unless explicitly opted out, **or** emit a boot-time `check.rs` error/warning when auth/sessions are present but `SecurityPlugin` is not. Add `SecurityPlugin::new().with_hsts(true)` to the shop wiring as the immediate fix. (Root cause shared with WEB-1 and AUTH-3: security is opt-in and the reference path forgets it.)

## AUTH-2 — Admin write handlers don't self-enforce CSRF; they depend entirely on `SecurityPlugin`
**Severity: high** (compounds AUTH-1) · **Verified** (`grep csrf plugins/umbra-admin/src/handlers/` → only `auth.rs` login)

- **File:** `plugins/umbra-admin/src/handlers/*.rs` (crud, actions, inline_edit, sheet)
- **Evidence:** Only the login handler self-checks CSRF (`auth.rs:139-151`). CRUD create/update/delete/action handlers have no CSRF check of their own. The login handler's comment claims a "redundant check protects the case where someone runs the admin without the security middleware" — but that protection exists for login only, not the write surface.
- **Attack path:** Run the admin without `SecurityPlugin` (the shop's exact config) and the entire CRUD surface is forgeable cross-site, including the `auth_user` change-password endpoint (`handlers/sheet.rs::change_password_handler`).
- **Fix:** Prefer the framework default in AUTH-1 (auto-mount `SecurityPlugin`). Alternatively, have the admin self-check CSRF on every write the way `login_post` does.

## AUTH-3 — RLS plugin ships policies with no mechanism to populate the security context
**Severity: high** · **Verified** (`grep -rn "set_config\|SET LOCAL\|app.user_id" plugins/umbra-rls/src/` → only **reads** of `current_setting('app.user_id')` in policy DDL; nothing writes it)

- **File:** `plugins/umbra-rls/src/lib.rs` (whole plugin); policies reference `current_setting('app.user_id')` at `:21-23`, `:393`
- **Evidence:** Every documented/tested policy filters on `current_setting('app.user_id')::int`. A codebase-wide search for any writer (`set_config`, `SET app`, `after_connect`, a per-request/per-connection hook) returns nothing. There is no middleware and no connection-acquire hook running `SET LOCAL app.user_id = <current user>`.
- **Attack path:** Two failure modes, both bad. (a) If the GUC was never defined at the Postgres level, `current_setting('app.user_id')` raises at query time, breaking every query against RLS tables. (b) If an operator defines a default GUC to avoid the error, it evaluates to that single static value for **every** request — RLS provides no per-user isolation while appearing enabled. Worse, a `SET` (session, not `SET LOCAL`) on a pooled connection persists across requests and leaks one user's identity to the next request reusing that connection. A developer who reads the README believes rows are isolated per user; they are not.
- **Fix:** Provide the missing half of the contract — an `RlsPlugin` middleware/connection hook running `SELECT set_config('app.user_id', $1, true)` (transaction-local) with the authenticated user id at the start of each request's DB work, and document that ambient-pool queries must run inside that transaction. Until it exists, the plugin should fail loudly rather than imply protection it can't deliver.

## AUTH-4 — Privilege escalation: `is_staff`/`is_superuser` editable through the generic admin form
**Severity: medium** (high if a non-superuser holds `change_auth_user`)

- **File:** `plugins/umbra-auth/src/lib.rs:235-237` (model), `examples/shop/src/main.rs:114-119` (admin registration), `plugins/umbra-admin/src/handlers/crud.rs:329-339` (update guard)
- **Evidence:** `AuthUser.is_staff`/`is_superuser` are plain `bool` with no `#[umbra(noform)]`/`#[umbra(noedit)]` guard (contrast `password_hash`, which is `#[umbra(noform)]`). The shop registers `auth_user` in the admin. The update handler gates only on the generic `change_auth_user` permission; there is no field-level restriction limiting `is_superuser`/`is_staff` edits to existing superusers (Django's `ModelAdmin` does exactly this).
- **Attack path:** A staff user granted `change_auth_user` (a routine "user manager" role) edits their own row, sets `is_superuser = true`, and gains full superuser rights — including the permission/RLS bypass paths.
- **Fix:** Restrict `is_superuser`/`is_staff` (and group/permission M2M) edits to requesters who are already superusers, mirroring Django. At minimum document that registering `auth_user` in the admin without this guard is a privilege-escalation vector. (Note: this and WEB-2 are the same class — server-managed fields writable through a generic path.)

## Lower-severity / hardening
- **AUTH-5 (low)** — CSRF cookie missing `Secure` (`umbra-security/src/lib.rs:172`, `umbra-admin/src/auth.rs:201`): set `Path=/; SameSite=Lax` with no `Secure`, so it's sent over plain HTTP. The session cookie correctly sets `Secure`. Add `Secure` to the CSRF cookie.
- **AUTH-6 (low)** — Admin login CSRF compare not constant-time (`auth.rs:143` uses `String ==`), whereas the `SecurityPlugin` middleware correctly uses `subtle::ConstantTimeEq` (`umbra-security/src/lib.rs:280-288`). Route the admin's own check through the same.
- **AUTH-7 (low/info)** — CSRF is double-submit-cookie, not session-bound (`umbra-security/src/lib.rs:19-31`): the token lives in a non-HttpOnly cookie compared against a header/form field, not tied to the session. If an attacker can set a cookie on the victim's domain (subdomain takeover, sibling-host MITM), the pattern can be defeated; `SameSite=Lax` is the real backstop. Consider binding the token to the session, as the module's own deferred note acknowledges.
- **AUTH-8 (info)** — Expiry is enforced but cleanup is lazy: expired sessions deleted only on read (`umbra-sessions/src/lib.rs:245-251`); bearer tokens have **no `expires_at`** (`token.rs:67-92`) and live until revoked. Logout deliberately does not revoke bearer tokens (`auth_routes.rs:333-339`); a password change (`set_password`, `lib.rs:669-685`) does **not** invalidate other sessions/tokens. A stolen bearer token is valid forever; a password reset doesn't lock out an attacker's existing sessions.

## Done well
- **Password hashing correct:** argon2 0.5 with per-password `SaltString::generate(&mut OsRng)`, PHC-encoded, verified via constant-time `Argon2::verify_password` (`umbra-auth/src/lib.rs:529-548`).
- **No user enumeration:** `authenticate` returns the same `InvalidCredentials` for unknown-user and wrong-password (`lib.rs:631-663`); bearer-auth returns `None` identically for missing/bad token; admin login uses one generic message.
- **Session fixation defended:** `login_user_id` destroys the pre-auth session row and mints a fresh token before writing the authenticated session (`umbra-sessions/src/lib.rs:431-474`); the layer skips overwriting the rotated cookie.
- **Tokens/sessions hashed at rest** (sha256/base64); raw value only in cookie/client. **Strong entropy:** session id = uuid v4 (OS CSPRNG, 122 bits); bearer = 32 bytes OsRng; CSRF = 32 bytes getrandom.
- **Session cookie flags right:** `HttpOnly; Secure; SameSite=Lax; Path=/`.
- **Permissions default-closed:** REST `HasPermission` returns Forbidden when extras missing; admin per-model check denies on any DB error.
- **Open-redirect guard** on admin `?next=`. **Secret-key boot check** hard-errors on the insecure dev default in prod.
- **RLS identifier escaping** doubles embedded quotes for table/policy names; verbatim-SQL policy bodies are documented as developer-authored-only.
