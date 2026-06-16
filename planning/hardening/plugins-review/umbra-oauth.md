# umbra-oauth — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbra-oauth/src/**` + `tests/`. Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`, `reviews/race-conditions.md`. Findings already filed are tagged **(already #N)**; everything else is **NEW**.

## Verdict

**Ship-worthy for login + connect; one real completeness hole (token refresh) and a cluster of already-filed security hardening items.** The plugin is more complete than the backlog assumed: it ships **two** real providers (Google OIDC + GitHub), not "Google only", and the `Masked<T>` token-at-rest story is genuine and tested (round-trips through the sealed column). The create-or-link policy is the strongest part — the verified-email anti-takeover gate is correct and well-tested. The gaps are (1) stored refresh tokens are never *used* (no refresh exchange), so long-lived API access silently dies at access-token expiry, and (2) the PKCE / single-use-state items already filed as **#74**.

Completeness one-liner: **Login + connect + disconnect + account-linking are complete and tested; token *refresh* is the one missing capability, and PKCE is absent (already #74).**

## Completeness

| Capability | State | Note |
|---|---|---|
| Providers | **Google (OIDC) + GitHub**, both real | Backlog said "Google only" — stale. Generic OIDC provider not shipped (trait is open for third parties). |
| Authorize → callback flow | Complete | state-CSRF, provider-match check, error/denial handling all present (`routes.rs:204-220`). |
| PKCE | **Missing** | already #74 (`providers/google.rs:106-119`). |
| State single-use | **Missing** (replayable) | already #74 (`routes.rs:143` set, `:214` read, never deleted). |
| Token storage at rest | Complete | `Masked<String>` access + refresh, `#[umbra(noform)]`, tested round-trip (`policy.rs:352`). |
| Token **refresh** (use the stored refresh token) | **Missing** | NEW — see Findings. Refresh tokens are stored and rotated-on-reauth, but there is no `grant_type=refresh_token` path, so once an access token expires the app can't call the provider API on the user's behalf until the user re-authenticates. The whole "later API access (Drive, repos)" use case in the crate docs is unreachable past expiry. |
| Account linking (user ↔ identity) | Complete | 4-rule policy in `policy.rs`; connect-mode hijack refusal tested (`:450`). |
| Error / denial handling | Complete | `?error=` → redirect to login (`routes.rs:204`); missing code / state-mismatch → 400. |
| CSRF on callback | Present (state token) | Not single-use (#74). |
| SPA token mode | Complete | bearer minted into URL fragment, `?next=` allowlisted. |
| Disconnect | Complete | `POST /oauth/{provider}/disconnect`, auth-gated, ORM delete. |
| Stubs / `todo!()` / no-ops | **None found** | No `todo!`, no `unimplemented!`, no swallowed errors. `unreachable!` only in the discovery test's FakeProvider. |

## Findings

### NEW — Important
- **Token refresh is never implemented; stored refresh tokens are write-only.** `provider.rs` / `policy.rs` / both providers. There is no method anywhere that takes a `SocialAccount`'s stored `refresh_token` and exchanges it for a fresh access token (`grep` for any `refresh_access`/`grant_type=refresh_token` returns nothing; the only "refresh" is `refresh_tokens()` which *overwrites* on re-auth). `expires_at` is stored but never read. Consequence: the documented "API access on their behalf (Drive, repos)" dies the moment the access token expires (Google: ~1h), with no recovery short of a full interactive re-auth. Fix: add `OAuthProvider::refresh(&self, refresh_token) -> TokenSet` (default `Err(Unsupported)` for GitHub which issues none) + a `SocialAccount::access_token_valid()` / `ensure_fresh_access_token()` helper that refreshes when `expires_at` is past. → **NEW gap.**

### NEW — Important (availability / perf)
- **`reqwest::Client::new()` per request, with no timeout.** `providers/google.rs:122,142`, `github.rs:130,150`. Each token-exchange and identity-fetch builds a brand-new `reqwest::Client` (a fresh connection pool + TLS config every call — wasteful) **and sets no `.timeout()`**, so a hung/slow provider endpoint ties up the callback handler indefinitely (the user's browser hangs on the redirect; a worker thread is pinned). A slow-loris-y provider or a network blip becomes a request-handler stall. Fix: one lazily-initialised shared `reqwest::Client` per provider (or a module `OnceLock`) built with a sane `.timeout(Duration::from_secs(10))` / connect timeout. → **NEW gap.**

### NEW — Important (concurrency, not covered by race-conditions.md)
- **`unique_username` SELECT-loop is a TOCTOU + unbounded retry.** `policy.rs:211-228`. Two concurrent social signups deriving the same base username both SELECT "not taken", both `create()` — the loser hits the `auth_user.username` UNIQUE constraint and the *whole resolve fails* (a 409 to the user) instead of retrying the next candidate. The loop also doesn't catch `UniqueViolation` to advance `n`. This path is **not** in `reviews/race-conditions.md` (that audit didn't cover umbra-oauth). Fix: catch `WriteError::UniqueViolation` from `create_auth_user`'s insert and retry with `n+1`, same pattern the backlog prescribes for `add_user_to_group` (#71). → **NEW gap** (sibling of #71; oauth was out of #71's scope).

### NEW — Optional (correctness)
- **`refresh_tokens` is a non-transactional multi-statement update, and the re-auth path has no row-lock.** `policy.rs:123-153` runs one `update_values`; benign today (single UPDATE), noted so the refresh-flow fix above doesn't reintroduce a read-modify-write. FYI.
- **GitHub `provider_uid` derives from a numeric id with no zero/garbage guard.** `providers/github.rs:94` `u.id.to_string()`. If GitHub ever returns `id: 0` or the parse defaults, two accounts could collide on `(github, "0")`. Low risk (GitHub ids are positive), FYI.

### Already filed (cross-ref — confirmed present, not re-counted)
- **No PKCE + replayable `state`** — `routes.rs:143-218`, `providers/google.rs:106-119`. **already #74** / security.md top risk #1.
- **Return-URL allowlist is a raw prefix match** (`is_allowed_return`, `lib.rs:141-143`) — `https://app.example.com` matches `https://app.example.com.evil.com`, and a login flow mints a bearer into that URL → token exfiltration. **already** security.md (Optional, Input boundaries). Worth promoting given token mode is shipped.
- **`state` compared with plain `!=`** (`routes.rs:218`) — **already** security.md (Optional).
- **`TokenSet` / provider structs derive `Debug` over plaintext secrets** (`provider.rs:13`, `google.rs:22`, `github.rs:24`) — **already** security.md (Optional).
- **Token-exchange parse errors interpolate raw provider body** into surfaced error (`google.rs:67`, `github.rs:65` → `routes.rs:229`) — **already** security.md (Optional).
- **No `Masked` key versioning / rotation orphans ciphertext** — **already** security.md (Optional, deferred).

## Architecture / plugin-contract

Clean. Facade-only imports (`use umbra::prelude::*`, `umbra::orm::Masked`, `umbra::plugin::*`) — no `umbra-core` internals. Owns its one migration (`SocialAccount` via `ModelMeta::for_::<SocialAccount>()`), declares `dependencies() = ["auth"]` correctly so the FK to `auth_user` orders right. No raw `sqlx::query` in `src/` outside `#[cfg(test)]` (`policy.rs:270,304` is the documented test-table-DDL exception). `Plugin` impl is idiomatic: routes, `api_endpoints`, `on_ready` discovery publish. Provider abstraction (`OAuthProvider` trait) keeps the flow provider-agnostic. The one structural nit: the per-call `reqwest::Client::new()` (above) belongs behind a shared handle.

## Tests

Good behavioral coverage of the **policy** (the security-critical part): all 4 link rules + the connect-hijack refusal + token round-trip through `Masked`, run against real sqlite rows via the public `resolve_user` path (`policy.rs:329-470`). Provider parsing is unit-tested against sample bodies (token/userinfo/emails). Discovery endpoint is driven through the real mounted router (`tests/discovery.rs`).

**Gaps (security-critical paths under-tested):**
- **No test drives the full `oauth_callback` HTTP handler** — state-mismatch rejection, `?error=` denial redirect, missing-code 400, and the SPA token-mode fragment append (`routes.rs:191-278`) are all untested end-to-end. The CSRF-state defense has zero integration coverage.
- **No test for the username-collision path** (`unique_username` retry) — would have surfaced the TOCTOU above.
- **No test for token refresh** — because it doesn't exist.
- `disconnect` handler untested.
