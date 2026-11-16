# review_4 · umbral-email

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 2,208 LOC (`plugins/umbral-email`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 2 medium · 2 low · 0 info — 4 new, 1 known |

## Summary

- Transactional email for umbral: one `send(&EmailMessage)` with four backends (console / SMTP via lettre+rustls / Resend-SendGrid HTTP API / user-supplied `EmailBackend`), settings-driven selection, plus the optional `auth_mailer()` adapter for umbral-auth. 2,208 LOC including a genuinely good test set.
- The security posture is strong and verified in code: the CRLF/header-injection guard has its own test file, SMTP uses rustls STARTTLS with full certificate verification and no bypass knob, the console backend is fail-closed outside Dev/Test, and both credential-bearing structs have redacting `Debug` impls.
- The weakest area is settings plumbing: backend selection reads a second, `from_env`-only view of settings while `send()` reads ambient settings, so programmatically-supplied settings silently select the console backend in production — every send then fails (EMAIL-4; fix first, it is silent prod mail loss).
- The HTTP API path is the only outbound call in the plugin ecosystem built per-send with **no timeout** and no shared client (EMAIL-2); every sibling (oauth, analytics) shares one timed `reqwest::Client`.
- The console fail-closed error text advertises an opt-in (`UMBRAL_EMAIL_BACKEND=console`) that the guard does not honor (EMAIL-1); the SMTP transport is also rebuilt per send (EMAIL-3, minor).

## Findings

### EMAIL-1 · LOW · docs/ergonomics
**The `ConsoleBackendInProduction` doc and Display tell the operator to set `UMBRAL_EMAIL_BACKEND=console` to opt in, but the guard only checks `Environment::Dev|Test` — following the documented remediation loops back to the same error.**
- **Where**: `plugins/umbral-email/src/lib.rs:368-374` (variant doc), `:426-432` (Display), `:730-741` (guard), `:552-556`, `:624-632` (where the env var is actually read)
- **Evidence**:
  ```rust
  /// set `UMBRAL_EMAIL_BACKEND=console` explicitly if you understand
  /// the risk and are intentionally forcing console mode.
  ConsoleBackendInProduction,
  ```
  ```rust
  let env = umbral::settings::get_opt().map(|s| s.environment);
  let is_dev_or_test = matches!(
      env,
      Some(umbral::Environment::Dev) | Some(umbral::Environment::Test)
  );
  if !is_dev_or_test {
  ```
- **Impact**: In a Prod-environment staging box, the operator follows the error message verbatim (`UMBRAL_EMAIL_BACKEND=console`), and `load_config` (`:624-632`) happily selects `BackendKind::Console` — but the guard consults only the ambient `Environment`, so every `send()` still returns the identical `ConsoleBackendInProduction`. The crate's own user doc states the true requirement (`documentation/docs/v0.0.1/plugins/email.mdx:70`: the env var "and use `Environment::Dev` or `Environment::Test`"), so the error surface is the side that misleads. No test references `UMBRAL_EMAIL_BACKEND` anywhere under `tests/`.
- **Fix**: Either make the documented opt-in real (read `env_forced_console` in the guard, which `send()` can re-derive from the env var) or reword the variant doc and the Display arm to say console is unavailable outside `Dev`/`Test` and point at that requirement. Add the missing test either way.

### EMAIL-2 · MEDIUM · reliability/drift · self-verified
**The API send path builds a fresh default `reqwest::Client` per send with no timeout; a hung provider blocks the awaiting caller indefinitely.**
- **Where**: `plugins/umbral-email/src/lib.rs:1094-1102`
- **Evidence**:
  ```rust
  async fn deliver_api(request: ApiRequest) -> Result<(), EmailError> {
      let client = reqwest::Client::new();
      let response = client
          .post(&request.url)
          .bearer_auth(&request.bearer)
  ```
- **Impact**: `reqwest::Client::new()` uses `ClientBuilder` defaults, which set no total and no connect timeout (verified in the vendored tree: `reqwest-0.12.28/src/async_impl/client.rs:299` `connect_timeout: None`, `:314` `timeout: None`). Trace (static; needs runtime confirmation): an app wires `umbral_email::auth_mailer()` (the `auth` feature); a password-reset handler awaits `send()` → `deliver_api`; the provider accepts TCP but stalls (or blackholes a packet) → `.send().await` never returns and the auth request hangs for as long as the peer holds the socket. The SMTP arm deliberately solved this (`:917-926`, configurable timeout, default 10 s), and both sibling plugins share one timed client (`plugins/umbral-oauth/src/http.rs:28-38`; `plugins/umbral-analytics/src/lib.rs:108-120` with `timeout(10s)` + `connect_timeout(5s)`) — this is the drift instance. Each send also pays a fresh TLS handshake.
- **Fix**: Mirror the oauth/analytics `OnceLock<reqwest::Client>` with `.timeout(...)`/`.connect_timeout(...)` (reusing `smtp_timeout_secs` or a new API-timeout setting), instead of a per-send default client.

### EMAIL-3 · LOW · perf
**The SMTP transport (and thus TCP + TLS) is rebuilt on every send — no connection reuse.**
- **Where**: `plugins/umbral-email/src/lib.rs:910-928`
- **Evidence**:
  ```rust
  let mut transport =
      AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?.port(cfg.smtp_port);
  ```
- **Impact**: Every message pays a full handshake + SMTP dialog setup. Cold-path today (one email per auth action), but once the tracked retry/queue work lands (`gaps5 #53`) a burst of N queued mails would pay N handshakes against the same relay. Note `deliver_smtp` already documents the implicit-TLS upgrade as future work, so this is a deliberate simplification, not an accident — flagged so the queue work remembers it.
- **Fix**: Build the transport once when the config resolves (cache it beside `EmailConfig` in the `OnceLock`), or configure lettre's pool and reuse it.

### EMAIL-4 · MEDIUM · drift/config · self-verified
**Backend selection reads only `Settings::from_env()` and discards its error silently, while `send()` reads ambient settings — the two disagree whenever settings are supplied programmatically, silently selecting Console in Prod.**
- **Where**: `plugins/umbral-email/src/lib.rs:562-567` vs `:730`
- **Evidence**:
  ```rust
  // Re-parse from env / umbral.toml rather than reaching for the
  // ambient `settings::get()`, which isn't on the facade and would
  // panic before `App::build()` runs anyway. `Settings::from_env`
  // is a pure function over env + cwd that the App also calls at
  // boot, so the two views agree.
  let settings = umbral::Settings::from_env().ok();
  ```
  ```rust
  let env = umbral::settings::get_opt().map(|s| s.environment);
  ```
- **Impact**: Trace (static): an app builds `Settings`, inserts `extra["email_smtp_host"]` in code, and calls `App::builder().settings(s)` (`crates/umbral-core/src/app.rs:417`) with `environment: Prod`. `load_config` consults `from_env()` only, sees no host → `BackendKind::Console`; `send()` then consults ambient settings, sees `Prod`, and refuses every send with `ConsoleBackendInProduction` — SMTP was configured but the app cannot mail and the failure only appears at send time. Two sharpeners: the comment's canonicality claim ("the two views agree") is false exactly when `.settings(...)` is used, and it says reaching for settings "isn't on the facade" while the same file calls `umbral::settings::get_opt()` 160 lines later. Separately, a malformed `umbral.toml` makes `from_env()` return `Err`, and `.ok()` discards it with no diagnostic before falling back to Console.
- **Fix**: One resolution helper used by both sites that prefers ambient `settings::get_opt()` (which is initialized before any send can run) and falls back to `Settings::from_env()`, logging rather than discarding its error; update the comment to match.

## Strengths

- **Header-injection guard done right.** `validate_header_value` (`:760-775`) checks subject, from, reply_to and every recipient for CR/LF/NUL before any backend branch — including the API path — and `tests/header_injection.rs` exercises each control character by name. lettre 0.11 does not reject these itself, so this is load-bearing, not ceremonial.
- **TLS posture has no bypass.** `starttls_relay` is lettre's `Tls::Required` with certificate verification, built on rustls + `ring` (no OpenSSL, no `danger_accept_invalid_certs` knob in the tree) — confirmed against vendored lettre 0.11.22.
- **Fail-closed console with settings absent.** `send()` uses `get_opt()` and treats "settings not initialized" as unknown → refuse, so the review_1 BROKEN-6 panic is fixed and `tests/prod_safety.rs` pins it; the refusal error carries no message body.
- **Secret hygiene.** Manual redacting `Debug` on `EmailConfig` (`:496-511`) and `ApiRequest` (`:954-964`); `ApiResponse` carries only the provider's own text.
- **Extensible backend contract that composes with the safety layers.** `EmailPlugin::with_backend` registers a set-once ambient sender, checked first, receiving the resolved + validated message — `tests/custom_backend.rs` asserts the spy sees the resolved message and that a CRLF subject is rejected before reaching it.
- **Feature hygiene**: `api`/`auth` off by default, API-without-feature fails at send time with a pointer rather than at compile time; SMTP-only builds never compile reqwest.
- **Docs match reality** (`documentation/docs/v0.0.1/plugins/email.mdx`) except for the EMAIL-1 error-text conflict; attachments/composition shapes documented exactly as built; `rfc2231` filename encoding in lettre closes the filename-injection vector.

## Clean areas checked

- CRLF/header injection — validated for every user-supplied header value plus recipients, before every backend (tested; no query/HTML surfaces).
- TLS verification — rustls, `Tls::Required`, no insecure options; SMTP credentials only sent post-STARTTLS.
- Secret leakage — redacting Debug on both credential structs; no `smtp_password`/`api_key` in any log or error path; `ApiResponse.body` is provider text only.
- Address parsing — lettre `Mailbox` parse errors surface as `EmailError`, no panics on malformed input.
- Feature-flag hygiene — API selected without the `api` feature errors at send time with `--features api` guidance (`:1133-1150`); custom-backend registry set-once with a warning on re-register.
- Raw-SQL rule — not applicable (no DB access); `grep sqlx::query` across `src/` and `tests/`: zero hits; imports are facade-only.
- Cargo dependency direction — `auth` feature depends on umbral-auth behind a feature, documented as the intended acyclic direction (auth does not depend on email).
- Panics — no `unwrap`/`expect` reachable from a send path except `deliver_smtp`'s `expect` on an invariant the selection code guarantees (`BackendKind::Smtp` ⇒ host set).

## Known issues already tracked elsewhere (not counted in the totals)

- `review_1 broken-features BROKEN-6` — `send()` panicked on the console backend with settings uninitialized — `[fixed/verified held]` (uses `get_opt`; `tests/prod_safety.rs` pins the refusal).
- `gaps2 #81` (archived) — email CRLF guard + console fail-closed in prod + SMTP timeout — `[fixed/verified held]` (all three verified in code).
- `audit_2 plugin-realtime-comms #6` — `EmailConfig`/`ApiRequest` `Debug` masks credentials — `[fixed/verified held]` (manual impls at `lib.rs:496-511`, `954-964`).
- `features #91` — pluggable email sender (`EmailBackend` trait + `EmailPlugin::with_backend`) — `[fixed/verified held]` (trait, registry, spy test all present).
- `gaps5 #53` (tf#266) — email has no durable retry queue — `[open]` (module docs say so; no queue in code).

## Out-of-scope notes

- `crates/umbral-core/src/app.rs:417` — the programmatic `.settings(Settings)` path vs ambient `settings::get_opt()` is the framework-wide seam behind EMAIL-4; the same re-read pattern appears at `plugins/umbral-analytics/src/lib.rs:487,516` and `plugins/umbral-livereload/src/lib.rs:174-178`. Routed to the core reviewer / synthesis (one facade helper would fix the class).
- The correct instruction already exists at `documentation/docs/v0.0.1/plugins/email.mdx:70` (console opt-in requires `Dev`/`Test`); no doc change needed — the code's error text is the side to fix (EMAIL-1).
