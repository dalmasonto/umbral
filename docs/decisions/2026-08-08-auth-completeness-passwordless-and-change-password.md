# umbral-auth completeness: passwordless (magic-link) login and authenticated change-password

| | |
|---|---|
| **Status** | Draft, pre-implementation |
| **Date** | 2026-08-08 |
| **Trackers** | gaps5 #14 (tf#227, magic-link/passwordless), gaps5 #15 (tf#228, change-password) |
| **Touches** | `plugins/umbral-auth` only. No `umbral-core` change, no new crate. |
| **Builds on** | `docs/decisions/2026-06-28-auth-full-surface.md` (the challenge/reset infrastructure both features reuse) |
| **Companions** | `docs/specs/outlines/auth-and-sessions.md` |

## Summary

Two auth-completeness features, both scoped as pure `umbral-auth` additions with zero core changes:

- **gaps5 #15 (change-password)** is *already largely implemented*. The core function and the JSON route exist and work today. This spec scopes the remaining work honestly: session rotation on change, an opt-in revoke-other-devices behavior, a throttle, the form-action surface, and the OpenAPI/route-listing entries. It is a finishing pass, not a build from scratch.
- **gaps5 #14 (magic-link)** is *not implemented at all*, but every primitive it needs already exists (the `AuthChallenge` table, opaque-token generation, hashed-at-rest single-use secrets, the mailer seam, the session-establishing login helper, the email-action throttle, and the anti-enumeration response pattern). This spec designs the new flow on top of those primitives.

The 2026-06-28 decision explicitly deferred both features (`docs/decisions/2026-06-28-auth-full-surface.md` "Out of scope" section, lines 165-170). This spec closes that deferral.

## Shared baseline that already exists (both features reuse it)

These are shipped, tested primitives. Neither feature rebuilds them.

- **`AuthChallenge` model** (`plugins/umbral-auth/src/challenge.rs:23-36`): one table discriminated by `purpose`, storing only `base64(sha256(plaintext))` in `secret_hash`, with `expires_at` (TTL), `attempts` (online brute-force cap), and `used_at` (single-use marker). Single-use and replay protection are inherent to this row shape.
- **Challenge ORM methods** (`challenge.rs:83-211`): `AuthChallenge::issue(user_id, purpose, plaintext, ttl)`, `find_active_for_user`, `find_active_by_secret(plaintext, purpose)`, `is_live()`, `mark_used()`, `bump_attempts()`. All go through the ORM (no raw SQL).
- **Token generation** (`challenge.rs:65-77`): `generate_reset_token()` produces `umbral_` + 43 URL-safe base64 chars (256 bits of OS entropy); `hash_secret(plaintext)` delegates to `token::digest_token` (sha256, URL-safe base64, 43 chars).
- **Mailer seam** (`plugins/umbral-auth/src/mailer.rs`): `AuthMailer` trait, `OutgoingMail`, `MailKind` (`#[non_exhaustive]`, already documents that "magic links" will add a variant, mailer.rs:15), `active_mailer()`, and the dev-default `ConsoleMailer`. Email bodies render from overridable `templates/auth/email/*.{html,txt}` via `umbral::templates::render`.
- **Session establishment** (`plugins/umbral-auth/src/session_user.rs:108-137`): `login_with_request(req_headers, resp_headers, &AuthUser)` writes the session, sets the cookie, fires the session-fixation defense (destroys any anonymous session the request carried, via `umbral_sessions::login_user_id`), and bumps `last_login`. This is exactly what a magic-link consume needs to log a user in.
- **Session rotation primitive** (`plugins/umbral-sessions/src/lib.rs:984-1078`): `login_user_id` and its in-request helper `login_user_id_in_request` rotate the session token in place, destroying the old row (fixation defense) and carrying non-empty session data over. Calling `login_with_request` again for the already-logged-in user reuses this to mint a fresh session id.
- **Revocation primitives**: `umbral_sessions::revoke_user_sessions(user_id_str)` (sessions.rs:643) deletes all of a user's session rows; `AuthToken::objects().filter(...).delete()` revokes bearer tokens. `reset_password` (challenge.rs:494-509) is the precedent for a post-commit "log out everywhere" sweep.
- **Throttle** (`plugins/umbral-auth/src/throttle.rs`): `email_action_throttle_check(ip, email)` (sliding-window, default 5/hour, keyed `ip + "\0" + email`), plus the `login_*` / `register_*` limiters. `AuthPlugin::email_action_throttle(max, window)` tunes it.
- **Anti-enumeration pattern**: `start_password_reset` returns `Ok(())` silently on unknown email (challenge.rs:344-355); the JSON layer answers with a fixed `202 {detail}` envelope (`accepted(...)`, auth_routes.rs:150-153) whose message is identical for known and unknown addresses.
- **Client IP + host base helpers** (auth_routes.rs:128-203): `client_ip(headers)` (trusted-proxy aware) and `reset_url_base(headers)` (host-guard-protected absolute URL construction). The magic-link request handler reuses both.

