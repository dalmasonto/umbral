# umbral-auth: full auth surface (verification, password reset, template pages)

| | |
|---|---|
| **Status** | Approved, pre-implementation |
| **Date** | 2026-06-28 |
| **Touches** | `plugins/umbral-auth`, `crates/umbral-core` (one new ambient), `plugins/umbral-rest` (publishes base path), `plugins/umbral-email` (optional adapter), `documentation/docs/v0.0.1/auth/` |
| **Companions** | `docs/specs/outlines/auth-and-sessions.md`, `docs/specs/outlines/email.md`, `docs/specs/02-plugin-contract.md` |

## Purpose

Grow `umbral-auth` from a JSON-only `register/login/logout/me` surface into a complete, batteries-included authentication system that exposes the *same* auth flows two ways — as a server-rendered Jinja (MiniJinja) page surface for template apps, and as a JSON surface that shows up under the REST/OpenAPI plugins — sharing one core of logic underneath. Adds email verification, password reset/forgot, a single reusable `logout`, and a pluggable email seam.

This document is the approved design. Implementation follows via the writing-plans flow.

## What exists today (baseline)

- `AuthUser` model: `id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login`. No verification column.
- `AuthToken` model: hashed-at-rest opaque bearer tokens (`plugins/umbral-auth/src/token.rs`). The precedent for hashing secrets at rest.
- JSON routes (`auth_routes.rs`), mounted by `AuthPlugin::with_default_routes()` at a hardcoded `/api/auth`: `POST /register`, `POST /login`, `POST /logout`, `GET /me`. Login returns both a Set-Cookie and a bearer token; logout calls `umbral_sessions::logout` directly.
- `AuthPlugin` builder: `with_default_routes[_at]`, `with_user_in_templates`, password-policy and throttle knobs. Secure-by-default password validation + login/register throttling already in place.
- Templating (`umbral::templates::render`, MiniJinja), flash messages (`umbral_sessions::messages::Messages`), and the `umbral-email` plugin (`umbral_email::send`, console backend in dev) all already exist and are reused, not rebuilt.
- The website's `accounts` plugin has app-specific login/signup HTML — NOT framework-level. This work promotes that capability into the framework so every app gets it.

## Decisions (locked)

1. **Token form — hybrid.** Email verification uses a 6-digit numeric code the user types; password reset uses a tokenized link the user clicks. Both hashed at rest, single-use, expiring (code 15 min, link 1 h).
2. **Surface — two opt-in builders.** `with_default_routes()` (JSON, extended) and `with_form_routes()` (form-action POST endpoints that redirect; the developer owns the pages — see the revised section below). Independent; both call the same core functions.
3. **Enforcement — opt-in.** `email_verified_at` is always tracked and the verify endpoints always exist, but login is blocked only when the app calls `require_verified_email()`. Backward-compatible.
4. **Logout — reusable function + route.** A single `umbral_auth::logout(req_headers, resp_headers)` used by both surfaces and callable from any handler.
5. **Base path — auto-follow + override.** JSON routes mount under the REST plugin's base path when present (`{rest_base}/auth`), `/api/auth` otherwise, via a decoupled core ambient. `with_default_routes_at(prefix)` always wins.

## Data model

### `AuthUser` — add one column

```
email_verified_at: Option<DateTime<Utc>>   // NULL = unverified
```

Nullable, so the autodetected migration backfills existing rows to `NULL` with no data-migration step. Existing apps are unaffected until they opt into `require_verified_email()`. Apps apply it with the normal `makemigrations` → `migrate` loop; the DB is never wiped.

### `AuthChallenge` — new model, one table for both flows

```rust
pub struct AuthChallenge {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub user_id: ForeignKey<AuthUser>,
    pub purpose: String,                 // choices: "email_verify" | "password_reset"
    #[umbral(max_length = 64)]
    pub secret_hash: String,             // base64(sha256(code|token))
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,                   // online brute-force guard for codes
    pub used_at: Option<DateTime<Utc>>,  // single-use marker
    pub created_at: DateTime<Utc>,
}
```

Registered alongside `AuthToken` (only when `U = AuthUser`, same `TypeId` guard the plugin already uses in `models()`).

- **email_verify**: 6 random digits. Hash compared after lookup by `(user_id, purpose, unused, unexpired)`. `attempts` increments per wrong guess; at 5 the row is invalidated. 6 digits is a 10^6 space — the online defenses (attempt cap + 15-min TTL + per-endpoint throttle), not the hash, are what make guessing infeasible. The at-rest hash protects a DB-leak scenario.
- **password_reset**: 256-bit opaque token, `umbral_`-style, looked up globally by `secret_hash`. Collision on 256 bits is impossible, so no global UNIQUE is required (and none is added, because 6-digit codes *would* collide across users).
- Both: single-use (`used_at`), TTL (`expires_at`). Expired/used rows are pruned by an optional management command (`umbral auth prune-challenges`), noted as a follow-up, not a gate.

