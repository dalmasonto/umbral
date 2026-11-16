# review_4 · umbral-logs

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 1,205 LOC (`plugins/umbral-logs`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | healthy |
| **Findings** | 0 critical · 0 high · 0 medium · 2 low · 0 info — 2 new, 2 known |

## Summary

- Records every HTTP request to a `RequestLog` row via a fire-and-forget middleware layer, with path-prefix exclusions, a deterministic sampler, a `min_status` floor, a read-only admin model, and an optional structured-logging/OTLP init helper (`observability.rs`). 1,205 LOC including tests.
- This is the healthiest crate reviewed in this slice: both findings are LOW (an inherited docs off-by-one and a comment/duplication drift), and every security-relevant behavior checked out — the previous audit fixes (trusted `LoggedUserId` extension, handle reaping, `min_status` caveat) are all verifiably present.
- The trust boundaries are drawn correctly: IP resolution delegates to core's `client_ip` (never re-parses XFF locally), `user_id` comes only from a trusted request extension (never a header), and only `req.uri().path()` is captured — query strings (password-reset tokens, signed URLs) never enter the table.
- The remaining tracked gaps are operational (retention/drains, `gaps5 #68`) and a convenience seam (auto-resolving `user_id` from the identity extension, `features #3` follow-up) — neither is a defect in this code.
- Nothing here blocks a request path; the fire-and-forget insert is cancellation-safe and its failure is warn-logged with the error attached.

## Findings

### LOGS-1 · LOW · docs
**The `RequestLog.ip` doc repeats core's off-by-one description of the trusted-proxy-hops rule; code and tests resolve the hops-th entry from the right, not the `(hops+1)`-th.**
- **Where**: `plugins/umbral-logs/src/lib.rs:88-89`
- **Evidence**:
  ```rust
  /// Client IP, resolved from `X-Forwarded-For` under the trusted-proxy-hops
  /// policy (the `(hops+1)`-th entry from the right; `X-Real-IP` is not used).
  ```
- **Impact**: An operator writing a query or a dashboard that mirrors the doc computes the wrong element for every hops value. Concretely: with `trusted_proxy_hops = 2` and `X-Forwarded-For: 9.9.9.9, real, proxy1`, the code resolves `"real"` (index `len - hops` = 1; core `client_ip_with_hops`, `crates/umbral-core/src/settings.rs:163-186`, pinned by the test at `:805-841`), while the doc says "3rd from the right" → `"9.9.9.9"`. The review_3 correction (purging the old "first hop" wording and the `X-Real-IP` claim) is present and held; only the count phrasing is still wrong.
- **Fix**: Reword to "the `hops`-th entry from the right". The same off-by-one wording exists upstream at `crates/umbral-core/src/settings.rs:152` and `:437` (routed below) — fixing the plugin copy is correct here because this is the doc a logs consumer reads.

### LOGS-2 · LOW · drift/perf
**`capture_layer`'s comment says excluded paths "skip the handler-side bookkeeping entirely", but four header reads/allocations run before the exclusion return; the exclusion predicate is also written twice with only a "kept consistent here" comment.**
- **Where**: `plugins/umbral-logs/src/lib.rs:387-401`, `:410-419`; predicate twin at `:280-292`
- **Evidence**:
  ```rust
  // Exclusion check up front: skip the handler-side bookkeeping entirely
  // for excluded paths.
  let excluded = cfg
      .exclude_prefixes
      .iter()
      .any(|p| path.starts_with(p.as_str()));

  let method = req.method().to_string();
  let ip = resolve_ip(req.headers());
  ```
  ```rust
  let seq = SAMPLE_COUNTER.fetch_add(1, Ordering::Relaxed);
  if !cfg.should_capture(&path, status, seq) {
  ```
  and at `:415`: `// The full predicate is `LogsConfig::should_capture` — kept consistent here.`
- **Impact**: No correctness bug today (traced: the inline check and `should_capture` agree; `seq` advances only past both), but two costs: (a) every excluded request — i.e. every `/static` asset hit and every `/health` probe — still allocates `method`/`user_agent`/`ip` strings and reads extensions before returning, which is exactly what the comment promises does not happen; (b) the exclusion rule now exists in two places, so a future exclusion dimension (a method filter, an opt-out header) added to `should_capture` must be duplicated inline or the "cheap pre-check" silently stops being equivalent.
- **Fix**: Move the `excluded` check above the string reads (the comment's stated intent) and let `should_capture` own the predicate as its single definition, or split it into `is_excluded` + `sampled` and call each once.

## Strengths

- **Trust boundaries are enforced, not documented.** `resolve_ip` delegates to `umbral::settings::client_ip` (single policy with core; never a local XFF parse, never `X-Real-IP`), and `user_id` resolves only from the trusted `LoggedUserId` request extension (`:351-370`) — the audit_2 #7 fix is real in code, and the user doc (`logs.mdx`) describes exactly this.
- **Minimal data capture.** Only `req.uri().path()` is stored (`:385`) — no query string, no body, no headers except User-Agent — so reset tokens and signed URLs cannot land in the table.
- **Fire-and-forget done safely.** The row is moved into the spawn, the error is warn-logged with the error value ("logs: failed to record request (swallowed)" — with the reason, not silently), and `track_handle` reaping bounds the `PENDING` list (audit_2 #2 verified).
- **Sampler and floor are deliberate.** Deterministic `SAMPLE_COUNTER`-based sampling (reproducible in tests; documented in `logs.mdx`), with excluded/below-floor requests excluded from the cadence, and the `min_status` security-visibility caveat documented on the builder itself (audit_2 #11 verified).
- **Defaults reduce their own noise** — `DEFAULT_EXCLUDE_PREFIXES` covers `/static`, `/admin/static`, `/health`, `/favicon.ico` (`:103`) — and `LogsPlugin` extends, never replaces, them.
- **Good test shape.** `tests/capture.rs` boots a real app over a tempfile sqlite DB and round-trips records/excludes through the public layer, plus unit coverage of `should_capture` with explicit configs.
- **Observability helper is conservative**: pinned mutually-compatible OTel versions with documented rationale, OTLP failure degrades to logs-only with an eprintln, and the whole stack is feature-gated (`otel`) so a base build carries none of it.
- **Admin integration is honest**: `admin_model()` is read-only with every column marked readonly, and the `admin` dependency is feature-gated (on by default, off for slim builds).

## Clean areas checked

- Raw SQL — `grep sqlx::query` across `src/` and `tests/`: zero hits; all row I/O via the ORM (`RequestLog::objects().create(row)`).
- Secret/PII capture — query strings and bodies never read; only path/method/status/duration/UA/extension-sourced user id.
- Log-forgery — no client-controlled identity input anywhere; the header-trust finding (audit_2 #7) is closed and the fix verified.
- Spawn/lock safety — no lock held across an await; the detached insert cannot break the response path; `flush()` in tests awaits tracked handles only.
- Stored-XSS (rendering side) — the admin engine sets `minijinja::AutoEscape::Html` globally (`plugins/umbral-admin/src/engine.rs:20`), so the audit_2 #8 feed is closed on the render side; row text in the audit file is historical (see Known).
- Panics — no `unwrap`/`expect` on the request path; `status.as_u16() as i32` cannot fail; `SAMPLE_COUNTER` overflow is a `u64` fetch_add (practically unreachable).
- Feature hygiene — a `default-features = false` logs-only build compiles without umbral-admin; `otel` is fully optional with documented version pinning.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps5 #68` (tf#281) — logs stored in the app DB can become a load/retention risk (drains, retention jobs) — `[open]`.
- `features #3` (follow-up) — auto-resolve `user_id` from the request `Identity` extension when auth is mounted — `[open]`; note the tracker text is stale (it still says "user_id is header-based (`X-Umbral-User-Id`)", which the audit_2 #7 fix replaced with the `LoggedUserId` extension — verified in code). What remains open is the auto-resolution seam itself; nothing in the tree inserts the extension yet (`grep LoggedUserId` finds only this crate).
- `audit_2 plugin-observability #8` — stored-XSS feed via rendered `user_agent`/`path` — `[fixed/verified held]` (admin escapes via `AutoEscape::Html`, `engine.rs:20`).
- `audit_2 plugin-observability #2` — unbounded `PENDING` JoinHandle list — `[fixed/verified held]` (reaping loop verified).
- `audit_2 plugin-observability #7` — client-supplied `X-Umbral-User-Id` header trusted — `[fixed/verified held]` (extension-based resolution verified).
- `audit_2 plugin-observability #11` — `min_status` security-visibility caveat — `[fixed/verified held]` (documented on the builder).
- `gaps3 #59` — `user_id` stored as TEXT so String/Uuid PKs are attributable — `[fixed/verified held]` (model field is `Option<String>` with the rationale in rustdoc).
- `review_3` client_ip doc corrections — `[partially held — see LOGS-1]` (the wrong claims review_3 purged are gone; the residual `(hops+1)`-th phrasing is this sweep's LOGS-1, with the source upstream in core).

## Out-of-scope notes

- `crates/umbral-core/src/settings.rs:152` and `:437` — both describe the trusted-proxy-hops entry as "(n+1)-th"/"(hops+1)-th from the right while `client_ip_with_hops` (`:163-186`) and its test (`:805-841`) implement hops-th; the source of LOGS-1's wording — routed to the core reviewer.
- `crates/umbral-core/src/settings.rs:419-425` — `Environment` defaults to `Dev`, which is what makes this plugin's permissive posture fail-open in un-configured deployments (audit_2 #6, still open at core level); routed for context.
- `plugins/umbral-admin` list view for `RequestLog` — escaping is globally `AutoEscape::Html`; the admin reviewer may want to confirm the list/table code path explicitly (audit_2 #8's render-side check).
