# umbra-security — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbra-security/src/lib.rs` + `tests/`. Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`. Findings already filed are tagged **(already #N)**; everything else is **NEW**.

## Verdict

**Complete and well-built for what it covers: CSRF + a configurable security-header bundle. The CSRF core is genuinely sound (signed double-submit, constant-time compare, auto-rotation). The known gaps are all already-filed config-conditional ones (#75: empty `SECRET_KEY`; exempt-prefix boundary; HSTS/CSP opt-in).** Scope is narrower than Django's `SecurityMiddleware` + the full `SECURE_*` family — notably **no rate-limiting, no CORS** (CORS lives in `umbra-core`, not here), and **no boot-time `check.rs` warnings** for the dangerous defaults. Everything it claims to do, it does, with no stubs.

Completeness one-liner: **CSRF + headers are real and complete; rate-limiting is absent, several `SECURE_*` knobs are opt-in with no fail-closed boot check, and the empty-`SECRET_KEY` hole (#75) is the one outright bug.**

## Completeness

| Capability | State | Note |
|---|---|---|
| CSRF middleware | **Complete** | signed double-submit `<raw>.<hmac>`, header + 2 form-field shapes, GET/HEAD/OPTIONS exempt, auto-mint-before-handler, auto-rotation of stale cookies. |
| Signed / session-bound CSRF | Complete | HMAC-SHA256, optional session-cookie binding, constant-time `ct_eq`. |
| Security headers | Complete | nosniff, X-Frame-Options DENY, Referrer-Policy, X-XSS-Protection:0, COOP same-origin, Server, + opt-in HSTS / CSP / Permissions-Policy / CORP / COEP. |
| `Server` header set/strip | Complete | configurable, tested. |
| Request body limit | Complete | tower-http `RequestBodyLimitLayer`, tested (413). |
| Sensitive-header redaction | Complete | authorization/cookie/set-cookie marked sensitive. |
| **Rate limiting** | **Absent** | not in this plugin (no token-bucket / per-IP throttle). A real DoS / brute-force gap for a "security" plugin. |
| **CORS** | Not here | lives in `umbra-core::cors` (`reviews/security.md` confirms it's strict-by-default there). FYI: a consumer looking in `umbra-security` for it won't find it. |
| `SECURE_*` parity / boot checks | **Partial** | the *headers* exist, but there's **no boot-time warning** when HSTS/CSP are off in Prod, when `secret_key` is empty, or when the exempt-prefix is dangerously broad. Django surfaces these via `manage.py check --deploy`. |
| Stubs / `todo!()` / no-ops | **None** | clean; `strip_server_header` and `forbidden()` are real. |

## Findings

### NEW — Important (clarified from #75)
- **Empty/whitespace `secret_key` silently signs CSRF with an empty HMAC key.** `lib.rs:392-394` + `459-460`. `from_config` treats `Some("")` as a resolved secret (`settings.map(|s| s.secret_key.clone())` — no emptiness check), and `Hmac::new_from_slice(b"")` accepts a zero-length key (the `.expect("HMAC accepts any key length")` proves it). `UMBRA_SECRET_KEY=""` ⇒ signed-mode tokens are signed under a *publicly-known empty key* ⇒ forgeable, with **no warning**. This is the one outright security bug in the plugin. **already #75 / security.md top risk #2.** Fix: treat empty/whitespace `secret_key` as `None` in `from_config` (degrade to plain double-submit, which is the intended fallback) **and** emit a `check.rs` finding.

### NEW — Important
- **`csrf_exempt_paths` uses bare `path.starts_with(prefix)` — no segment boundary.** `lib.rs:407-411`. Exempting `/api` also exempts `/api-internal`, `/apikeys`, `/api.json`, and every other path sharing the prefix. A cookie-authed `/api/account/delete` under an `/api` exemption becomes fully CSRF-exempt. **already** security.md (CSRF/Headers, Important). Fix: `path == p || path.starts_with(&format!("{p}/"))`. *(Note: this is genuinely net-new vs the backlog's synthesized list, which folded only the empty-key item into #75 and omitted the exempt-prefix boundary — worth a dedicated line.)*

### NEW — Important (defense-in-depth, missing capability)
- **No rate-limiting / brute-force throttle anywhere in the plugin.** A framework "security" plugin with CSRF + headers but no per-IP / per-route rate limit leaves login brute-force, OAuth-callback hammering, and generic request-flood DoS entirely to the deployer's reverse proxy. Django leans on third-party (`django-ratelimit`) too, so this is *defensible as out-of-scope*, but it should be an explicit deferred entry, not an unstated absence. → **NEW gap** (deferred / explicit-scope).

### NEW — Important (boot-time safety, ties to gaps2 #25)
- **No `check.rs` boot warnings for the dangerous defaults this plugin ships with.** HSTS default `false` and CSP default `None` (`lib.rs:220,224`) mean a Prod app mounting `SecurityPlugin::new()` ships **no** `Strict-Transport-Security` (SSL-stripping) and **no** CSP (no XSS backstop), silently. **already** security.md (CSRF/Headers, Important) but worth restating as a *boot-check* item: when `Environment::Prod` + this plugin mounted + `hsts==false`/CSP `None`, warn. Fix lives in the framework boot `check.rs` (the plugin should contribute system checks). → fold into **gaps2 #25** / **#75**.

### FYI — controls correctly in place (not findings)
- CSRF token entropy (32-byte CSPRNG, hex), constant-time compare (`subtle::ct_eq`), GET/HEAD/OPTIONS-only exemption, no `_method`/`X-HTTP-Method-Override` smuggling path, form-body peek capped at 1 MiB (`MAX_FORM_BODY`), `Set-Cookie` *appended* not inserted (handler's session cookie survives — tested). Auto-rotation makes the `signed_csrf` default-on flip deploy-safe. All correct.
- `cookie_value` is a naive linear scan with no quoted-value handling (`lib.rs:510-522`) — fine for the hex tokens it parses; not a finding.

## Architecture / plugin-contract

Clean and idiomatic. Facade-only (`use umbra::prelude::*`, `umbra::settings`, `umbra::templates`) — no core internals. No models, no migrations (correct — it's middleware-only). Implements `Plugin::wrap_router` exactly as intended (the documented reason this lives there: middleware needs a `tower::Layer`). Layer ordering is deliberate and documented (CSRF innermost, body-limit outermost). `test_support` module is `#[doc(hidden)]` and honestly labeled non-stable. One observation: the plugin reads `secret_key` itself rather than the framework providing a validated secret — which is exactly how the empty-key hole slips through; a framework-validated `Secret` type would close it at the source (Fix-don't-patch).

## Tests

Strong, behavioral, driven through the real `Plugin::wrap_router` and `test_support` against actual axum routers — not asserting internals. Covers: first-visit mint visible to handler, Set-Cookie append-not-clobber, valid/mismatched/missing-token POST (200/403), header bundle presence + opt-in, body-limit 413, exempt-path skip, signed-mode rotation + non-rotation, session-binding, signing determinism + forgery rejection.

**Gaps:**
- **No test for the empty-`secret_key` degradation** — the #75 bug. A test asserting `from_config` with `secret_key == ""` either rotates to plain mode or warns would lock the fix in.
- **No test for the exempt-prefix boundary** (`/api` vs `/api-internal`) — the unit test (`exempt_path_matching_is_prefix_based`) actually *encodes the buggy behavior* as expected (`is_exempt("/api/customer/1")` true is fine, but there's no `/api-internal` negative case).
- **No form-field CSRF path integration test** (`csrf_token`/`__csrf` body extraction, `lib.rs:597-621`) — only the header path is exercised end-to-end.