## Shared core (the seam both surfaces call)

New/promoted public functions in `umbral-auth` (`create_user`, `authenticate`, `login_with_request` already exist):

- `pub async fn logout(req: &HeaderMap, resp: &mut HeaderMap) -> Result<(), AuthError>` — wraps `umbral_sessions::logout`. The single reusable command. Both surfaces and any custom handler call it.
- `pub async fn start_email_verification(user: &AuthUser) -> Result<(), AuthError>` — generates a code, writes an `AuthChallenge`, renders the email from a shipped template, sends it through the configured mailer.
- `pub async fn verify_email(email: &str, code: &str) -> Result<(), AuthError>` — looks up the user's active challenge, validates (TTL / attempts / single-use), sets `email_verified_at = now`, marks the challenge used. Generic error on any failure (no enumeration).
- `pub async fn start_password_reset(email: &str) -> Result<(), AuthError>` — silent no-op on unknown email (no enumeration); writes an `AuthChallenge`, emails the reset link.
- `pub async fn reset_password(token: &str, new_password: &str) -> Result<(), AuthError>` — validates the challenge, runs the password-strength policy, sets the new hash, marks the challenge used, and **revokes the user's sessions + bearer tokens** (a reset implies possible compromise; "log out everywhere" is the safe default).

All flows go through the ORM (no raw SQL), per the plugin rule.

## Email: pluggable mailer (the "wire-in", decoupled)

`umbral-auth` does NOT take a Cargo dependency on `umbral-email` — a REST API that never sends auth mail must not compile a mail stack. Instead:

- Auth renders email bodies itself via `umbral::templates::render` (core, no extra dep) from shipped, overridable templates (`templates/auth/email/*.{html,txt}`).
- Auth defines the seam:
  ```rust
  pub struct OutgoingMail { pub to: String, pub subject: String, pub html: String, pub text: String }
  #[async_trait] pub trait AuthMailer: Send + Sync {
      async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError>;
  }
  ```
  wired via `AuthPlugin::mailer(impl AuthMailer + 'static)`.
- **Default `ConsoleMailer`**: prints the rendered mail (code/link visible) to stderr in Dev/Test; logs a loud warning if it is ever the active mailer outside Dev/Test. Mirrors `umbral-email`'s console default so the flows work with zero config in development.
- **Convenience adapter** `umbral_email::auth_mailer()` returns an `impl AuthMailer` that delegates to `umbral_email::send`. This lives in `umbral-email` (email → auth dep; acyclic since auth does not depend on email). Common production wiring becomes `.mailer(umbral_email::auth_mailer())`.

## Two surfaces

Both are thin: parse input → call a core fn → format output. No business logic is duplicated.

### JSON (`with_default_routes()`)

Base path auto-follows the REST plugin (see below). Existing four routes plus:

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `{base}/auth/verify-email` | `{email, code}` | 204 / 400 generic |
| POST | `{base}/auth/resend-verification` | `{email}` | 202 (always, generic) |
| POST | `{base}/auth/password-forgot` | `{email}` | 202 (always, generic) |
| POST | `{base}/auth/password-reset` | `{token, new_password}` | 204 / 400 |

Each gets an `openapi_paths` entry under the `auth` tag, so the REST/OpenAPI plugins render them in Swagger UI exactly like the existing four.

### Form-action endpoints (`with_form_routes()`) — REVISED 2026-06-29

**Revision rationale.** The original design shipped full server-rendered login/signup/verify/reset *pages* (GET handlers + bundled Jinja templates). That was wrong: those pages carry the developer's brand and design, so the framework shipping opinionated pages is noise the developer has to fight. The framework's job is the **form-action endpoints** — the developer writes their own pages (in Jinja or anything else) whose forms POST to these. The bundled page templates were reverted; the email-body templates (which the framework *sends*, and which need a default) stay.

POST-only. Default prefix `/auth` (configurable via `with_form_routes_at(prefix)`). Each handler: form-decode the body → run the same core logic the JSON handler runs (including throttle for login/signup) → set a session flash message → `303 See Other` redirect. No GET handlers, no template rendering, no shipped page templates.

| Route | Form body | Behavior |
|---|---|---|
| `POST /auth/login` | `username, password` | authenticate (+throttle); success → session + 303 to success-target; failure (bad creds / throttled / unverified-when-required) → flash error + 303 to error-target |
| `POST /auth/logout` | — | `logout` + 303 to success-target |
| `POST /auth/signup` | `username, email, password` | create_user (+register throttle, +auto-send code if `require_verified_email`); success → flash + 303; failure (weak pw / dup) → flash error + 303 to error-target |
| `POST /auth/verify-email` | `email, code` | `verify_email`; flash + 303 |
| `POST /auth/resend` | `email` | `start_email_verification` best-effort (generic flash, no enumeration) + 303 |
| `POST /auth/password-forgot` | `email` | `start_password_reset` (generic flash, no enumeration) + 303 |
| `POST /auth/password-reset` | `token, new_password` | `reset_password`; flash + 303 |

