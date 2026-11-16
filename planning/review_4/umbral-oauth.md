# review_4 · umbral-oauth

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 3127 LOC (`plugins/umbral-oauth`) |
| **Reviewer** | review_4 wave 1 (sessions + oauth) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 2 medium · 0 low · 1 info — 3 new, 4 known |

## Summary

- The crate implements the OAuth 2.0 login/connect flow: an `OAuthProvider` trait with Google and GitHub built-ins (plus custom providers), a state+PKCE authorization flow kept in the session, create-or-link policy (`resolve_user` rules 1–4), connect/disconnect routes, cookie-session login with an optional SPA bearer-token mode, and a `SocialAccount` model whose tokens are sealed in `Masked` columns.
- Overall state is healthy. State CSRF, PKCE S256, server-derived `redirect_uri`, the return-URL allowlist, provider-error passthrough, token-at-rest secrecy, and the disconnect authorization scope all read sound and are mostly covered by tests; the previous sweep's fixes (OAU-1/2/4, gaps2 #74, gaps4 #8/#93) are verified held in the tree.
- Both MEDIUMs are policy-level, each with a small local fix. OAUTH-1: rule 3's verified-email auto-link attaches a provider identity to any pre-existing local account with that email without checking the *target* account was ever verified — the account pre-hijack that OAU-2's own recommendation named but that was not implemented. OAUTH-2: a deactivated user with a linked provider completes the OAuth login — session established, and in token mode a bearer token minted — while every sibling login surface gates on `is_active`.
- The gaps4 #93 SPA token-mode traps are no longer silent: the "no allowlisted `?next` while token mode is on" case emits an explicit warning (`routes.rs:336-348`).
- Fix order: OAUTH-1 → OAUTH-2 → the INFO observability nit.

## Findings

### OAUTH-1 · MEDIUM · security · self-verified
**Rule 3 links a trusted provider's verified email to any pre-existing `AuthUser` holding that address without checking whether the target account was itself verified — enabling OAuth account pre-hijacking against an unverified local registration.**

- **Where**: `plugins/umbral-oauth/src/policy.rs:87-101` (the link), target-side gap in the same block; context `planning/audit_2/findings/plugin-oauth-tenants.md:29` (OAU-2)
- **Evidence**:
  ```rust
  // policy.rs:89-101
  if trusted_verified_email && let Some(email) = identity.email.as_deref() {
      let matched = AuthUser::objects()
          .filter(auth_user::EMAIL.eq(umbral_auth::normalize_email(email)))
          .first()
          .await
          .map_err(db_err)?;
      if let Some(user) = matched {
          create_social_account(user.id, provider_key, identity, tokens).await?;
          return Ok(user.id);
      }
  }
  ```
  The match filters on email only — no `email_verified_at` condition on `matched`. OAU-2's recorded recommendation reads "additionally require the target account's own email to be verified before linking"; only the provider-trust half (`trusted_verified_email`) shipped, and the tracker row is marked ✅ done.
