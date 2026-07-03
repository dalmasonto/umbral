# Security audit — `plugins/umbral-auth/`

Scope: the crown-jewel authentication plugin. Every file under `plugins/umbral-auth/src/` was read in full. Focus areas: password hashing, brute-force throttling, user enumeration, session fixation, staff/superuser gating, reset/verify token hygiene, Masked PII, argon2 worker starvation.

---

## A. Executive summary

The core cryptographic primitives are sound: Argon2id at the OWASP-minimum parameters with a fresh per-hash salt, constant-time verify via the `argon2` crate, bearer/reset/verify secrets that are hashed-at-rest (SHA-256) with 256-bit entropy, single-use time-boxed challenges consumed inside transactions, and post-reset "log out everywhere" revocation. Password validation and rate-limiting are genuinely secure-by-default (opt-out, not opt-in). Enumeration-safe design is visible and deliberate across the password-reset / resend / verify surfaces (uniform `202`, uniform `InvalidChallenge`).

The three most urgent problems are all about the *reachability* of the brute-force defenses, not their internal correctness. **(1)** The client IP that keys every throttle bucket is taken from the leftmost `X-Forwarded-For` hop with **no trusted-proxy validation** — an attacker who can reach the app off-proxy rotates that header per request and bypasses login, register, and email-action throttling completely (`auth_routes.rs:117`). **(2)** `authenticate` returns early for unknown/inactive users *without running Argon2*, so an existing active username costs ~30-50 ms more than a non-existent one — a timing oracle for username enumeration (`lib.rs:1151-1160`). **(3)** The `register` endpoint returns `409` on a duplicate username/email vs `201`/`400` otherwise, a direct existence oracle for a 10M-user PII database (`auth_routes.rs:522`). Secondary but real: argon2's 19 MiB-per-hash cost is unbounded in concurrency (memory-amplification DoS once the throttle is bypassed), several JSON error paths echo raw internal error strings to the client, 6-digit verify codes are weak, reset tokens ride in a query string, and the default `ConsoleMailer` prints plaintext codes/reset links to stderr.

