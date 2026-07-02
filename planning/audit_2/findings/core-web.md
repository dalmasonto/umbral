# Audit: core-web (transport / headers / injection)

Scope: `crates/umbral-core/src/web.rs`, `web/multipart.rs`, `web/streaming.rs`, `routes.rs`, `middleware.rs`, `cors.rs`, `hosts.rs`, `slash.rs`, `static_files.rs`, `errors.rs`, `ratelimit.rs`. Wiring cross-checked in `app.rs` and `check.rs` (read-only, out of edit scope).

---

## A. Executive summary

The transport layer is, for the pieces that exist, mostly careful: static-file resolution has a genuine three-layer traversal/symlink defence (`resolve_under_root`), the Host allowlist HTML-escapes the reflected host, CORS refuses the `*`-origin-plus-credentials footgun at build time, and the slash-redirect path guards CRLF injection. The dev-vs-prod error surface is gated correctly so stack/SQL detail does not leak in prod.

The serious gaps are things that are **absent by default**, which at 10M users is where the risk lives. (1) The core web stack adds **no security response headers at all** — no `X-Frame-Options`, `X-Content-Type-Options`, HSTS, CSP or `Referrer-Policy` are set anywhere in `App::build`; those live in a separate `SecurityPlugin`, and its absence is only a non-fatal boot *Warning* (`check.rs:655`). A default app is therefore clickjackable and MIME-sniffable. (2) There is **no request body size limit and no request timeout** wired by the framework — the multipart parser buffers the whole body in memory and its `TooLarge` cap is documented as unenforced dead code (`multipart.rs:100`). (3) The rate limiter that backs auth brute-force protection is **single-process and unbounded-memory** (`ratelimit.rs:36`), so across replicas the effective limit is `num × replicas` and an attacker can grow the key map without bound.

The three most urgent: no default hardening headers, no body-size/timeout limits (both memory/clickjacking DoS surfaces), and the non-distributed rate limiter weakening brute-force defence. What I could NOT assess from this scope: the actual CSRF token generation/verification and the security-header values (both in the out-of-scope `umbral-security` plugin), whether the rate limiter is actually applied to auth/expensive routes (that wiring is in `umbral-auth`/`umbral-rest`), and TLS termination / reverse-proxy config.

No CRITICAL issues found in the provided artifacts.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|
| 1 | HIGH | Headers | `app.rs` middleware wiring (1160-1272); `check.rs:635-668` | Core web stack sets **no security response headers** (X-Frame-Options, X-Content-Type-Options, HSTS, CSP, Referrer-Policy). They exist only in the out-of-scope `SecurityPlugin`, whose absence is a boot **Warning**, not an error. | A default umbral app is clickjackable (no frame-ancestors defence), MIME-sniffable, and does not force HTTPS. Ships insecure unless the operator both knows about and mounts `SecurityPlugin`. | Ship safe default headers from core (a `SetResponseHeader`/tower layer in `App::build`), or escalate `plugin.security_missing` to a boot **Error** in `Environment::Prod`. |
| 2 | HIGH | Input / DoS | `web/multipart.rs:98-108`, `parse_multipart` (213-263); `app.rs:572` | No framework request-body size limit and no enforced multipart cap. `parse_multipart` buffers the entire body in memory and reads each part fully via `field.bytes()`; the `MultipartError::TooLarge` variant is documented as never produced ("no cap is imposed at this layer yet"). No `DefaultBodyLimit`/`RequestBodyLimitLayer` is installed in `App::build`. | Unbounded in-memory buffering of upload bodies → memory-exhaustion DoS. File uploads are the classic bypass of axum's per-extractor 2 MB default. | Install a configurable `tower_http::limit::RequestBodyLimitLayer` (or `DefaultBodyLimit`) in `App::build` with a sane default, and wire the existing `TooLarge` check into the multipart entry points. |
| 3 | MEDIUM | DoS | `app.rs` (no `TimeoutLayer`); builder note at `app.rs:572` | No default per-request timeout layer. | A slow or hung handler / slowloris-style client ties up connections and tasks indefinitely; at scale this is a resource-exhaustion vector. | Add a default `tower_http::timeout::TimeoutLayer` (configurable, opt-out) in `App::build`. |
| 4 | MEDIUM | Rate limiting | `ratelimit.rs:36-44`, `check_at` (183-227) | Rate limiter is in-memory / single-process with an **unbounded** key map pruned only lazily per-key. Documented as known. | Behind N replicas the real limit is `num × replicas`, weakening the auth brute-force throttle that consumes this primitive; an attacker rotating keys/IPs grows the `HashMap` without bound (memory DoS). | Back the limiter with a shared store (Redis) for multi-instance correctness; add a periodic global sweep or capacity bound on the key map. |
| 5 | MEDIUM | Hosts / availability | `hosts.rs:83-91` (`host_guard`) | Allowlist check reads only the `Host` header. HTTP/2 requests carry `:authority` and may have no `host` header; such a request is treated as "missing Host" and 400'd in prod. | If the app terminates HTTP/2 directly (no HTTP/1.1 reverse proxy), legitimate traffic is rejected. Availability, not bypass. Assumption: axum/hyper does not synthesize `Host` from `:authority` into `headers()`. | Fall back to the `:authority` / URI authority when the `Host` header is absent before rejecting. |
| 6 | LOW | Open redirect | `slash.rs:94-112`, 209-215 | `alternate_path` builds the redirect target by toggling a trailing slash on `req.uri().path()`. A request path beginning `//host` yields `Location: //host/` (protocol-relative → cross-origin). | Only emitted if a route literally exists at `//host`, so not exploitable in practice, but a latent open-redirect shape. | Reject/normalize paths that start with `//` before probing; ensure the `Location` always begins with a single `/`. |
| 7 | LOW | Static / build | `static_files.rs:970-994` (`copy_tree`) | `collectstatic` follows symlinks (`std::fs::read`) when copying a plugin's `source_dir` tree into the public `static_root`. | A symlink inside a source dir copies arbitrary file contents into the served static tree. Build-time, trusted plugin input, so low. | Skip or reject symlinks during collect, or canonicalize-and-contain each source entry. |
| 8 | LOW | Error handling | `errors.rs:530-590` (`render_error_middleware`, `error_context`) | For registered non-404/500 status templates, the captured handler response body is interpolated as `message` into the error page in **both** dev and prod. | If a handler places internals in its error body, they are styled and shown in prod. Autoescaped (minijinja HTML), so not XSS; disclosure only. | Gate `message` on `dev_mode`, or document that handler error bodies for restyled statuses are shown verbatim. |

