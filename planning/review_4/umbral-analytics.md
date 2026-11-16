# review_4 · umbral-analytics

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 810 LOC (`plugins/umbral-analytics`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 2 low · 2 info — 5 new, 2 known |

## Summary

- Fire-and-forget product analytics to PostHog: ambient `capture`/`identify` free functions, an opt-in `capture_requests()` middleware that fires `$pageview` per request, a default-on path scrubber, operator-supplied exclusion prefixes, and a shared timed HTTP client with a bounded send fan-out. 810 LOC including tests.
- The infrastructure half is well built: shared `OnceLock` reqwest client (mirroring umbral-oauth exactly), a `SEND_SLOTS` semaphore capping concurrent sends at 64 with drop-not-queue behavior (audit_2 #5's fan-out half, verified in code + a test), clean no-op without a key, never blocks a response.
- The privacy half is where the work is: the `scrub_path` heuristic misses common secret shapes — sub-24-char tokens (nanoid-21, 16-hex ids) and percent-encoded emails ship raw to PostHog while the doc claims the scrubber "makes even a captured path safe to ship" (ANALYTICS-1, fix first).
- The crate also ships no default exclusions, unlike its sibling `umbral-logs` — `/health`, favicon and dev endpoints become pageview rows once `capture_requests()` is on (ANALYTICS-2).
- Two smaller items: key/host resolution re-reads `Settings::from_env()` so programmatic settings are invisible (ANALYTICS-3, same class as EMAIL-4), and two Cargo dependencies are unused (ANALYTICS-4).

## Findings

### ANALYTICS-1 · MEDIUM · privacy/defense-in-depth · self-verified
**`scrub_path`'s heuristics miss common secret shapes — short tokens (< 24 chars) and percent-encoded emails pass through — so the doc's claim that the scrubber "makes even a captured path safe to ship" is false for routine cases.**
- **Where**: `plugins/umbral-analytics/src/lib.rs:161-163` (the claim), `:260-294` (heuristics), `:363-368` (applied to every auto pageview)
- **Evidence**:
  ```rust
  // Excluding a whole route family is the operator's call; the scrubber
  // (gaps4 #22) is the universal default that makes even a captured path
  // safe to ship.
  ```
  ```rust
  // Email — anything with an `@` and a dot after it.
  if seg.contains('@') {
      return ":email";
  }
  ```
  ```rust
  if seg.len() >= 24
      && seg
          .bytes()
          .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'='))
      && seg.bytes().any(|b| b.is_ascii_digit())
      && seg.bytes().any(|b| b.is_ascii_alphabetic())
  {
      return ":token";
  }
  ```
- **Impact**: With `.capture_requests()` on, a GET of `/reset/V1StGXR8_Z5jdHi6B-myT` (a nanoid-21 reset token, one of the most common token shapes) is not an email, not a UUID, not all digits, and 21 < 24 — so `path` and `$current_url` ship **raw** to PostHog. Same for `/invite/a3f9c2d84b7e1f60` (16-hex invite/session id, len 16) and for `/users/ada%40x.com` (the `%` defeats both the `@` check and the token charset). Only ≥ 24-char mixed-alnum tokens, UUIDs, unencoded emails and pure integers are replaced. This is exactly the class `planning/review_3/codex/16` was about; its recommendation 1 is shipped but with these holes. Static trace; the classification is pure, so runtime behavior is fully determined by the quoted conditions. (Scope note: `capture()`/`identify()` pass caller-supplied properties untouched by design — the scrubber's guarantee is about auto-pageview paths only.)
- **Fix**: Percent-decode the segment before classifying (or match `%40`); widen the token heuristic so common shapes are covered (e.g. mixed alnum with any digit and length ≥ 16, hex ≥ 12) while keeping slug-like strings (single class, no digits) untouched; and reword the comment to describe a best-effort heuristic instead of promising safety. A test with a nanoid-21 and a 16-hex segment alongside the existing unit test would pin it.

### ANALYTICS-2 · LOW · drift
**No default exclusion prefixes — unlike `umbral-logs`, the plugin ships zero defaults, so health checks, favicon requests and dev endpoints all become `$pageview` rows once capture is enabled.**
- **Where**: `plugins/umbral-analytics/src/lib.rs:530-538` (`exclude_prefixes: Vec::new()`), `:460-468` (builder); contrast `plugins/umbral-logs/src/lib.rs:103`
- **Evidence**:
  ```rust
  impl Default for AnalyticsPlugin {
      fn default() -> Self {
          Self {
              api_key: None,
              host: DEFAULT_POSTHOG_HOST.to_string(),
              auto_capture_requests: false,
              exclude_prefixes: Vec::new(),
          }
      }
  }
  ```
  ```rust
  const DEFAULT_EXCLUDE_PREFIXES: &[&str] = &["/static", "/admin/static", "/health", "/favicon.ico"];
  ```
- **Impact**: Scoped to apps that opt into `.capture_requests()` (default off), but for them every uptime probe and favicon hit ships a path to a third-party host and pollutes the pageview numbers. `review_3/codex 16`'s recommendation 3 ("default exclusions for auth, admin, media, API, and webhook paths unless opted in") remains unshipped while recommendation 1 (scrub) shipped — the mismatch with logs' shipped defaults is the drift.
- **Fix**: Ship a small default list matching logs where the semantics transfer (`/health`, `/favicon.ico`, `/__umbral/`, static mounts), with `.exclude_path_prefix` staying additive and each default documented; leave the opinionated ones (auth/admin paths) to the operator.

### ANALYTICS-3 · LOW · drift/ergonomics
**Key/host resolution re-reads `Settings::from_env()` and `on_ready` ignores its `ctx`, so programmatically-supplied settings silently produce a lifetime no-op.**
- **Where**: `plugins/umbral-analytics/src/lib.rs:487`, `:516`, `:546`
- **Evidence**:
  ```rust
  // umbral.toml extra key last.
  if let Ok(settings) = umbral::Settings::from_env() {
      if let Some(v) = settings.extra.get("posthog_api_key") {
  ```
- **Impact**: Trace (static): an app inserts `extra["posthog_api_key"]` in code and passes `App::builder().settings(s)` → `resolve_api_key` consults env and `from_env()` only, sees nothing → boot logs the "no key" warning and installs no ambient client → every `capture()`/`identify()` is a silent no-op for the process lifetime. Same class as EMAIL-4 (`plugins/umbral-email/src/lib.rs:562-567`); `on_ready` already receives the settings through `ctx` and reads `_ctx` only for the warning.
- **Fix**: Prefer ambient `settings::get_opt()` (falling back to `from_env`, logging the error) in both resolvers, or read `ctx.settings` in `on_ready`; keep the documented builder > env > settings precedence.

### ANALYTICS-4 · INFO · hygiene
**`serde` and `async-trait` are unused dependencies.**
- **Where**: `plugins/umbral-analytics/Cargo.toml:14`, `:17`
- **Observation**: `grep` over `src/` and `tests/` finds zero uses of either (`serde_json` alone is used, `use serde_json::{Value, json}` at `src/lib.rs:55`). Leftovers from an earlier shape; drop them (or the `derive` feature on serde) to keep the dependency set honest.

### ANALYTICS-5 · INFO · observation
**Every auto pageview is unattributed and unfiltered.**
- **Where**: `plugins/umbral-analytics/src/lib.rs:375` (hardcoded `"anonymous"`), doc note at `:342`, middleware at `:347`
- **Observation**: All auto-captured events share the distinct_id `"anonymous"`, non-GET requests (form POSTs) also fire `$pageview`, and there is no bot/asset filtering. The identity seam is owned by auth/sessions (see `features #3` follow-up); consent hooks and destination pluggability are already tracked at `gaps5 #69`, so this is context, not a new ask.

## Strengths

- **Payload correctness is pinned against PostHog's documented shape** (`build_payload`, `:197-205`) with tests covering the JSON body, and the no-key path is a tested clean no-op.
- **Shared timed HTTP client done exactly like the sibling**: `OnceLock<reqwest::Client>` with `timeout(10s)`/`connect_timeout(5s)` (`:108-120`), `O(1)` clone per send — the pattern EMAIL-2 should adopt.
- **Bounded outbound fan-out**: `SEND_SLOTS` semaphore (64) acquired *before* spawn, drop-on-full with a debug log, permit moved into the task (`:211-254`) — audit_2 #5's amplification half is genuinely fixed, with a test (`outbound_sends_are_concurrency_bounded`).
- **Nothing blocks a response by construction**: middleware spawns, free functions spawn, errors are warn/debug-logged and never propagated — and the crate documents this contract precisely (`analytics.mdx:10`).
- **The scrubber itself is structurally sound** where it applies: `split('/')` preserves slash shape (leading/trailing/internal), the function is pure and unit-tested, and scrub is default-on for auto pageviews rather than opt-in (gaps4 #22 shipped).
- **Exclusion mechanism is minimal and explicit**: `should_capture_path` is exposed for testing, `exclude_path_prefix` is repeatable, and the doc comment tells operators exactly which routes to exclude.
- **Feature-free dependency set** (only the unconditional reqwest rustls client, justified in Cargo.toml comments); facade-only imports; zero raw-SQL.

## Clean areas checked

- Key secrecy — the API key travels only in the JSON body; warn/debug logs emit `url`/`status`/`error` only, never the payload or key.
- Unbounded concurrency — bounded (see Strengths); no lock held across an await; no queue growth (drop instead).
- Response-path safety — every public entry point spawns; `capture`/`identify` return before any network I/O; no `unwrap` on a request path (`http_client`'s `expect` fires only if the TLS backend fails to build, matching oauth's established pattern).
- Method handling — `status` is captured after the inner handler (documented + implemented at `:354-355`).
- Test quality — 810 LOC total with 161 test LOC: payload shape, no-op-without-client, client sharing, concurrency bound, scrub behavior, exclusion predicate; the live PostHog round-trip is correctly `#[ignore]`.
- Raw SQL / ORM rule — not applicable (no DB); `grep sqlx::query`: zero hits.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps5 #69` (tf#282) — analytics is PostHog-only and best-effort (pluggable destinations, consent hooks, retry/outbox) — `[open]`.
- `audit_2 plugin-observability #5` — unbatched per-event sends / bounded fan-out — `[open]`; status verified: the fan-out half is fixed (SEND_SLOTS = 64, drop-on-full, tested at this commit), batching (`/batch/`) is still absent.
- `audit_2 plugin-observability #4` — full request paths shipped to PostHog with no exclusion list — `[fixed/verified held]` (exclusion mechanism + default scrub shipped; the effectiveness gaps are ANALYTICS-1 / ANALYTICS-2, filed numbered above).
- `review_3/codex` 16 — pageview path privacy — `[partially held — see ANALYTICS-1, ANALYTICS-2]` (recommendation 1 shipped as `scrub_path`; recommendation 3, default exclusions, did not; recommendation 2, route-template reporting, is a larger product decision not re-filed here).

## Out-of-scope notes

- The `"anonymous"` distinct_id has no server-side identity seam yet — resolving it belongs to auth/sessions (extension-producing middleware), tracked as the `features #3` follow-up; the analytics side cannot fix it alone (ANALYTICS-5).
- The `Settings::from_env()` re-read pattern (ANALYTICS-3) also appears at `plugins/umbral-email/src/lib.rs:562-567` and `plugins/umbral-livereload/src/lib.rs:174-178`; one facade helper would fix the class — routed to the core reviewer / synthesis.
- `planning/review_3/codex/16` recommendation 2 (report route names/templates instead of raw paths) would need router-side pattern knowledge that plugins don't have today; if wanted, it belongs in a tracker entry, not this crate's fix list.