**Redirect targets (open-redirect-safe).** A target is *safe* only if it is a same-site relative path: starts with `/`, does not start with `//`, contains no scheme/backslash. Success → the `?redirect=<path>` query param if safe, else `/`. Error → the `Referer` header if safe (returns the user to the form page), else the `?redirect` value if safe, else `/`. Unsafe targets are silently replaced with `/`.

**Errors → flash messages.** On any failure the handler sets a `umbral_sessions::messages` flash (error level) and redirects; the developer renders `{{ messages }}` in their own page. This is the only feedback channel — no error query params, no rendered error page.

This surface is the redirect-style counterpart to the JSON surface (Task 10): same core fns, same throttle/enumeration protections, but form-encoded in and `303`-redirect out instead of JSON. The framework still ships and sends the **email** bodies (`templates/auth/email/*`), overridable by the app, since the framework is what sends those.

## Enforcement (opt-in)

`AuthPlugin::require_verified_email()`:
- On register (both surfaces) auto-calls `start_email_verification`.
- On login, an unverified user (`email_verified_at IS NULL`) is rejected: JSON `403 {error:"email_not_verified"}`; HTML re-renders login with a flash + a "resend" affordance.
- Off by default: endpoints still exist, the column is still tracked, nothing auto-sends, nothing blocks.

## Base-path auto-follow (decoupled)

New core ambient in `umbral-core`, surfaced on the facade as `umbral::web::api_base() -> String` (`OnceLock<String>`, default `"/api"`), with an internal `set_api_base`. The REST plugin publishes its configured base path into it during a build phase that completes *before* router assembly (so the value is present regardless of plugin registration order — the per-plugin `models()`/`system_checks()` walks all finish before `routes()` runs). Auth reads `api_base()` when building its JSON router and mounts at `{api_base}/auth`. `with_default_routes_at(prefix)` overrides entirely. When REST is absent the default `/api` keeps today's `/api/auth` behavior identical.

## Throttling & security

- Reuse the existing throttle infrastructure for `verify-email`, `resend-verification`, and `password-forgot`, keyed per IP + email, to stop email-bombing and online code-guessing at the edge (in addition to the per-challenge attempt cap).
- Enumeration: `resend-verification` and `password-forgot` always return the same generic accepted response whether or not the account exists; `verify-email` returns a generic error on any failure.
- CSRF: every HTML POST form carries `{{ csrf_input }}`; the automatic-CSRF middleware validates it.
- Reset/forgot tokens and codes are never logged except by the dev ConsoleMailer.

## Testing

Behavioral tests (real rows, real route, read the object graph back), per the project's testing convention:
- A recording `TestMailer` captures `OutgoingMail` so tests can extract the emitted code/token and complete the flow end-to-end.
- Cover, on **both** surfaces where applicable: register → verify (code), forgot → reset (token, incl. session/token revocation), login blocked when `require_verified_email` and unverified, logout clears the session, code expiry/attempt-cap/single-use, reset-token expiry/single-use, enumeration responses are generic.
- Base-path auto-follow: a test with REST `.at("/v2")` asserts auth JSON mounts at `/v2/auth`; without REST it stays `/api/auth`.
- SQLite, mirroring the existing `plugins/umbral-auth/tests/*` harness (shared ambient pool + test lock).

## Documentation

Per "ship a feature, ship its doc page", add MDX pages under `documentation/docs/v0.0.1/auth/`:
- `email-verification.mdx`, `password-reset.mdx`, `auth-pages.mdx` (the Jinja surface + overriding templates), `mailer.mdx` (the AuthMailer seam + the umbral-email adapter).
Each: purpose, one example, link back to this design note.

## Out of scope (YAGNI; not requested)

- Authenticated `change-password` (distinct from forgot/reset).
- Magic-link / passwordless login.
- TOTP / 2FA.
- Durable retry queue for mail (belongs to an `umbral-email` + `umbral-tasks` integration).

These are natural follow-ups; each can be its own spec.

## Migration & compatibility notes

- Two schema changes (one added column, one new table) ship as autodetected migrations. Consuming apps run `makemigrations` then `migrate`. Per project policy the DB is never wiped to force a clean run; the nullable column + new table apply cleanly against existing data.
- The JSON base path changes from a hardcoded `/api/auth` to `{api_base}/auth`. With REST absent or at its default base this is byte-identical (`/api/auth`); apps that customized the REST base will see auth follow it (the intended behavior) and can pin it with `with_default_routes_at` if they need the old literal.