---

## C. Detailed findings (CRITICAL / HIGH)

### Finding 1 — No default security headers (HIGH)

`App::build` layers CORS, host validation, panic-catch, compression, and a trace span, but nothing that sets hardening response headers. The only place these header names appear in core is the *warning text* of a boot check:

```rust
// crates/umbral-core/src/check.rs:653
vec![SystemCheckFinding {
    check_id: "plugin.security_missing",
    severity: Severity::Warning,   // <-- boot continues
    ...
    message: format!(
        "{who} mounted without SecurityPlugin — requests have no CSRF \
         protection or security headers (CSP, HSTS, X-Frame-Options, …). ..."),
```

Because it is a `Warning`, an app that never mounts `SecurityPlugin` boots and serves traffic with zero clickjacking / MIME-sniffing / transport-downgrade protection.

**Attack scenario.** An attacker frames the target's authenticated admin page inside an invisible iframe on `evil.com`. With no `X-Frame-Options`/`frame-ancestors`, the page renders; a clickjacking overlay tricks a logged-in staff user into clicking a destructive admin action. Separately, without `X-Content-Type-Options: nosniff`, a user-uploaded file served with a loose content type can be sniffed into `text/html` and executed.

**Corrected direction — ship defaults from core** (in `App::build`, so a bare app is safe):

```rust
use axum::http::header::{HeaderName, HeaderValue};
use tower_http::set_header::SetResponseHeaderLayer;

router = router
    .layer(SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    ));
// HSTS only when TLS is terminated at/ahead of the app:
if matches!(settings.environment, Environment::Prod) {
    router = router.layer(SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    ));
}
```

Alternatively (minimum change): make `plugin.security_missing` a hard **Error** under `Environment::Prod` so an insecure prod boot fails fast. Note: the header *values* and CSRF itself live in the out-of-scope `umbral-security` plugin — see Blind spots.

### Finding 2 — No request body size limit; multipart buffers unbounded (HIGH)

The `TooLarge` machinery is explicitly dead:

```rust
// crates/umbral-core/src/web/multipart.rs:98
/// A part (or the whole body) exceeded a configured size cap.
/// Not produced by [`parse_multipart`] today (no cap is imposed at this
/// layer yet); reserved so a future size-limited entry point can report it...
TooLarge { limit: usize, actual: usize },
```

and the parser reads whole parts into memory:

```rust
// crates/umbral-core/src/web/multipart.rs:242
let bytes = field.bytes().await ... ;   // entire part buffered
form.files.push(FilePart { ..., bytes: bytes.to_vec() });   // + a full copy
```