Could **not** verify (out of scope, cross-crate): whether `umbral_sessions::login_user_id` actually rotates the session id on login (the documented session-fixation defense lives in `umbral-sessions`), whether the production host-guard that makes `reset_url_base`'s `Host` trust safe is actually mounted in `umbral-core::app`, and the eviction/memory behavior of the core `RateLimiter` store.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | Brute-force / rate-limit | `auth_routes.rs:117-134` | Throttle key derives client IP from the **leftmost, client-controlled** `X-Forwarded-For` hop with no trusted-proxy count/validation | Attacker rotates `X-Forwarded-For` per request → unlimited distinct buckets → login/register/email-action throttles fully bypassed; no account lockout backstop | Take the Nth-from-right hop per a configured trusted-proxy count, or read a proxy-set `X-Real-IP` only; strip inbound XFF at the edge; document the requirement | deferred: needs framework trusted-proxy config |
| 2 | MEDIUM | User enumeration (timing) | `lib.rs:1151-1166` | `authenticate` returns `InvalidCredentials` for unknown/inactive users **before** calling `verify_password_async`; only real active users pay the ~30-50 ms Argon2 cost | Timing side channel distinguishes registered-active usernames from non-existent ones | Always run one Argon2 verify against a fixed dummy hash when the user lookup misses (constant-work path), then return the same error | ✅ done |
| 3 | MEDIUM | User enumeration (status) | `auth_routes.rs:520-528`, `form_routes.rs:306-315` | `register`/`signup` return `409 Conflict` ("unique") for an existing username/email, distinct from `201`/`400` | Existence oracle over usernames AND emails at signup for a 10M-user PII base | Return a uniform `202`/generic success and drive account activation through the enumeration-safe email-verification flow; or rate-limit + CAPTCHA the signal | deferred: API-contract redesign; not behavior-preserving |
| 4 | MEDIUM | DoS / resource exhaustion | `lib.rs:996-1014`, `1092-1116` | Argon2 hashing (19 MiB each) is offloaded to `spawn_blocking` with **no concurrency cap**; every login/register/reset spawns one | Once throttle is bypassed (finding #1), a flood of valid-username logins saturates the blocking pool and balloons memory (512 threads × 19 MiB ≈ 10 GB) | Gate hashing behind a bounded `tokio::sync::Semaphore`; return `503` when saturated rather than OOM | deferred: concurrency-cap default is framework decision |
| 5 | MEDIUM | Error handling / info leak | `auth_routes.rs:521-527, 583-586, 594-598, 657-661` | JSON error responses echo `format!("{e}")` — the raw `AuthError`/sqlx Display — in the `detail` field on `create_failed`, `token_failed`, `session_failed`, `lookup_failed` | Leaks DB driver/schema/internal error text to unauthenticated clients | Log the detailed error server-side (`tracing::error!`); return a static generic `detail` to the client | ✅ done |
| 6 | MEDIUM | Token hygiene | `challenge.rs:358`, `auth_routes.rs:181-195` | Password-reset token is embedded in a URL **query string** (`?token=…`) | Query strings leak via proxy/access logs, browser history, and `Referer` on the reset page's sub-resources | Deliver the token in the URL **path segment** (or a POST body / fragment); ensure the reset page loads no third-party resources | deferred: reset-URL contract + page-routing change |
| 7 | LOW-MEDIUM | Secrets in logs | `mailer.rs:110-114` | Default `ConsoleMailer` `eprintln!`s the full email body (plaintext verify code / reset link) to stderr in **all** environments | If a mailer isn't wired in prod, live reset tokens and codes land in stdout/stderr log aggregation | Gate the body print to Dev/Test; in non-Dev, log only that a mail *would* have been sent (recipient hash), never the secret | ✅ done |
| 8 | LOW | Token entropy | `challenge.rs:50-53`, `challenge.rs:278-286` | Email-verification code is a 6-digit number (~20 bits); brute-force resistance rests entirely on the 5-attempt cap + email-action throttle | With throttle bypassed (#1), online guessing of the current code becomes feasible over time (fresh code per resend, 5 guesses each) | Raise to 8 digits or an alphanumeric code; keep the per-challenge attempt cap; ensure the throttle is not IP-forgeable (#1) | deferred: changes public code format; hardening choice |
| 9 | LOW | Doc vs code (fixed) | `lib.rs:967-986` / `users-and-passwords.mdx:54` | Docs claimed hashes "silently re-hash on next login"; no rehash-on-verify logic exists | Operators may believe parameter upgrades auto-apply; stale weak-parameter hashes persist | Doc corrected (see Docs updated). Optionally implement `needs_rehash` + rehash in `authenticate` | ✅ done |
| 10 | LOW | Doc vs code (fixed) | `auth_routes.rs:117-134` / `users-and-passwords.mdx:224` | Docs claimed the XFF-first-hop scheme "never opens a hole" | Misleads operators into trusting a forgeable throttle key | Doc corrected with a forgery warning (see Docs updated) | ✅ done |

No issues found in: password salting (`lib.rs:968` fresh `SaltString::generate(&mut OsRng)` per hash), constant-time verify (argon2 crate `Error::Password` mapping, `lib.rs:983`), bearer-token hashing-at-rest (`token.rs:119-123`, 256-bit source entropy), reset-token entropy (`challenge.rs:65-69`, 256-bit), transactional challenge consumption (`challenge.rs:296-320`, `435-457`), post-reset revoke-all (`challenge.rs:463-472`), staff/superuser default-deny (`lib.rs:228-235`), atomic attempt-counter increment (`challenge.rs:204-210`), SQL injection (all row access goes through the ORM / parameterized predicates), open-redirect defense (`form_routes.rs:46-136`).

---

## C. Detailed findings (CRITICAL / HIGH)

### Finding 1 — HIGH — Throttle key uses forgeable `X-Forwarded-For` leftmost hop

**Vulnerable code** (`auth_routes.rs:117-134`):

```rust
pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {   // <-- leftmost = client-supplied
            let ip = first.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) { /* ... */ }
    "unknown".to_string()
}
```

Every throttle bucket is keyed on this value: login on `client_ip + "\0" + username` (`throttle.rs:219`), register on `client_ip`, email-actions on `client_ip + "\0" + email`. The **leftmost** `X-Forwarded-For` entry is whatever the client sent; a correctly-configured proxy *appends* to XFF, it does not clear a forged inbound value. umbral applies no trusted-proxy count.

**Attack scenario.** Attacker runs a credential-stuffing list against one victim account:

```
POST /api/auth/login   X-Forwarded-For: 1.2.3.4      {username: victim, password: guess1}
POST /api/auth/login   X-Forwarded-For: 1.2.3.5      {username: victim, password: guess2}
POST /api/auth/login   X-Forwarded-For: 1.2.3.6      {username: victim, password: guess3}
...
```

Each request lands in a distinct `(ip, username)` bucket, so `login_throttle_check` never returns `false`. The documented "5 / 5 min" limit becomes unlimited. Same technique defeats register (mass signup) and email-action (online code guessing / email bombing) throttles. There is no account-lockout fallback that would catch this. The 10M-user, sensitive-PII context makes credential stuffing the primary threat, and this defense is the only in-framework brake on it.

**Corrected approach** — derive the IP from the right-hand side of XFF using an operator-configured hop count, and prefer a proxy-set header the edge is known to overwrite:

```rust
/// `trusted_proxies` = number of proxies the operator runs (from settings).
/// The client-controlled hops are the *leftmost* ones; the real client is the
/// entry `trusted_proxies` from the right.
pub(crate) fn client_ip(headers: &HeaderMap, trusted_proxies: usize) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let hops: Vec<&str> = xff.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if !hops.is_empty() {
            // Right-anchored: the last `trusted_proxies` entries were added by our
            // own infra; the one just left of them is the real client.
            let idx = hops.len().saturating_sub(trusted_proxies + 1);
            return hops[idx].to_string();
        }
    }
    if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let ip = real.trim();
        if !ip.is_empty() { return ip.to_string(); }
    }
    "unknown".to_string()
}
```

Additionally: require the edge proxy to strip inbound `X-Forwarded-For` / `X-Real-IP`, and document that off-proxy exposure disables throttling.

---

### Finding 2 — MEDIUM (elevated by #1) — Username-enumeration timing oracle in `authenticate`

**Vulnerable code** (`lib.rs:1151-1166`):

```rust
let Some(user) = user else {
    return Err(AuthError::InvalidCredentials);   // <-- fast: no Argon2
};
if !user.is_active() {
    return Err(AuthError::InvalidCredentials);   // <-- fast: no Argon2
}
if verify_password_async(plaintext, user.password_hash()).await? {   // <-- ~30-50 ms
    Ok(user)
} else {
    Err(AuthError::InvalidCredentials)
}
```

A login attempt for a registered, active username performs a 19 MiB Argon2 verify (~30-50 ms). An attempt for a non-existent or inactive username returns after only a DB SELECT. The measurable latency gap lets an attacker enumerate valid usernames despite the identical `401` body and the identical throttle response. (Because the throttle in #1 is bypassable, an attacker can gather clean timing samples at will.)

**Corrected approach** — always spend one Argon2 verify against a fixed dummy hash on the miss path:

```rust
// Precomputed once: hash_password("*").unwrap()
static DUMMY_HASH: Lazy<String> = Lazy::new(|| hash_password("umbral-timing-dummy").unwrap());

let Some(user) = user else {
    let _ = verify_password_async("x", &DUMMY_HASH).await;   // burn equivalent time
    return Err(AuthError::InvalidCredentials);
};
if !user.is_active() {
    let _ = verify_password_async("x", &DUMMY_HASH).await;
    return Err(AuthError::InvalidCredentials);
}
```

---

### Finding 3 — MEDIUM — `register` status code leaks account existence

**Vulnerable code** (`auth_routes.rs:520-528`):

```rust
Err(e) => {
    let msg = format!("{e}");
    let status = if msg.to_lowercase().contains("unique") {
        StatusCode::CONFLICT           // 409 = "this username/email already exists"
    } else {
        StatusCode::BAD_REQUEST
    };
    err(status, "create_failed", msg) // <-- also leaks raw error (finding #5)
}
```

An attacker submits `POST /register` with a candidate email and a throwaway username: a `409` means the email is registered, anything else means it is not. This is a clean existence oracle over the entire user base, and it is explicitly documented in the OpenAPI (`auth_routes.rs:303`). The password-reset and verify flows went to great lengths to avoid enumeration (uniform `202`); the register endpoint undoes that for the same identifiers. Note this pairs with finding #5: the `409`/`400` body also carries the raw DB error string.

**Corrected approach** — do not distinguish existence at signup; register optimistically and confirm via the enumeration-safe email flow, or at minimum return a generic body and lean on email verification:

```rust
Err(e) => {
    tracing::warn!(error = %e, "register failed");            // detail server-side only
    // Uniform response regardless of whether the clash was username or email.
    // Preferred: 202 + "check your email to finish signing up", and only email a
    // real, unregistered address; a taken address gets a "you already have an
    // account" email out-of-band. Never branch the HTTP response on existence.
    err(StatusCode::CONFLICT, "create_failed", "could not create account")
}
```

---

## D. Blind spots (could not verify from `plugins/umbral-auth/` alone)

1. **Session fixation.** `login_with_request` → `umbral_sessions::login_user_id` (`session_user.rs:113`). The doc-comment (`session_user.rs:99-103`) claims the anonymous session is destroyed before the authenticated row is written. That defense lives in `umbral-sessions` (out of scope) — not verified here. If it does *not* rotate the session id, this plugin inherits a session-fixation bug.
2. **Host-header trust for reset URLs.** `reset_url_base` (`auth_routes.rs:181-195`) embeds the request `Host` into the reset link and relies on a production host-guard mounted in `crates/umbral-core/src/app.rs` (Phase 5.95). Not verifiable in this scope; if that guard is absent or misconfigured, this is host-header injection / reset-link poisoning (CWE-640).
3. **RateLimiter store growth.** `Throttle` wraps `umbral::ratelimit::RateLimiter` (`throttle.rs:80`). Whether the per-key timestamp map is bounded/evicted is in `umbral-core`. Combined with forgeable XFF (#1), an unbounded map is a memory-exhaustion vector (one entry per forged IP).
4. **Cookie flags (HttpOnly/Secure/SameSite).** The session cookie is set entirely inside `umbral-sessions::login_user_id`; its attributes are not visible here.
5. **Argon2 blocking-pool sizing.** The actual `max_blocking_threads` and process memory limits are runtime/deployment config, not in this crate (bears on finding #4 severity).
6. **Masked<T> PII.** The audit brief flags "sensitive PII via Masked fields", but `AuthUser` (`lib.rs:247-269`) uses **no** `Masked` fields — `email` is stored/serialized in plaintext (needed for lookup) and returned in `UserOut` (`auth_routes.rs:60-78`). Nothing in this crate misuses `Masked`; any Masked PII lives in downstream app user models, not here.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
- Finding #5: stop echoing `format!("{e}")` in the four JSON error `detail` fields; log server-side, return static text.
- Finding #7: gate `ConsoleMailer`'s body `eprintln!` to Dev/Test only.
- Finding #9/#10: doc corrections — **done** in this pass.
- Finding #2: add the dummy-hash constant-work miss path in `authenticate`.

**Short term (< 2 weeks)**
- Finding #1: rewrite `client_ip` to a right-anchored, trusted-proxy-count-aware derivation; wire the count from settings; document the edge-proxy stripping requirement. This is the highest-value fix.
- Finding #4: bound concurrent Argon2 with a semaphore; shed load with `503`.
- Finding #6: move the reset token from query string to a path segment.
- Finding #8: widen verify codes to 8 digits / alphanumeric.

**Structural (needs design work)**
- Finding #3: redesign signup to be enumeration-safe (optimistic register + email-confirm), a cross-cutting UX + flow change.
- Shared, persistent (Redis-backed) throttle + real account-lockout so the brute-force brake survives horizontal scaling and does not depend solely on an in-memory per-replica map (ties #1).
- Automatic Argon2 `needs_rehash` on successful login to let parameter upgrades roll forward (ties #9).

---

## Docs updated

- `documentation/docs/v0.0.1/auth/users-and-passwords.mdx:54` — Rewrote the hashing Callout. Removed the false "silently re-hash on next login" claim (no rehash-on-verify logic exists in `lib.rs`); stated the concrete Argon2id parameters (19 MiB / 2 / 1, OWASP minimum) and that parameter rotation is manual today. Reason: doc contradicted code (finding #9).
- `documentation/docs/v0.0.1/auth/users-and-passwords.mdx:224` — Replaced the "it never opens a hole" reassurance about the `X-Forwarded-For` first-hop scheme with a warning that the leftmost hop is client-forgeable and, absent a trusted edge that strips inbound XFF, lets an attacker rotate the throttle key to bypass all three limiters. Reason: doc actively misrepresented the security property (findings #1, #10).