---

## Feature gaps5 #15: authenticated change-password

### What already exists (precise)

Change-password is not a green field. The following is shipped and working:

1. **Core function** `crate::change_password(user, current, new)` (`challenge.rs:417-442`):
   - verifies `current` against `user.password_hash` via `verify_password_async` (constant-time argon2 verify), returning `AuthError::InvalidCredentials` on mismatch;
   - enforces the strength policy on `new` via `validate_password(..., PasswordContext::new(username, email))`, returning `AuthError::WeakPassword(reasons)`;
   - rotates the stored hash with `hash_password(new)` and an ORM `update_values`.
   - Its doc-comment states deliberately: "Unlike `reset_password`, this does NOT revoke sessions/tokens - the user proved knowledge of the current password, so no compromise is implied."

2. **JSON route** `POST {prefix}/change-password` (`auth_routes.rs:205-275`, handler `change_password_h`, wired at `auth_routes.rs:295-302` in `build_router`):
   - body `{current_password, new_password}`;
   - resolves the caller via `OptionalIdentity` (session cookie OR bearer token), parses the user id as `i64`, loads the active `AuthUser`;
   - maps results to `204` (success), `401 not_authenticated`, `400 invalid_credentials`, `400 weak_password`, `500 server_error`.

So the "old-password verification" requirement of gaps5 #15 is **done**, and a JSON endpoint already exists.

### What is missing

Measured against the gaps5 #15 ask ("HTML and JSON endpoints, with old-password verification, session rotation, and optional revoke-other-devices"):