No `DefaultBodyLimit` / `RequestBodyLimitLayer` is installed in `App::build` (grep of `app.rs` for body-limit layers returns only the doc-comment at line 572 telling users to add one themselves).

**Attack scenario.** An attacker POSTs a large or many-part `multipart/form-data` body to any upload endpoint. Each concurrent request buffers its full body (plus a `to_vec()` copy) in memory; a modest number of parallel large uploads exhausts RAM and OOM-kills the process. axum's built-in 2 MB `DefaultBodyLimit` does not protect streaming/multipart consumers, so the framework offers no backstop.

**Corrected direction:**

```rust
use tower_http::limit::RequestBodyLimitLayer;

// In App::build, from a configurable setting (e.g. settings.max_request_body):
router = router.layer(RequestBodyLimitLayer::new(settings.max_request_body_bytes));
```

and wire the cap into the multipart entry points so `MultipartError::TooLarge` actually fires (accumulate part sizes, compare against the limit, return `TooLarge` instead of buffering unboundedly).

---

## D. Blind spots (could not verify from this scope)

- **CSRF token generation, storage, and verification** — core only carries the `CURRENT_CSRF` task-local plumbing (`templates.rs`); the token mint/verify and the "which methods are protected" logic live in `umbral-security` / `umbral-sessions` (out of scope). Whether unsafe methods (POST/PUT/PATCH/DELETE) are actually verified was not assessable here.
- **Security-header values** — the actual CSP/HSTS/X-Frame-Options emission is in `umbral-security` (out of scope). Finding 1 is about the *core* adding none by default.
- **Whether the rate limiter is applied to auth/expensive routes** — `ratelimit.rs` is only the primitive; the application to login/register/API throttles is in `umbral-auth` and `umbral-rest` (out of scope).
- **Cookie flags (HttpOnly/Secure/SameSite)** — no cookie is set anywhere in the audited files; session/auth cookie construction is in `umbral-sessions`/`umbral-auth` (out of scope). Could not verify defaults.
- **TLS termination, reverse-proxy trust, `X-Forwarded-*` handling** — no code in scope consumes forwarded headers; runtime/infra config unknown.
- **axum `DefaultBodyLimit` interaction** — Finding 2 assumes the multipart body reaches `parse_*` via an extractor not subject to (or with a disabled) `DefaultBodyLimit`; the extraction site is in the plugin/handler layer, not in scope.
- **HTTP/2 `Host`/`:authority` behaviour** (Finding 5) — the exact axum/hyper mapping was reasoned about, not runtime-verified.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
- Install a `RequestBodyLimitLayer` and a `TimeoutLayer` with sane defaults in `App::build` (Findings 2, 3).
- Escalate `plugin.security_missing` to an Error in `Environment::Prod`, or add the default hardening headers in core (Finding 1).
- Normalize/reject `//`-prefixed paths in the slash-redirect probe (Finding 6).

**Short term (< 2 weeks)**
- Wire the multipart `TooLarge` cap end-to-end so uploads are bounded independently of the global body limit (Finding 2).
- Fall back to `:authority`/URI authority in `host_guard` when `Host` is absent (Finding 5).
- Gate the restyled-error `message` on dev mode (Finding 8); reject symlinks in `collectstatic` (Finding 7).

**Structural (needs design work)**
- Give the rate limiter a shared/distributed backing store and a bounded/swept key map so multi-replica brute-force limits are correct (Finding 4).
- Decide the framework's default security posture: whether core ships hardening headers itself or whether `SecurityPlugin` becomes mandatory-in-prod (Finding 1).

---

## Docs updated

No documentation edits were made. The web doc pages in `documentation/docs/v0.0.1/web/` that overlap this scope — `trailing-slash.mdx`, `error-pages.mdx`, `streaming.mdx`, `middleware.mdx`, `routes.mdx` — were checked and do **not contradict** the code (`trailing-slash.mdx` matches `slash.rs` exactly: 308, probe-on-404, query preservation, custom-fallback precedence).

Gap worth noting for the orchestrator (not a contradiction, so no edit): the security-relevant web features found lacking here — CORS (`cors.rs`), ALLOWED_HOSTS host validation (`hosts.rs`), request body limits, and security headers — have **no user-facing doc page at all**. Per the repo's "ship a feature, ship its doc page" rule these are missing pages, but writing new pages for absent/insecure-by-default behaviour would be documenting a recommendation rather than shipped behaviour, so I left them for the fixes in section E.
