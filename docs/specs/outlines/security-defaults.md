# Outline — Security defaults

| | |
|---|---|
| **Status** | Outline. Promotes at M9 entry (with auth/sessions). |
| **Maps to milestone** | M9 |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `05-backends-and-system-check.md`, outlines `auth-and-sessions.md`, `web-layer.md`, `forms.md`, `templates.md`, `arch.md §4.5` |

## Purpose

`umbra-security` is the plugin that turns the Django "secure by default" list into a single boot-time fact: a fresh `App::builder().build()` already has CSRF on, `X-Frame-Options: DENY` set, `X-Content-Type-Options: nosniff` set, a sane `Referrer-Policy`, signed cookies, and parameterised SQL (free from sqlx). HSTS and CSP — the two headers whose wrong default actively breaks apps — are off in `Environment::Dev` and on in `Environment::Prod`, driven by the `Settings.environment` knob that already exists in `01-app-and-settings.md`. The framing is the one Django ships and that `arch.md §4.5` names: **most of these are on by default; users opt out, not in.** A misbehaving header is a one-line override; the dangerous default is the one that requires a config call to reach a safe state, and umbra refuses to ship that shape. This outline also owns the `umbra::sign` primitive (HMAC over `Settings.secret_key`) that sessions, CSRF tokens, password-reset URLs, and any third-party plugin's signed payload all reach for.

## Key concepts

**CSRF middleware.** A double-submit token: a signed cookie (`umbra_csrf`) issued on first GET, plus the same token expected in either a hidden `<input>` (forms) or an `X-CSRF-Token` header (AJAX) on any unsafe-method request. The token is signed via `umbra::sign` so an attacker can't forge one without the secret. The `{% csrf_token %}` template tag (lives in `templates.md`) emits the hidden input; `forms.md` renders it automatically on every form. Exempt paths are configurable per-plugin (`CsrfSettings.exempt = ["/webhooks/*"]`) and per-handler (a `#[csrf_exempt]` attribute that emits a tower layer skip). Login POST is **not** exempt — `auth-and-sessions.md` cross-references this explicitly.

**Clickjacking — `X-Frame-Options: DENY`.** Set globally by the security plugin's middleware. A per-route override (`#[frame_options(SameOrigin)]` or a route-scoped layer) handles the rare embeddable view. No knob to disable globally; the override path is the documented one.

**HSTS — `Strict-Transport-Security`.** Configurable via `SecuritySettings.hsts { max_age, include_subdomains, preload }`. Off in `Environment::Dev` (HSTS over `http://localhost` breaks dev), on in `Environment::Prod` with `max_age = 31536000` and `include_subdomains = true` by default. The system check from `05-backends-and-system-check.md` warns at boot if `Prod` ships with HSTS disabled.

**`X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`.** Both on by default, single setting each to override.

**COOP and CSP.** `Cross-Origin-Opener-Policy: same-origin` ships on by default. CSP is the genuinely per-app one: a sensible default (`default-src 'self'`) ships, but the framework expects the user to override `SecuritySettings.csp` for any non-trivial app. The deep spec will design the per-route override hook (`#[csp(...)]` or `CspBuilder` returned from a handler) once admin and rest exercise it.

**Secret signing.** One primitive, used everywhere a payload needs an unforgeable tag:

```rust
pub fn sign(payload: &[u8]) -> SignedToken;
pub fn verify(token: &SignedToken) -> Result<Vec<u8>, SignError>;
```

HMAC-SHA256 over `Settings.secret_key`. Sessions sign their cookie payload through this. CSRF tokens are `sign(session_id || nonce)`. Password-reset URLs are signed and time-bounded via the same call. Third-party plugins use it for their own signed payloads — no plugin should hand-roll HMAC.

**Password validators.** A small catalogue (`MinLength`, `CommonPasswordList`, `NumericOnly`, `UserAttributeSimilarity`) configured in `AuthSettings`. Owned operationally by `auth-and-sessions.md`; cross-listed here because the *defaults* (which validators ship enabled, which are off until you opt in) are a security-policy decision this outline anchors.

**Secure-cookie defaults.** `SameSite=Lax` (configurable to `Strict`), `Secure` in `Environment::Prod`, `HttpOnly` for session cookies. The defaults apply to every cookie issued through `umbra::web::Cookie`, not just the session cookie — a plugin that issues its own cookie inherits the safe shape unless it explicitly opts out.

## Promote-to-deep trigger

Promote at **M9 entry**, when `umbra-auth` and `umbra-sessions` re-express as plugins and the first cookie- and token-bearing flows go live. The deep spec locks the CSRF token format, the per-route override mechanism for headers, and the `umbra::sign` API surface that sessions consume.

## Open questions

- **CSP shape.** Default (`default-src 'self'`) vs a stricter starter; whether per-route overrides are an attribute (`#[csp(...)]`), a builder returned from the handler, or a tower layer. Likely needs admin and rest to weigh in before locking.
- **CSRF for AJAX.** The cookie-to-header pattern (frontend reads `umbra_csrf` cookie, echoes it in `X-CSRF-Token`) vs a custom header issued out-of-band vs treating Bearer-token endpoints as automatically exempt. Affects how `umbra-rest` interacts with browser-driven SPAs.
- **Password-validator catalogue extensibility.** A fixed enum of built-ins plus a `Box<dyn PasswordValidator>` escape hatch vs an ordered `Vec<Box<dyn PasswordValidator>>` from the start. Shared open question with `auth-and-sessions.md`.
- **`SECRET_KEY` rotation.** Whether umbra ships a key-rotation helper (old-key list for verify, new key for sign) or treats rotation as an ops concern with a recipe in the docs. Sessions and signed URLs both care.
- **Per-route header overrides.** Whether `#[frame_options(...)]`, `#[csp(...)]`, and friends are first-class attributes the macro layer recognises, or a single `#[security(...)]` group, or a `SecurityLayer::for_route(...)` builder. Affects ergonomics across every plugin.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (`Settings.secret_key`, environment-aware defaults via `Environment::{Dev, Test, Prod}`), `02-plugin-contract.md` (security middleware ships as `Plugin::middleware()` — the same contract every plugin uses), `05-backends-and-system-check.md` (the boot-time warning when `Prod` ships with HSTS off or with an empty password-validator list).
- Sibling outlines: `auth-and-sessions.md` (password hashing and validator catalogue, secure-cookie defaults applied to the session cookie, login POST CSRF interaction), `forms.md` (CSRF token rendered into every form by default), `templates.md` (autoescape is the XSS defence, the `{% csrf_token %}` tag lives here), `web-layer.md` (the middleware chain the security headers attach to, the cookie shape the secure-cookie defaults apply through).
- `arch.md §4.5` — the secure-by-default list this outline mechanises.