- **Session rotation on success.** `change_password` and `change_password_h` do not rotate the caller's session id after the hash changes. Best practice (and what OWASP recommends) is to issue a fresh session id on any credential change so a session token captured before the change cannot be replayed. The primitive exists (`login_with_request` / `login_user_id`) but is not called.
- **Optional revoke-other-devices.** There is no way to say "change my password AND sign out my other sessions/tokens, keeping this one." Today change-password revokes nothing; `reset_password` revokes everything (including the current caller). The middle option (revoke all *except the current credential*) does not exist.
- **Form-action (HTML) surface.** `with_form_routes()` covers login/logout/signup/verify-email/resend/password-forgot/password-reset (per the 2026-06-28 decision's revised table) but NOT change-password. An HTML app has no framework-provided POST target for it.
- **Route-listing and OpenAPI entries.** `declared_routes` (auth_routes.rs:329-340) and `openapi_paths` (auth_routes.rs:350-579) enumerate eight routes and omit `change-password`, so it does not appear in the dev 404 route listing or in Swagger UI.
- **Throttle.** `change_password_h` runs no throttle. An authenticated attacker (stolen session) can brute the *current* password field. Low severity (they are already authenticated) but the endpoint should share the `email_action` limiter keyed on the user id.

### Design for the missing pieces

Everything below is additive to the existing `change_password` core function; the function keeps its current no-revoke default so existing callers are unchanged.

#### Core: extend, do not replace

Keep `change_password(user, current, new)` as-is (the simple, no-revoke path). Add a superset:

```rust
/// Options controlling the side effects of a successful change-password.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChangePasswordOptions {
    /// Revoke every OTHER session and bearer token for the user, keeping
    /// only the credential that made this request signed in. Off by default.
    pub revoke_other_sessions: bool,
}

/// Change an authenticated user's password with explicit side-effect control.
/// `keep_session_token` / `keep_bearer_hash` name the caller's CURRENT
/// credential so the revoke sweep can spare it.
pub async fn change_password_with(
    user: &AuthUser,
    current: &str,
    new: &str,
    opts: ChangePasswordOptions,
    keep_session_token: Option<&str>,
    keep_bearer_hash: Option<&str>,
) -> Result<(), AuthError>
```

Behavior:

1. Same current-password verify + strength policy + hash rotation as `change_password` today, in the same `update_values` write. (Refactor: `change_password` becomes `change_password_with(user, current, new, Default::default(), None, None)`.)
2. When `opts.revoke_other_sessions`:
   - delete all `AuthToken` rows for the user WHERE `key_hash != keep_bearer_hash` (or all, when the caller authenticated by session and presented no bearer token);
   - delete all session rows for the user except the one keyed by `keep_session_token`. `umbral_sessions::revoke_user_sessions` deletes *all* sessions, so this needs a sibling that excludes one token: add `umbral_sessions::revoke_user_sessions_except(user_id_str, keep_token)` in umbral-sessions (ORM `delete()` with a `session::ID.ne(hash_token(keep))` predicate). Follow the `reset_password` pattern: run these as best-effort post-write steps, log failures at ERROR, never un-change the password.
3. Session rotation of the *current* session happens in the route layer (below), not in the core function, because it needs the request/response `HeaderMap`s.

Revoke-other-devices is *opt-in per request*, surfaced as a boolean in the request body (see routes), not a plugin-wide switch. Rationale: whether to sign out other devices is a per-action user choice ("change password and sign out everywhere else"), like the checkbox every consumer app shows.

#### Session rotation (route layer)

After a successful core call, the handler rotates the caller's session by calling `login_with_request(&req_headers, resp.headers_mut(), &user)` again. This mints a new session id, destroys the old row (fixation defense), carries session data across, and writes the fresh `Set-Cookie`. A bearer-only caller (no session cookie) simply gets no cookie rotation, which is correct: their bearer token is unchanged and still valid, and revoke-other-devices (if requested) spared it by hash.

#### JSON route (extend the existing handler)

`POST {prefix}/change-password`, body extended to:

```
{ "current_password": "...", "new_password": "...", "revoke_other_sessions": false }
```

`revoke_other_sessions` is optional and defaults to `false` (serde `#[serde(default)]`), so existing clients keep working byte-for-byte. The handler:

1. throttle on `email_action` keyed by `user_id` (after identity resolution); `429` on trip;
2. resolve identity, load user (unchanged from today);
3. extract the caller's current session token (`umbral_sessions::cookie_from_headers(&headers)`) and current bearer hash (`digest_token(parse_bearer_header(&headers))`) to pass as the "keep" credentials;
4. call `change_password_with(...)`;
5. on success, rotate the session (call `login_with_request`) and return `204`;
6. same `401` / `400 invalid_credentials` / `400 weak_password` / `500` arms as today.

Add `change-password` to `declared_routes` and add an `openapi_paths` entry under the `auth` tag (operationId `auth_change_password`, `200/204`, `400`, `401`, `429`).

#### Form-action route (new, under `with_form_routes`)

`POST {form_prefix}/change-password`, form body `current_password`, `new_password`, optional `revoke_other_sessions` checkbox, plus the automatic `{{ csrf_input }}`. Same core call and same session rotation. On failure set a `umbral_sessions::messages` error flash; on success set a success flash; `303 See Other` to the open-redirect-safe target (reuse the existing `with_form_routes` redirect-target rules). The developer owns the page that renders the form and `{{ messages }}`.

#### Config / builder

No new plugin-wide builder flag is required (revoke-other-devices is per-request). The existing `AuthPlugin::email_action_throttle(max, window)` already tunes the limiter this endpoint borrows. Document that the change-password endpoints appear automatically with `with_default_routes()` (JSON) and `with_form_routes()` (HTML).

---

## Feature gaps5 #14: magic-link / passwordless login

### What already exists (precise)

Nothing implements magic-link today. But the reusable primitives listed in "Shared baseline" above are all present, and one hook is pre-wired: `MailKind` is `#[non_exhaustive]` and its doc-comment (mailer.rs:15) already names "magic links" as the reason. The whole feature is new code that composes existing pieces; it introduces no new infrastructure category.

### What is missing (everything, listed)

- a `PURPOSE_MAGIC_LINK` discriminator constant and the flow helpers `start_magic_link` / `consume_magic_link`;
- a `MailKind::MagicLink { login_url }` variant and `templates/auth/email/magic_link.{html,txt}`;
- request + consume routes (JSON and form);
- an opt-in builder `AuthPlugin::with_magic_link()`;
- `declared_routes` / `openapi_paths` entries;
- tests and a doc page.

### Design

#### New purpose + TTL

```rust
pub const PURPOSE_MAGIC_LINK: &str = "magic_link";
const MAGIC_LINK_TTL: Duration = Duration::from_secs(10 * 60); // 10 minutes
```

10 minutes: short enough to bound the replay window, long enough to survive a mail-delivery delay. Shorter than the 1-hour reset link because a magic link grants a full login, not just a password-change opportunity.

#### Token shape and storage

Reuse `generate_reset_token()` (256-bit opaque `umbral_...` token) and store it via `AuthChallenge::issue(user_id, PURPOSE_MAGIC_LINK, token, MAGIC_LINK_TTL)`. Only `sha256(token)` is stored. This inherits, for free:

- **single-use / replay protection**: consume calls `mark_used()` inside the same transaction that establishes the login, so a second click on the same link finds `used_at` set and `find_active_by_secret` returns `None`;
- **TTL expiry**: `is_live()` rejects an expired link;
- **hashed-at-rest**: a DB leak does not yield usable links;
- **collision safety**: 256 bits, looked up globally by `secret_hash`, so no per-user uniqueness constraint is needed (same reasoning as the reset token, decision doc line 63).

#### Core helpers

```rust
/// Issue a magic-link login token for the account owning `email`, render
/// templates/auth/email/magic_link.{html,txt} with the absolute login URL,
/// and send it via the ambient mailer. Silent no-op on unknown email
/// (anti-enumeration), exactly like start_password_reset.
pub async fn start_magic_link(email: &str, login_url_base: &str) -> Result<(), AuthError>;

/// Consume a magic-link token: validate the challenge (live + unused),
/// mark it used, and return the AuthUser so the route can establish a
/// session. Generic AuthError::InvalidChallenge on ANY failure
/// (unknown / expired / already-used token, deleted user).
pub async fn consume_magic_link(token: &str) -> Result<AuthUser, AuthError>;
```

`start_magic_link` mirrors `start_password_reset` line-for-line (silent unknown-email, `AuthChallenge::issue`, render, `active_mailer().send`) with `MailKind::MagicLink { login_url }` and the new templates. `login_url_base` is built by the route from `reset_url_base(headers)`-style logic, pointing at the app's consume page (default path `/auth/magic-link`), so the emailed URL is `{base}?token={token}`.

`consume_magic_link` mirrors the token half of `reset_password`: `find_active_by_secret(token, PURPOSE_MAGIC_LINK)`, load the user by `challenge.user_id.id()`, then in one `transaction` stamp `used_at` (so a replay after this point fails even under concurrency). It returns the `AuthUser` rather than logging in internally, because establishing the session needs the request/response headers, which live in the route. Consume does NOT revoke other sessions (logging in is not a compromise signal).

Enforcement note: when `require_verified_email()` is active, `consume_magic_link` treats a successful magic-link click as sufficient proof of email control and MAY stamp `email_verified_at = now` if null, in the same transaction. This is consistent (the user just proved they receive mail at that address). Gate this behind the existing `verified_email_required()` check.

#### Routes

**Request (send the link).** `POST {prefix}/magic-link/request`, JSON `{email}`:

1. `email_action_throttle_check(client_ip(&headers), &email)`; `429` on trip (stops email-bombing and link-harvesting);
2. `start_magic_link(&email, &base)` best-effort, ignoring the result;
3. always `202` with the fixed anti-enumeration `detail` (reuse `accepted(...)`), identical for known and unknown addresses.

**Consume (log in).** The link target. Two concerns pull in opposite directions:

- A magic link is clicked from an email client via `GET`, so a `GET` consume is the natural UX.
- A `GET` that mutates state (marks the token used, logs the user in) is a side-effecting GET, and link-scanning/prefetching (corporate mail security scanners, chat unfurlers) can burn the token before the human clicks.

Resolution, two-step, matching how mature stacks handle this:

- `GET {prefix}/magic-link` with `?token=...` does NOT consume. It renders (for the form surface) or returns (for JSON) a minimal "confirm login" step whose form `POST`s the token back. This keeps the emailed `GET` idempotent, so a prefetch does not spend the token.
- `POST {prefix}/magic-link` with `{token}` (JSON) or form body calls `consume_magic_link`, and on success calls `login_with_request(&headers, resp.headers_mut(), &user)` to establish the session (+ optional `AuthToken::create_for(&user, "magic-link")` in the JSON response so CLI/mobile clients get a bearer token, mirroring `/login`). JSON returns `{user, token}` + `Set-Cookie` and `200`; the form surface `303`-redirects to the safe success target with a success flash. Failure: JSON `400 invalid_or_expired` (generic); form flash + `303` to error target.

For pure-JSON API clients that want a single call and accept the prefetch tradeoff, document that they can `POST` the token directly (they control their own link handling), so the two-step GET is really a browser-safety affordance, not a hard gate.

Throttle the consume `POST` on `email_action` keyed by client IP (there is no email in the consume body) to blunt token-guessing, though the 256-bit space already makes guessing infeasible; the throttle is defense in depth against a flood.

#### Opt-in builder

```rust
/// Enable the passwordless magic-link login routes. Off by default:
/// passwordless changes the security posture (anyone with mail access to
/// the address can sign in), so the app opts in explicitly.
pub fn with_magic_link(mut self) -> Self { self.magic_link_enabled = true; self }
```

Off by default, unlike verify/reset (which are always mounted). Passwordless login is a deliberate posture choice, so it is gated behind an explicit builder call. When enabled, `routes()` mounts the request/consume routes at both bare and trailing-slash forms (matching the existing gaps3 #11 pattern) and adds them to `declared_routes` / `openapi_paths`. When disabled, none of the routes exist and the `magic_link` challenge purpose is never issued. Store the flag in an ambient the same way `verified_email_required()` is stored (`plugins/umbral-auth/src/lib.rs:771-775`).

#### Templates + mailer

Add `templates/auth/email/magic_link.html` and `.txt`, overridable by the app like the other auth email templates. Add `MailKind::MagicLink { login_url }`; because `MailKind` is `#[non_exhaustive]`, existing `AuthMailer` impls with a `_ =>` arm keep compiling. `ConsoleMailer` prints the link in dev.

---

## Security summary (both features)

- **Replay / single-use.** Both magic-link consume and the existing reset/verify flows stamp `used_at` inside the transaction that performs the privileged action, so a second use of the same secret fails. `bump_attempts` + the attempt cap remain available if a code form is ever added; the opaque 256-bit token needs no attempt cap.
- **Throttling.** Magic-link request and consume, and change-password, all route through the existing sliding-window `email_action` limiter (default 5/hour) so email-bombing, link-harvesting, and current-password brute force are bounded at the edge, on top of the per-challenge single-use guarantee. Tunable via `AuthPlugin::email_action_throttle`.
- **Timing safety.** Current-password verification uses `verify_password_async` (argon2, constant-time). Magic-link and reset tokens are looked up by `sha256` digest, never by comparing plaintext in a WHERE clause, and the digest is deterministic so the lookup is a single indexed equality.
- **Anti-enumeration.** `magic-link/request` always answers `202` with a fixed message; `start_magic_link` is a silent no-op for unknown emails, matching `start_password_reset`. Consume returns a single generic error for every failure arm.
- **Session rotation.** Change-password rotates the caller's session id via `login_with_request`; magic-link consume establishes a fresh session the same way, and its underlying `login_user_id` destroys any anonymous session the request carried (fixation defense).
- **Revoke-other-devices** (change-password, opt-in) spares the caller's current credential by session token and bearer hash, then best-effort deletes the rest, logging failures without un-changing the password (the `reset_password` precedent).
- **Host-header safety.** Absolute URLs in both the reset and magic-link mails are built from the `Host` header, which the production host-guard layer (`crates/umbral-core/src/app.rs` Phase 5.95) has already validated against `settings.allowed_hosts` before any handler runs (see the extended rationale at auth_routes.rs:169-188).
- **CSRF.** Every HTML POST form (change-password form, magic-link confirm form) carries `{{ csrf_input }}`, validated by the automatic CSRF middleware.
- **All DB access via the ORM.** No new raw SQL; the one new sessions helper (`revoke_user_sessions_except`) is an ORM `delete()` with a `ne` predicate.

## Testing

Behavioral tests (real rows, real route, read the object graph back), with a recording `TestMailer` to extract the emitted magic-link token, mirroring the existing `plugins/umbral-auth/tests/*` harness (shared ambient pool + test lock), SQLite:

- **change-password**: success rotates the hash and the session id (old cookie no longer resolves, new cookie does); wrong current password -> `400 invalid_credentials`, hash unchanged; weak new password -> `400 weak_password`; unauthenticated -> `401`; `revoke_other_sessions: true` signs out a second session/token for the same user while the caller's own session/token still work; throttle trips after the budget.
- **magic-link**: request emits a mail whose token consumes into a live session; second consume of the same token fails (single-use); expired token fails; unknown email still answers `202` (enumeration); consume establishes a session (`/me` returns the user) and, when `require_verified_email` is on, stamps `email_verified_at`; the two-step `GET` does not spend the token; `with_magic_link` off means the routes 404.

## Documentation

Per "ship a feature, ship its doc page", add MDX under `documentation/docs/v0.0.1/auth/`:

- `change-password.mdx`: purpose, one JSON example and one form example, the `revoke_other_sessions` option, link back to this note.
- `magic-link.mdx`: purpose, the `with_magic_link()` opt-in, request/consume example, the two-step-GET rationale, link back to this note.

Each is the smallest useful slice, not a rewrite of this spec.

## Out of scope (unchanged from the 2026-06-28 deferral)

- TOTP / 2FA / WebAuthn passkeys (gaps5 #10).
- Full session/device inventory + admin UI (gaps5 #13); revoke-other-devices here is the per-action primitive, not the management console.
- Durable mail retry queue (gaps5 #53).
- Magic-link as a 6-digit code variant (this spec ships the tokenized-link form only; a code form could reuse the same `AuthChallenge` attempt-cap machinery later).