- **Impact**: Concrete pre-hijack. The attacker registers locally as `victim@example.com` before the victim ever uses OAuth. By default (email verification is opt-in and enforced only at the register/password-login routes) that account is immediately usable with the attacker's password and its `email_verified_at` is `None`. When the victim later clicks "Sign in with Google" — Google is trusted to assert verification, and the email matches — rule 3 links the victim's Google identity to the attacker's row and returns it as the login. The victim's session now belongs to an account whose password the attacker chose, and nothing in the flow tells the victim the email was already registered. Bounding facts: only providers with `trusts_verified_email() == true` reach this rule (Google; GitHub's public-email path is untrusted), and the target row must exist.
- **Fix**: gate rule 3 on `matched.email_verified_at.is_some()`; for an unverified match, skip to rule 4's create path (which will surface the conflict) or return a dedicated error instructing the user to sign in with their password and link explicitly from settings.

### OAUTH-2 · MEDIUM · security · self-verified
**The OAuth login flow does not check `is_active`, so a deactivated account completes the callback, gets a session, and in token mode gets a freshly minted bearer token — the one login surface where the `is_active` gate is missing.**

- **Where**: `plugins/umbral-oauth/src/routes.rs:354-360` (session establishment), `:328-335` (token minting in the redirect fragment), `:367-374` (`mint_login_token` loads the user unfiltered); the already-linked return is `policy.rs:68-79`
- **Evidence**:
  ```rust
  // routes.rs:354-356
  if flow.connect_user.is_none() {
      if let Err(e) =
          login_user_id(&headers, response.headers_mut(), Some(user_id.to_string())).await
  // routes.rs:368-371 — no is_active filter:
  let user = AuthUser::objects()
      .filter(auth_user::ID.eq(user_id))
      .first()
  ```
- **Impact**: A deactivated (`is_active = false`) user whose provider link still exists clicks "Sign in with Google": the flow completes, a session row is created bound to their id, and in token mode the SPA redirect carries `#token=<bearer>`. In-tree consumers all re-check — `AuthUser::authenticate` (password) filters `is_active`, umbral-auth's identity resolvers re-filter `IS_ACTIVE` for session and bearer reads (`plugins/umbral-auth/src/extractors.rs:105-126`), and umbral-permissions denies deactivated users in `pre_perm_check` — so no built-in gate is bypassed. The exposure is the documented raw primitive `umbral_sessions::current_user_id_str`, which now reports the disabled user as logged in for any app middleware that trusts it (the framework's own docs invite this), plus a minted credential whose validity depends on every consumer re-filtering, and a session row a deactivation flow does not clean up. The divergence is the finding: four sibling surfaces gate on `is_active`; this one forgot.
- **Fix**: after `resolve_user` returns (and on rule 1's already-linked return), point-read the user's `is_active` — `mint_login_token` already does the row load — and refuse with the same generic inactive error the password login returns before `login_user_id`/`mint_login_token` run. One check at the callback covers both login shapes.

### OAUTH-3 · INFO · observability
**Google's token exchange never checks the HTTP status, so the provider's own error detail is discarded before it can reach a log.**
- **Where**: `plugins/umbral-oauth/src/providers/google.rs:134-159`
- **Observation**: `exchange_code` sends the form, reads `resp.text()`, and hands the body straight to `parse_token` with no `error_for_status()`. Google returns error JSON (`{"error":"invalid_grant","error_description":...}`) with a 400 on a rejected code, so the log records only a serde "missing field" error — the actionable detail (`invalid_grant` due to a replayed/expired code, redirect_uri mismatch) is thrown away. No leak concern: parse errors never interpolate the body and the client sees a fixed 502. Add `.error_for_status()` plus a log of the status and the provider's `error` field before parsing; GitHub's `parse_token` shares the shape and would benefit equally.

## Strengths

- **State and PKCE are done properly.** 128-bit UUIDv4 `state` compared with a constant-time equality helper and a provider match, the flow consumed before the code exchange, and PKCE S256 with the verifier persisted in the session under the same flow record — with an RFC 7636 test vector in `tests/pkce_flow.rs` and both mismatch and mismatch-passes paths in `tests/state_csrf.rs`.
- **`redirect_uri` is server-derived, never request-derived** (`{base}/oauth/{key}/callback`), which closes the open-redirect class at the wire level, and the `?next` return path is a parsed-URL allowlist (`allowed_return_matches`: scheme, host, port, path-boundary prefix) with `validate_next` failing closed with a 400.
- **Token secrecy is thorough**: `Masked<String>` columns for access/refresh tokens (`models.rs:56-61`), `TokenSet`'s `Debug` redacts both tokens (`provider.rs:25-35`), provider parse errors never interpolate response bodies, and every client-facing error is a fixed string with the detail logged server-side (`server_error`, `resolve_user_client_error`).
- **The create path is atomic and correctly classified**: user + social insert share one transaction with a fresh transaction per username-retry attempt, and any non-username `UniqueViolation` falls through to a real error — no orphan user (OAU-4 verified, including the `classify_sql_error` core dependency it needed). The trusted-email path uses the canonical normalised email and unique placeholders for untrusted/unverified ones, so the email column cannot collide by construction.
- **Disconnect is authorization-scoped**: `oauth_disconnect` takes the user from the session and filters `social_account::USER.eq(user_id)`, so user A cannot disconnect user B's link (the route needs only a valid session — the CSRF half of that story is OAU-3, tracked).
- **The provider trait's contract is small and honest**: `trusts_verified_email` defaults to false, GitHub's file documents and implements the untrusted public-email path, and each provider file carries the quirks it encodes.

## Clean areas checked

- **ORM-not-raw-SQL**: the only two `sqlx::query` hits in the crate are inside the `#[cfg(test)]` module (`policy.rs:374`, `:409`; module gated at `policy.rs:331`); production code goes through `AuthUser`/`SocialAccount`/`AuthToken` managers only.
- **No `unsafe`** in the crate; the only production panic sites are boot-time construction (`http.rs:37`, the shared client `expect`) — every other `unwrap`/`expect` is in tests.
- **Constant-time compare**: `ct_eq` used for the state comparison, not `==` (`routes.rs:406-417`, unit-tested at `:419-459` including length-mismatch cases).
- **Callback error handling**: provider `error` query param logs at info and redirects to the login page — no echo of provider text to the client; unknown provider → 404 naming the key only.
- **Token-mode knowledge in docs/on_ready**: `on_ready` warns when token mode stores tokens without a mask keyring, and info-logs when the mode is active; the gaps4 #93 trap is warned at the callback.
- **Plugin contract**: `dependencies() = &["auth", "sessions"]` with the framework's boot check making a missing dep a clear startup error (review_3/codex 01, gaps4 #8).
- **Feature flags**: no serializer/REST dependency; the plugin compiles without umbral-rest.
- **Cargo.toml direction**: depends on the `umbral` facade (+ umbral-auth/umbral-sessions for the two shared primitives, as declared dependencies) — no core-internal imports.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps3 #28` (= `audit_2` OAU-3) — `POST /oauth/{provider}/disconnect` has no in-plugin CSRF guard; protection depends on umbral-security being mounted. `[open]`
- `audit_2` OAU-5 — state validation and consumption are not atomic (two concurrent callbacks can both read the flow); provider's single-use `code` blocks the replay, deferred. `[open]`
- `gaps2 #79` — refresh-token *exchange* deferred (tokens are stored and refreshed on re-login; no refresh job). `[open]`
- `gaps5 #9` — generic OIDC discovery/JWKS/ID-token verification and SAML deferred (Google/GitHub built-ins only). `[open]`
- `audit_2` OAU-1 — callback error response no longer renders the internal DB error string; verified fixed. `[fixed/verified held]`
- `audit_2` OAU-2 — provider-trust gate against auto-link takeover (`trusts_verified_email`). Verified; OAUTH-1 is the unimplemented second half of its own recommendation, hence a numbered finding rather than a "held" item. `[fixed/verified held]`
- `audit_2` OAU-4 — user + social account create is one transaction with classified unique-violation retries. Verified. `[fixed/verified held]`
- `gaps2 #74` — state + PKCE shipped; both verified in `routes.rs` and tests. `[fixed/verified held]`
- `gaps4 #8` — sessions declared in `dependencies()`; verified at `lib.rs`. `[fixed/verified held]`
- `gaps4 #93` — SPA token-mode traps (no allowlisted `?next` → no bearer token; fragment delivery) documented and warned at the callback; verified in `routes.rs:328-335` (fragment) and `:336-349` (warning). `[fixed/verified held]`
- `review_3/codex 01` — oauth missing the sessions dependency; verified fixed (same as gaps4 #8). `[fixed/verified held]`

## Out-of-scope notes

- `plugins/umbral-auth/src/extractors.rs:105-126` — `resolve_identity`/`identity_from_session`/`identity_from_bearer` re-filter `IS_ACTIVE`; that read-time filtering is what bounds OAUTH-2's blast radius. The auth reviewer should confirm read-time filtering is the intended contract (vs. a gate on every login surface) so the OAUTH-2 fix lands in the right layer.
- `plugins/umbral-permissions/src/middleware.rs:200-240` — reads the raw `current_user_id_str` and compensates for its missing `is_active` knowledge with `pre_perm_check`; noted so the permissions reviewer doesn't double-report the primitive (it cannot know the user model by design).
- `plugins/umbral-sessions/src/lib.rs:833-861` — `current_user_id_str` is a user-agnostic primitive with no `is_active` concept; the OAuth gate in OAUTH-2 should live at the login surface, not here. Cross-filed in the sessions review as an out-of-scope note.
