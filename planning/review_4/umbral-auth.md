# review_4 · umbral-auth

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 13,213 LOC (`plugins/umbral-auth` — src 7.0k + tests 6.2k, 25 test files) |
| **Reviewer** | Claude (umbral-auth) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 2 high · 1 medium · 2 low · 2 info — 7 new, 5 known |

## Summary

- The plugin ships users + sessions bridge, login/logout/register/change-password, email-verify and password-reset flows, argon2 hashing, bearer tokens, throttling, and the `RequireAuth`/`RequireStaff`/`LoggedIn` extractor family. The cryptographic core is sound and the enumeration discipline on the async surfaces (uniform 202/400, dummy-verify constant work, normalize-at-every-boundary) is deliberate and largely complete.
- The two HIGH findings are both *drift inside the crate's own hardening*: AUTH-1 — the two password-rotation paths (`change_password`, `reset_password`) call the **sync** argon2 helper inline in async handlers, which the same file's docstring forbids and which bypasses the `HASH_GATE` DoS bound; AUTH-2 — throttle keys are built from the **raw** username/email while every other boundary normalizes (gaps3 #33), so case/whitespace variants rotate into fresh buckets and defeat the login brute-force throttle.
- AUTH-3 (MEDIUM): bearer tokens have no expiry and login mints a brand-new row every time — every token ever issued stays valid until password *reset* (password change, by design, revokes nothing — gaps5 #15).
- Two small items: AUTH-4 — both login surfaces discard every non-`Overloaded` error into "invalid credentials" with no server log (sibling paths in the same file log); AUTH-5 — the `FnAuthentication` Basic-auth doc example in core cannot compile (routed lead, confirmed).
- Fix order: AUTH-2 (normalize the two key builders), then AUTH-1 (two `.await` call-site swaps), then AUTH-3 (add `expires_at` + filter), then the LOW/INFO notes.
- All historic auth findings from audit_2/review_3 re-verified as held (store-aware session reads incl. `max_session_age`, 60 s `last_used` coalesce, dummy-hash timing burn, hash gate, `client_ip` doc correction) — details in Known.

## Findings

### AUTH-1 · HIGH · correctness / async · self-verified
**The two async request paths that rotate a password hash (`change_password`, `reset_password`) call the synchronous argon2 helper inline — pinning a tokio worker for ~100 ms and bypassing the `HASH_GATE` bound the crate built for exactly this — while the same file's docstring says async handlers must not do this.**

- **Where**: `plugins/umbral-auth/src/challenge.rs:434` (`change_password`), `plugins/umbral-auth/src/challenge.rs:466` (`reset_password`); the contract at `plugins/umbral-auth/src/lib.rs:1408-1415`; the gate at `plugins/umbral-auth/src/lib.rs:1375-1406`.
- **Evidence**:
  ```rust
  // challenge.rs:433-434 — inside `pub async fn change_password(…)`
  // 3. Rotate the hash.
  let hash = crate::hash_password(new)?;          // sync argon2, inline
  ```
  ```rust
  // lib.rs:1414-1415 — doc on `hash_password_async`
  /// other tasks. **Async request handlers must use this**; the sync
  /// [`hash_password`] remains for non-async / CLI / test callers.
  ```
  ```rust
  // challenge.rs:466 — inside `pub async fn reset_password(…)`
  let hash = crate::hash_password(new_password)?; // same bypass
  ```
- **Impact**: both fns are called directly from axum handlers (`auth_routes.rs:353` `change_password_h` → `crate::change_password`; `auth_routes.rs:1050` `password_reset_h` → `crate::reset_password`; the form reset handler likewise). The sync path never touches `with_hash_gate`, so the invariant stated at `lib.rs:1331-1335` ("waiting is cheap and memory stays bounded no matter how deep the queue") does not hold here. Concrete scenario: any self-registered account (register is part of the default routes; `change-password` has **no throttle** — unlike login/register/email actions) fires N concurrent `POST {prefix}/change-password` requests with its own correct current password. Each passes the *gated* verify (offloaded, fine) and then runs a raw inline argon2: ~19 MiB + ~100 ms CPU pinned on an async worker, unbounded by the gate. With N ≥ the tokio worker count the whole runtime stalls for the duration (no other connection is polled), and peak memory grows ~19 MiB × N instead of the capped `cap × 19 MiB` audit_2 #4 promised. `reset_password`'s valid-token path has the same inline hash.
- **Refutation attempt / verification**: re-read both fns end-to-end; confirmed the hash is computed outside any `spawn_blocking`/gate (`grep` of `src/` shows `with_hash_gate` is used only by the two `*_async` wrappers); confirmed `hash_concurrency_gate.rs` exercises only the free `*_async` helpers, so nothing in the suite would fail if these routes kept the sync path.
- **Fix**: swap both call sites to `crate::hash_password_async(new).await?` (already gated + offloaded; CLI/tests keep the sync helper). The type is `Result<String, AuthError>` either way, so it is a two-line change; extend the gate test to drive `change_password` concurrently.

### AUTH-2 · HIGH · security · self-verified
**Throttle keys are built from the raw, un-normalized username/email, while the account itself is matched case-insensitively — rotating case (or adding whitespace) mints a fresh budget for the same account, bypassing the login brute-force throttle and the email-action throttle.**

- **Where**: `plugins/umbral-auth/src/throttle.rs:219-221` (`login_key`), `:253-255` (`email_action_key`); callers `auth_routes.rs:828` (JSON login), `:972/:1000/:1034` (verify/resend/forgot), `form_routes.rs:211`, `:333/:360/:389`; the normalization that the same request undergoes at `lib.rs:1643`.
- **Evidence**:
  ```rust
  // throttle.rs:219-221
  fn login_key(ip: &str, username: &str) -> String {
      format!("{ip}\0{username}")            // raw — no normalize_username
  }
  ```
  ```rust
  // auth_routes.rs:828 — the raw body field is the key
  if !crate::login_throttle_check(&ip, &body.username) {
  ```
  ```rust
  // lib.rs:1643 — the same value IS normalized two lines later in authenticate
  let ident = normalize_username(username);
  ```
- **Impact**: attacker credential-stuffs a known account. `victim` → 5 tries, the 6th returns 429; `Victim` keys a different bucket (`…\0Victim`) and gets 5 more; `authenticate` normalizes it to the same stored row, so the guesses land. `VICTIM`, `vIcTiM`, ` victim`, `victim ` each mint another 5 — for any ≥6-char username that is ≥64 variants (× whitespace variants), i.e. effectively unlimited online guesses per window with only the per-attempt argon2 cost as friction; there is no account-lockout backstop. `email_action_key` has the same flaw, so the 5/hour email-action budget (verify-code guessing, resend/forgot email-bombing) is equally rotate-able.
- **Refutation attempt / verification**: checked the body structs for serde normalization (`LoginIn` at `auth_routes.rs:120-123` is plain `String` fields; the form structs likewise), checked both surfaces pass the raw field, and checked `login_throttle_clear` is raw-keyed too so it cannot compensate. `normalize_username`/`normalize_email` (trim+lowercase) was applied at every *write* and *lookup* boundary by gaps3 #33/#34 — the throttle key builders are the one sibling that sweep missed.
- **Fix**: normalize inside the key builders (`login_key` → `crate::normalize_username(username)`, `email_action_key` → `crate::normalize_email(email)`) so every current and future caller gets the canonical key; add a regression test asserting `Victim` shares `victim`'s budget.

### AUTH-3 · MEDIUM · security / lifecycle
**Bearer tokens are minted fresh on every successful login and carry no expiry — every token ever issued stays valid until a password reset or user deletion, and one row accumulates per login.**

- **Where**: mint at `plugins/umbral-auth/src/auth_routes.rs:867`; model `plugins/umbral-auth/src/token.rs:67-92` (no `expires_at` column); lookup `token.rs:158-165` (no time check).
- **Evidence**:
  ```rust
  // auth_routes.rs:867
  let (_token_row, plaintext) = match AuthToken::create_for(&user, "login").await {
  ```
  ```rust
  // token.rs:88-91 — the row's lifetime columns; an expiry is absent
  pub created_at: DateTime<Utc>,
  /// Last time this token authenticated a request. NULL until
  /// the first successful lookup.
  pub last_used_at: Option<DateTime<Utc>>,
  ```
- **Impact**: the credential returned in the login response body (`LoginOut.token`) never expires. `logout` revokes only the presented token by design; every login labels its token `"login"` and mints anew, so nothing is reused or superseded; the table grows one row per successful login per user with no cleanup path and no cap. A token captured once (access log, backup, shared machine, an ex-device) remains a live credential for years unless the owner's password is *reset* — note `change_password` deliberately revokes nothing (`challenge.rs:415-416`), so the everyday password change does not close it. A workaround exists (password reset revokes all tokens; `last_used_at` supports a manual prune), hence MEDIUM rather than HIGH.
- **Refutation attempt / verification**: read `AuthToken::lookup` and the model in full — no expiry column, no expiry check, no reuse path; confirmed the login handler mints unconditionally per successful login (line 867) and that `revoke` is only invoked by logout/reset paths (`lib.rs:1274-1290`, `challenge.rs:498-507`).
- **Fix**: add a nullable `expires_at` to `AuthToken` + a predicate in `lookup` (and an `AuthPlugin` knob for the default lifetime), then either reuse the caller's most recent live `"login"` token or prune expired rows. The device-inventory class is tracked (gaps5 #13); this specific defect (no expiry + per-login mint) is not — see Known.

### AUTH-4 · LOW · correctness / observability
**Both login surfaces collapse every non-`Overloaded` `authenticate` error into "invalid credentials" with no server-side log — a DB outage reports as a wave of 401s and leaves no signal in the log.**

- **Where**: `plugins/umbral-auth/src/auth_routes.rs:846-853` (JSON), `plugins/umbral-auth/src/form_routes.rs:217-223` (form).
- **Evidence**:
  ```rust
  // auth_routes.rs:846-853
  Err(_) => {
      // The failed attempt is already counted by the check above.
      return err(
          StatusCode::UNAUTHORIZED,
          "invalid_credentials",
          "username or password is incorrect",
      );
  }
  ```
- **Impact**: `authenticate` propagates `AuthError::Db/Query` (lib.rs:1656-1659), `AuthError::Runtime` (gate), and `AuthError::PasswordHash` (a malformed/foreign stored hash, e.g. a ported Django hash) — all land here and are discarded unlogged; form `do_login` does the same. During a DB fault every user is told "invalid credentials" and the operator's log shows nothing to distinguish it from credential stuffing; clients retry credentials instead of backing off. The sibling arms in the same file already do it right (log + static text): `/me` `:948-956`, token mint `:869-877`, session write `:884-891`, register `:796-800`. This is CLAUDE.md's "an error you can't reproduce is one you'll never fix" pattern.
- **Fix**: match on the variant — `InvalidCredentials` stays a silent 401; everything else gets `tracing::error!` + 500 (`"could not complete login"`), mirroring register.

### AUTH-5 · LOW · docs · routed lead
**The `FnAuthentication` doc example for HTTP Basic auth against umbral-auth cannot compile: `umbral_auth::authenticate` is generic over the user model and the example never pins `U`, so `U` is uninferable at that call site.**

- **Where**: `crates/umbral-core/src/auth_contract.rs:269-282` (the doc example); the real signature at `plugins/umbral-auth/src/lib.rs:1633-1639`. Filed here per the contract-owner rule (umbral-auth's public surface); the comment itself lives in umbral-core.
- **Evidence**:
  ```rust
  /// // HTTP Basic Auth against umbral-auth:
  /// RestPlugin::default().authenticate(FnAuthentication::new(|headers| async move {
  ///     let (user, pass) = umbral::auth::parse_basic_credentials(&headers)?;
  ///     let auth_user = umbral_auth::authenticate(&user, &pass).await.ok()?;
  ///     Some(Identity::user(auth_user.id).with_staff(auth_user.is_staff))
  /// }));
  ```
  vs. `pub async fn authenticate<U>(username: &str, plaintext: &str) -> Result<U, AuthError> where U: UserModel + FromRow<SqliteRow> + FromRow<PgRow> + HydrateRelated + Unpin`.
- **Impact**: copying the example gives `E0282: type annotations needed`; `auth_user.id` field access does not drive inference. The block is ```` ```ignore ````, so rustdoc never compiles it and the staleness is invisible to CI. (The first example, lines 271-274, does check out against `current_user(&HeaderMap) -> Result<Option<AuthUser>, SessionError>` — only the Basic-auth one is wrong.)
- **Fix**: pin the type in the example (`umbral_auth::authenticate::<umbral_auth::AuthUser>(&user, &pass)` or an annotated `let`). Routing note in Out-of-scope: the core slice that owns `auth_contract.rs` makes the edit.

### AUTH-6 · INFO · security
**`password-reset` is the only auth POST with no throttle at all.**
- **Where**: `plugins/umbral-auth/src/auth_routes.rs:1050` — compare its throttled siblings at `:972`, `:1000`, `:1034`.
- **Observation**: a failed attempt is one indexed `find_active_by_secret` lookup against a 256-bit secret, so the omission is not exploitable today; noted because it is the one endpoint the "throttle every auth POST" convention skips, and a future shorter-token variant would inherit the gap.

### AUTH-7 · INFO · security
**The email-verification code comparison is not constant-time.**
- **Where**: `plugins/umbral-auth/src/challenge.rs:283` — `if hash_secret(code) != challenge.secret_hash`.
- **Observation**: early-exit byte compare, but guessing is capped at 5 attempts against a 20-bit code and the leaked signal concerns SHA-256 digests of candidates (you would have to brute-force digest prefixes far beyond 10^6 work to use it); not exploitable — hygiene note only, a `subtle`-style compare would remove the question.

## Strengths

- **The cryptographic core is textbook and honestly documented.** argon2id at `Params::new(19_456, 2, 1, None)` (OWASP minimum) with a fresh `OsRng` salt per hash, verify mapping `Error::Password` → `false` and erroring only on malformed hashes (`lib.rs:1301-1320`); bearer/reset/verify secrets are 256-bit, hashed at rest via one shared digest implementation (`token.rs:119-123`, `challenge.rs:75-77`).
- **The hash gate is one of the better-reasoned pieces in the framework's plugins** (`lib.rs:1322-1447`): permit acquired *before* `spawn_blocking` so waiters hold only a String, shed at `cap × 8` in-flight with `Overloaded` → 503, override knob; `hash_concurrency_gate.rs` drives 80 concurrent hashes and asserts the shed accounting. (AUTH-1 is the one place its reach is incomplete.)
- **Enumeration discipline on the async surfaces**: uniform 202 for forgot/resend, uniform 400 for verify, silent-on-unknown-email resets, dummy-hash constant-work on both miss paths (`lib.rs:1457-1479`, `:1661-1677` — audit_2 #2 genuinely fixed, not papered over), and `normalize_*` applied at every write + lookup boundary — AUTH-2 is the single key that escaped the sweep.
- **Reset flow hygiene**: single-use challenge consumed inside the same transaction that writes the new hash, uniform `InvalidChallenge` for every failure arm, and post-commit log-out-everywhere revocation that logs at ERROR level without rolling back the password (`challenge.rs:444-510`); the attempt counter is an atomic `update_expr("attempts", F::col("attempts").add(1))`.
- **Extractor consistency (the historic drift class) is closed**: `LoggedIn`, `RequireAuth`, `RequireStaff`, `OptionalIdentity` and the identity resolvers all resolve through the same store-aware session read (`login_required.rs:304-316`), all filter `is_active`, and the fix for the direct-SQL/store-blind read carries an inline explanation of the old bug.
- **Tests are behavioral on the public path**: real rows + real flows in `password_reset.rs` (asserts bearer tokens are revoked on reset through actual token/session rows), `change_password.rs` (rotate, old password fails), `login_required.rs`, `uuid_user.rs`/`custom_user.rs` (non-i64 PKs), plus deterministic injected-clock throttle tests.

## Clean areas checked

- **Raw SQL / ORM discipline**: `grep -rn "sqlx::query" plugins/umbral-auth/src/` → **zero hits**; every read/write goes through ORM managers, `on_tx`, `update_values`, `update_expr`. No DDL in the plugin (migrations come from `Plugin::models()`).
- **Dependency law**: depends only on the `umbral` facade + `umbral-sessions`; the deliberate absence of umbral-tasks/umbral-email deps is documented at `Cargo.toml:71-82` and the `AuthMailer` seam keeps a REST-free mail-free app compiling.
- **Session lifecycle**: login rotates the session (`umbral_sessions::login_user_id` documented fixation defense), logout revokes the presented bearer token + session with both halves always running (`lib.rs:1274-1290`); `SessionAuthentication` yields anonymous on any error (`session_user.rs:163-164`); session `user_id` parsed defensively for non-i64 PKs.
- **Token integrity**: SHA-256-at-rest, unique digest index, FK cascade on user delete, `touch_last_used` coalesced at 60 s — the review_3/codex #07 fix holds (`token.rs:185-209`).
- **Challenge edge cases**: expired-but-unused race handled by an explicit `is_live()` after the SQL filter (`challenge.rs:124-157`); attempt cap burns the challenge; `find_active_for_user` picks newest by `created_at DESC`.
- **Client-IP resolution**: `client_ip` delegates to `umbral::settings::client_ip` (trusted-proxy-hops, `(hops+1)`-th from the right) with a fail-closed `"unknown"` fallback, and the docstring now states the policy accurately (`auth_routes.rs:187-204`) — audit_2 H9 fix held; audit_2 H9-era fail-closed sharing noted as BY-DESIGN.
- **Body/limit and open-redirect hygiene**: `JsonOrForm` caps bodies at 1 MiB; form surface guards `?redirect=` via `safe_path`/`same_site_referer_path`; register validates email format via the ORM's shared validator and the password policy at the boundary only.
- **Hygiene**: no committed DBs/artifacts under the crate (src/, tests/, templates/, README, CHANGELOG only); 25 test files / 6.2k test LOC against 7.0k src.

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2 plugin-auth #3` — register/signup `409` duplicates are a username/email existence oracle; still branched via `msg.to_lowercase().contains("unique")` (`auth_routes.rs:802`, `form_routes.rs:308`) though the response body is now static `[open]` — deferred there as an API-contract redesign.
- `audit_2 plugin-auth #6` — password-reset token travels in the URL query string (`challenge.rs:358`) `[open]`.
- `audit_2 plugin-auth #8` — 6-digit (~20-bit) verification code; held up by the attempt cap + email-action throttle only (and that throttle is exactly what AUTH-2 circumvents) `[open]`.
- `gaps5 #13 [tf#226]` — session/device inventory + revoke-all/revoke-one APIs missing `[open]`; AUTH-3 is a distinct, untracked specific defect (token rows have **no expiry** and login mints per call) that this class-level entry does not absorb — cross-check AUTH-3.
- `gaps5 #15 [tf#228]` — change-password does not rotate the session or revoke other credentials `[open]`; the reasoning ("user proved knowledge of the current password") is documented in `change_password`'s docstring (`challenge.rs:415-416`).
- `audit_2 plugin-auth #1 + H9` — forgeable XFF throttle key; fixed via `trusted_proxy_hops` and `client_ip` delegation `[fixed/verified held]`. BY-DESIGN residual: with the default `hops=0` all unproxied callers share one `"unknown"` bucket (review_3's per-username-global/global-signup note is downstream of this core resolver) — noted, not re-filed.
- `audit_2 plugin-auth #2` — username-enumeration timing oracle in `authenticate`; dummy-hash constant-work fix present at `lib.rs:1661-1677` `[fixed/verified held]`.
- `audit_2 plugin-auth #4` — unbounded argon2 concurrency; `with_hash_gate` present and tested — except on the two paths AUTH-1 covers `[fixed/verified held]`.
- `audit_2 plugin-auth #5` — raw error text echoed to clients; static bodies + server-side logs in place (`auth_routes.rs:796-807`, `:948-956`, `:869-877`) `[fixed/verified held]`.
- `audit_2 plugin-auth #7` — ConsoleMailer body gating to Dev/Test `[fixed/verified held]`.
- `audit_2 plugin-auth #9` — argon2 parameters are hardcoded (19 MiB/2/1) and rotation is manual; docs corrected, and the optional `needs_rehash`-on-login upgrade path remains an acknowledged, tracked gap there `[fixed/verified held]`.
- `review_3 (claude)` — store-blind session read / `max_session_age` unenforced on the extractor surface; now routed through `umbral_sessions::current_user_id_str`, which applies both, with the history inline (`login_required.rs:304-316`) `[fixed/verified held]`.
- `review_3 (claude)` — `client_ip` doc drift ("first hop"); docstring describes the trusted-proxy policy accurately `[fixed/verified held]`.
- `review_3/codex #07` — bearer `last_used_at` write amplification; 60 s coalesce in place `[fixed/verified held]`.

## Out-of-scope notes

- `crates/umbral-core/src/auth_contract.rs:269-282` — `FnAuthentication`'s Basic-auth example names `umbral_auth::authenticate` without pinning its generic `U` (AUTH-5); the fix belongs to the core slice that owns this file.
- `crates/umbral-core/src/settings.rs::client_ip` — returning `None` for `trusted_proxy_hops == 0` (no peer-IP fallback) is what makes the auth throttles share the `"unknown"` bucket by default; if a peer-IP fallback is ever wanted, it is a core change (review_3 raised the lockout side; audit_2 H9 chose fail-closed).
- `plugins/umbral-sessions` — the session-fixation defense (anonymous session destroyed before the authenticated row is written) lives in `login_user_id`; not verifiable from this crate (audit_2 blind spot #1) — for the sessions reviewer to confirm.
- `crates/umbral-core/src/ratelimit.rs` module doc still claims keys are "never swept globally" while the code runs the `SWEEP_EVERY = 1000` sweep; same stale-doc note as the core-5 slice — cross-check there, do not duplicate.
