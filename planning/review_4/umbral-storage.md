# review_4 · umbral-storage

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 9,096 LOC (`plugins/umbral-storage`) |
| **Reviewer** | claude (review_4 wave agent) |
| **Verdict** | needs attention |
| **Findings** | 1 critical · 2 high · 1 medium · 2 low · 2 info — 8 new, 2 known |

## Summary

- `umbral-storage` is the merged static+media plugin: a `Storage` backend surface (FS, optional S3, custom), the upload pipeline (type policy, size cap, sniffing, deferred/background saves), media serving (ServeDir for FS, a gated proxy for non-FS), a signed-URL gate, and the cache-first `media_access_cached` authorization.
- The design instincts are right — one policy point via decorators so every upload path inherits it, mid-stream caps, neutralised filenames on every backend, a length-prefixed cache key, a guarded store for the media-access race — and the code is unusually well documented. Two of the findings are gaps in that decorator work.
- Biggest risk: **STORAGE-1** — the gated non-FS proxy decodes `%2F` in a URL and hands the resulting key to the backend unvalidated; with a path-join backend (`FsStorage` via `media_with_storage`, or a user `Storage`) any caller the gate admits reads arbitrary files (and the same unvalidated key feeds delete-cleanup). The core doc-comment claiming "the storage layer already blocks path traversal" is false.
- The `.accept(...)` decorator is broken in two ways: **STORAGE-2** (it buffers the whole body before any check, so `max_size` no longer bounds memory and two comments claim the opposite) and **STORAGE-3** (`save_deferred` + `accept` always fails because `TypeLimitedStorage` doesn't implement `put`).
- Fix order: STORAGE-1 → STORAGE-3 (dead feature, tiny fix) → STORAGE-2 → STORAGE-4.

## Findings

### STORAGE-1 · CRITICAL · security · self-verified

**The gated non-FS media proxy percent-decodes the request path and passes the key to the backend unvalidated; with any path-join backend (`FsStorage` via `media_with_storage`, or a user-written `Storage`) a caller the gate admits can read arbitrary files, and the same unvalidated key reaches delete-cleanup.**

- **Where**: `plugins/umbral-storage/src/lib.rs:1137-1149` (proxy), `plugins/umbral-storage/src/media_gate.rs:26-35` (`key_for_path`), `plugins/umbral-storage/src/media.rs:233-235` and `:544-556` (`FsStorage`), `crates/umbral-core/src/orm/file_field.rs:48-61` (the false claim + the unused validator)
- **Evidence**:
  ```rust
  // lib.rs:1137-1149 — the `None if gated` proxy arm
  let key = media_gate::key_for_path(req.uri().path(), &mount_prefix);
  let ok = media_gate::allows(signed, &access, req.uri().query(), req.headers(), &key).await;
  if !ok { return forbidden_media(); }
  match storage.retrieve_stream(&key).await {
  ```
  ```rust
  // media_gate.rs:26-35 — no structural validation; only invalid UTF-8 keeps the raw string
  pub(crate) fn key_for_path(path: &str, mount_prefix: &str) -> String {
      let raw_key = path.strip_prefix(mount_prefix).unwrap_or(path).trim_start_matches('/');
      percent_encoding::percent_decode_str(raw_key)
          .decode_utf8()
          .map(|s| s.into_owned())
          .unwrap_or_else(|_| raw_key.to_string())
  }
  ```
  ```rust
  // media.rs:233-235 — the join every FsStorage method funnels through
  fn path_for(&self, key: &str) -> PathBuf {
      self.dir.join(key)
  }
  ```
  ```rust
  // crates/umbral-core/src/orm/file_field.rs:48-61 — the claimed backstop
  /// Defense-in-depth (gaps / IDOR hardening): the storage layer
  /// already blocks path traversal at the filesystem boundary, but ...
  pub(crate) fn validate_storage_key(key: &str) -> Result<(), &'static str> {
  ```
  `validate_storage_key` is `pub(crate)` and its only call site is inside serde `Deserialize` (`file_field.rs:227`) — no serve/serve-proxy/delete path calls it.
- **Impact**: Concrete scenario. An app wires the two documented shapes `media_with_storage("/files", Arc::new(FsStorage::new("", "./media")))` and `media_access_identity(|caller, _key| async move { caller.is_some() })`. A logged-in user requests `GET /files/%2Fetc%2Fpasswd`. `key_for_path` decodes `%2F` to `/`, giving key `/etc/passwd`; the gate admits the caller (it authorizes *who*, never *which key*); `FsStorage::retrieve_stream` opens `dir.join("/etc/passwd")`, and `PathBuf::join` with an absolute path **replaces** the base — so `/etc/passwd` is streamed back with a 200. The relative form `/files/..%2F..%2Fetc%2Fpasswd` escapes the same way. With `media_access_staff`/`media_access_roles` any user meeting the role gets the same read. The default `media()` FS arm is *not* affected (ServeDir + `SymlinkGuardService`, `lib.rs:1078-1079`); the hole is the `None if gated` proxy arm (`lib.rs:1127`) and any backend that resolves keys as paths. Second arm: a key stored in the DB by a writer that bypasses `Deserialize` (see Out-of-scope MD-2) reaching `delete_blob_best_effort` (`media.rs:324-326`) deletes arbitrary paths through the same join. Provenance: the decode in `key_for_path` is the fix for review_3's candidate "the media_access callback receives the raw percent-ENCODED path but ServeDir decodes before serving" (`planning/review_3/claude/_raw_candidates.json:320`) — before that fix, `%2F` stayed encoded and `dir.join` saw a harmless literal filename, so this traversal is a consequence of the (correct) decode fix, not a re-report of it; no tracker entry covers validating the decoded key.
- **Fix**: validate at the key boundary. Make `key_for_path` return `Result` and reject a leading `/`, any `..` segment, backslashes and NUL (it already commits to "fails CLOSED" for bad UTF-8 — extend that posture from encoding to structure), and/or make `FsStorage::path_for` containment-check (reject absolute keys and `..`; canonicalize-prefix in debug). Promoting core's `validate_storage_key` to a shared storage-boundary validator would make the core doc-comment true. The signed-only configuration is not exploitable (signatures can't be minted for traversal keys), so gate the wire-up fix against the identity/roles/owner presets.

### STORAGE-2 · HIGH · perf/alloc · self-verified

**With `.accept(...)` configured, `TypeLimitedStorage::store_stream` buffers the entire request body before any size check — `max_size` bounds what is written, not what is buffered — and both the code comment and the user docs claim the opposite.**

- **Where**: `plugins/umbral-storage/src/media.rs:1251-1268` (`store_stream`), `plugins/umbral-storage/src/lib.rs:1253-1265` (decorator order + comment), `plugins/umbral-storage/src/media.rs:653-671` (`SizeLimitedStorage::store_stream`, the mid-stream cap that no longer runs), `documentation/docs/v0.0.1/orm/file-image-fields.mdx` ("Streaming large files")
- **Evidence**:
  ```rust
  // media.rs:1251-1268
  async fn store_stream(&self, filename: &str, content_type: &str, body: umbral::storage::ByteStream) -> ... {
      // Sniffing needs the leading bytes, so a policed stream is buffered.
      // Correctness beats streaming here: you cannot decide whether bytes are
      // acceptable without looking at them, and the size cap (applied by the
      // decorator underneath) still bounds how much gets buffered.
      let mut buf: Vec<u8> = Vec::new();
      while let Some(chunk) = body.next().await {
          buf.extend_from_slice(&chunk.map_err(StorageError::Io)?);
      }
      self.check(content_type, &buf)?;
      self.inner.store(filename, content_type, &buf).await
  }
  ```
  ```rust
  // lib.rs:1253-1265 — SizeLimited is INNER; TypeLimited wraps it
  Some(max_size) => { Arc::new(SizeLimitedStorage::new(media.storage.clone(), max_size)) }
  ...
  // gaps3 #51: the type policy wraps the size cap, so an oversized
  // upload is refused on size before anything is buffered to sniff it.
  if let Some(accept) = &media.accept {
      storage = Arc::new(media::TypeLimitedStorage::new(storage, accept.clone()));
  }
  ```
  The chain is `MediaTracking(TypeLimited(SizeLimited(raw)))`, and `TypeLimited::store_stream` never hands a stream to `SizeLimited::store_stream` (where `cap_stream` enforces the cap mid-stream) — it buffers first, then calls the buffered `store`, which rejects only after the fact.
- **Impact**: An app with `.media(...).max_size(25MiB).accept_images()` and any hand-written upload route calling the ambient `store_stream` (the exact pattern the `lib.rs` comment says "inherit[s] the policy") is an unauthenticated memory-exhaustion target: `check()` runs only after the loop, so *any* declared type — even a disallowed one — causes the full body to be held in RAM; 5 GiB in one request, or 20 concurrent 500 MiB ones, exhausts the process before `SizeLimited` returns `TooLarge`. The plugin's own `save_stream` and the multipart/admin paths pre-cap the stream (`media.rs:1109-1113`; `parse_multipart_capped`) and stay bounded — the gap is the ambient streaming API the decorators advertise. The docs assert the opposite invariant: "when a `max_size` cap is configured it is enforced **mid-stream**, rejecting an oversize body the instant its real bytes cross the cap".
- **Fix**: swap the decorator order — `SizeLimited` outermost — so `cap_stream` bounds what `TypeLimited` can buffer, or teach `TypeLimited::store_stream` to buffer a bounded sniff window and then pass a still-capped stream through (`size_hint`-aware). Fix the two comments in the same change.

### STORAGE-3 · HIGH · correctness · self-verified

**`.accept(...)` + `save_deferred()` makes every deferred upload end `status="failed"`: `TypeLimitedStorage` does not implement `put`, and `save_deferred` calls `put` on the type-limited decorator — whose `put` is the trait's erroring default.**

- **Where**: `plugins/umbral-storage/src/lib.rs:220-228` (`media_policy_storage`), `:950-966` (`save_deferred`), `plugins/umbral-storage/src/media.rs:1085-1090` (the spawned prelude), `:1226-1278` (`impl Storage for TypeLimitedStorage`), `crates/umbral-core/src/storage.rs:230-240` (the default)
- **Evidence**:
  ```rust
  // lib.rs:220-228 — with `.accept(...)`, the policy wrapper replaces the backend
  fn media_policy_storage(media: &MediaSide) -> Arc<dyn Storage> {
      match &media.accept {
          Some(accept) => Arc::new(media::TypeLimitedStorage::new(media.storage.clone(), accept.clone())),
          None => media.storage.clone(),
      }
  }
  ```
  ```rust
  // media.rs:1085-1090 — the deferred write is the prelude
  run_processing(spawn_row, procs, move || async move {
      storage
          .put(&key, &put_content_type, &bytes)
          .await
          .map(|_| ())
          .map_err(|e| Box::new(e) as BoxError)
  })
  ```
  ```rust
  // crates/umbral-core/src/storage.rs:236-239 — what that resolves to
  Err(StorageError::Unsupported(
      "this Storage backend does not implement put(); override it to write at an exact key"
          .to_string(),
  ))
  ```
  `impl Storage for TypeLimitedStorage` (`media.rs:1226-1276`) implements `store`, `store_at`, `retrieve`, `store_stream`, `delete`, `url` — no `put` (nor `put_stream`), so both fall to the default.
- **Impact**: `StoragePlugin::new().media("/media", "./media").accept_images()` then `plugin.save_deferred("a.png", "image/png", bytes).await?` returns `Ok` with a final-looking URL; the detached task's prelude hits the `Unsupported` default, `run_processing` logs "background processing failed" and persists `status="failed"` (`media.rs:107-133`); the URL 404s forever and the bytes are never written. Every deferred save in an accept-configured app is broken, and the caller's `Ok` hides it. The combination is untested: `media_background_processing.rs::save_deferred_writes_in_background` builds its plugin via `media_with_storage` with no `.accept(...)`.
- **Fix**: implement `put` (and `put_stream`) on `TypeLimitedStorage`: run the same `check` (declared type + sniffed leading bytes) then forward to `inner.put` / `inner.put_stream`. Add an `accept + save_deferred` test so the combination can't rot again.

### STORAGE-4 · MEDIUM · correctness/observability · self-verified

**`media_invalidate_on` maps every deserialization failure (and a missing `instance` payload) to zero tags with no log — one schema drift silently disables media-access invalidation for that model.**

- **Where**: `plugins/umbral-storage/src/lib.rs:540-547`
- **Evidence**:
  ```rust
  let erased = Arc::new(move |payload: &serde_json::Value| -> Vec<String> {
      let Some(instance) = payload.get("instance") else {
          return Vec::new();
      };
      match serde_json::from_value::<M>(instance.clone()) {
          Ok(row) => map_fn(&row),
          Err(_) => Vec::new(),
      }
  });
  ```
- **Impact**: The media-access cache is invalidated only through these maps (gaps6 #9). If model `M` gains an enum variant / field shape the row no longer deserializes as (or a writer emits a payload without `instance`), every `post_save`/`post_delete` for that table maps to no tags, `bust_tag` never fires, and cached allow/deny decisions survive until their TTL (default 60s) — with zero signal in logs or metrics. The documented promise is the opposite: the wiring "keeps `media_access_cached` decisions fresh below their TTL".
- **Fix**: `tracing::warn!` with the table name and the error in the `Err` arm (and once for the missing-`instance` shape); decide consciously whether a decode failure should bust a coarser fallback tag rather than nothing.

### STORAGE-5 · LOW · correctness

**S3 `exists()` maps every 4xx — including 401/403 — to `Ok(false)`, turning an auth/misconfiguration error into "object missing".**

- **Where**: `plugins/umbral-storage/src/s3.rs:451-459`
- **Evidence**:
  ```rust
  Ok((_head, 200)) => Ok(true),
  Ok((_head, 404)) => Ok(false),
  Ok((_head, status)) if (200..400).contains(&status) => Ok(true),
  Ok((_head, status)) if (400..500).contains(&status) => Ok(false),
  ```
- **Impact**: A bucket policy that lost `s3:HeadObject` (but kept GET), expired credentials, or a wrong endpoint makes every `exists` a false negative; callers branch on that (skip-if-present logic treats present objects as absent) and an operator sees no error while the misconfig persists. No security impact — a later fetch still fails loudly.
- **Fix**: keep 404 → `Ok(false)`; map 401/403 (and 400s whose body carries a non-`NotFound` AWS code) to `StorageError::Backend`.

### STORAGE-6 · LOW · docs

**`media_with_storage`'s rustdoc still describes the pre-`gaps4 #18` behaviour: it says the GET-serving `ServeDir` reads `mount` as a dir, but the builder sets `dir: None` and mounts no ServeDir for a custom backend.**

- **Where**: `plugins/umbral-storage/src/lib.rs:668-687` (doc vs. body)
- **Evidence**:
  ```rust
  /// Serve uploads backed by a custom [`Storage`] mounted at `mount` (the
  /// old `MediaPlugin::with_storage`). The GET-serving `ServeDir` still
  /// reads `mount` as a dir; a non-filesystem backend serves its own
  /// URLs, so the route is a no-op for keys that never hit local disk.
  ...
          self.media = Some(MediaSide {
              // gaps4 #18: custom/S3 backend serves its own URLs — no local ServeDir.
              dir: None,
  ```
- **Impact**: A reader wiring `FsStorage` through `media_with_storage` believes local disk still serves `/mount/<key>`; what actually happens is the gated proxy arm (or nothing when ungated). The stale sentence hides the routing that STORAGE-1 exploits. (This is a new instance of the `gaps6 #21` stale-doc batch — not in its list.)
- **Fix**: state that GETs are served only by the gated proxy route (`lib.rs:1127-1174`) and that an ungated custom backend mounts nothing.

### STORAGE-7 · INFO · defense-in-depth

**`SizeLimitedStorage::put` / `put_stream` bypass the size cap entirely** (`media.rs:697-704` delegates straight to `inner.put`, unlike `store`/`store_at`/`store_stream`). Today's only caller pre-checks (`save_deferred_through`, `media.rs:1045-1052`), so nothing is oversized in practice — but the decorator named "SizeLimited" silently loses its contract for the next direct caller.

### STORAGE-8 · INFO · docs

**Stale version in the S3 module doc**: `s3.rs:6-8` says "the same crate + version, `s3` 0.35" while `Cargo.toml:32` pins `rust-s3 = "0.37"`. Another `gaps6 #21`-batch instance.

## Strengths

- **The write path is genuinely layered.** Every store path on every backend goes through `neutralised_upload` (sanitise → active-content rename → recorded type forced to `text/plain` when defanged), including S3 and the deferred `put`; `SizeLimitedStorage::store_stream` caps mid-stream via `cap_stream` so even the buffered trait default can't launder a lie about `Content-Length`; control chars are stripped from both minted keys and the retained filename. The audit_2 #1/#2/#8/#11/#12 fixes are visibly load-bearing in the code, not just in the log.
- **The media-access cache is the strongest auth-adjacent surface this reviewer has read in the sweep.** Length-prefixed key components with an injectivity unit test (`media_cache_key`, `lib.rs:103-110`, test at `:1478-1485`), `bust_epoch()` captured *before* the closure runs and stored through `set_tagged_bool_guarded` (`lib.rs:459-476`), per-tag precision, and fail-closed defaults (`no row` → deny for owner gates).
- **The gate is shared, not duplicated.** One `media_gate::allows` serves both the FS middleware and the proxy; the signed path uses a constant-time compare (`ct_eq`, `media_gate.rs:110-113`) with the key AND expiry inside the HMAC.
- **Comments document intent and provenance unusually well** (each fix names its gap number). That is why the two wrong comments in STORAGE-2 stand out — elsewhere, comments were load-bearing evidence for this review rather than noise.
- **Fail-closed error handling on the serve path**: unknown key behind a gate → 404, retrieve errors → 500 with the key logged, no-oracle responses.

## Clean areas checked

- **ORM-not-raw-SQL**: `grep -rn "sqlx::query" plugins/umbral-storage/src/` → zero hits; every row touch (MediaFile saves, owner stamping, status persistence) goes through the ORM.
- **`unsafe`**: zero occurrences in `src/`.
- **Path traversal on the STORE side**: `sanitise_filename` strips separators/control chars before key minting on FS, S3 and deferred paths (`media.rs` tests `sanitise_filename_strips_separators_and_control_chars`, `media_fs_storage.rs` traversal test); the serve side is STORAGE-1.
- **Symlink escapes**: both FS serving arms wrapped in `SymlinkGuardService` (`lib.rs:1079`), with `media_symlink_escape.rs` / `static_symlink_escape.rs` covering them.
- **Signed URLs**: HMAC-SHA256 over `<decoded key>:<exp>`, constant-time compare, expiry checked before compare, unsigned/expired/tampered unit tests present (`media_gate.rs:138-200`).
- **Mid-stream cap primitive**: `cap_stream` / `is_cap_exceeded` wired through `SizeLimitedStorage::store_stream` and `save_stream_through`, with the `TooLarge` mapping covered by `media_streaming.rs`.
- **Static side**: conditional requests (etag/304), cache headers, collectstatic and embedded sides have dedicated test files; the static `put` path is the only writer and is ORM-free by design (DDL/asset exception N/A).
- **Injection**: no query construction from user input anywhere in the crate (no SQL at all); proxy response headers are fixed literals or `mime_guess`-derived valid tokens.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps3 #52` — No direct-to-S3 presigned upload (`presign_get` ships; uploads pass through the app) `[open]`
- `gaps6 #21` — Stale code doc-comments batch (storage instance (d) is `lib.rs:434`; my STORAGE-6/STORAGE-8 are new instances not in the list) `[open]`
- `gaps6 #9` — Media access: cache-first signal-invalidated authorization `[fixed/verified held]`
- `gaps6 #12` — `media_access_cached` write-skew guarded store (epoch captured before compute at `lib.rs:469`, stored via `set_tagged_bool_guarded` at `:473`) `[fixed/verified held]`
- `gaps6 #13` — Media-access cache key injectivity (length-prefixed; unit test present) `[fixed/verified held]`
- `gaps6 #14` — Non-PK delete invalidation (storage-side test `media_invalidation_delete_nonpk.rs` present) `[fixed/verified held]`
- `gaps4 #56/#58` — Signed media URLs and the gated non-FS proxy (the route STORAGE-1 hardens) `[fixed/verified held]`
- `audit_2 plugin-storage-tasks #1/#2/#8/#11/#12` — S3 active-content guard, 25 MiB default cap, SymlinkGuardService, tracing in place of `eprintln!`, control-char strip `[fixed/verified held]`

## Out-of-scope notes

- **Core ORM (MD-2 lead)**: `validate_storage_key` (`crates/umbral-core/src/orm/file_field.rs:61`) is `pub(crate)` and called only from serde `Deserialize` (`:227`); keys written by any other path (raw `update_values`, transfer/backup, direct SQL) reach storage unvalidated. The storage-side backstops cited as the mitigation do **not** cover it: the delete path joins the raw key at `media.rs:233-235` via `delete_blob_best_effort` (`:324`). Routes to the core ORM slice.
- **Core web (WEBL-3 lead)**: the `StreamingResponse::into_response` content-type `.expect(...)` is **not** reachable from this crate — storage never echoes a stored/user-supplied content type into a response builder; the proxy derives it from `mime_guess::from_path` (`lib.rs:1150-1153`), and the FS arm serves through ServeDir. No storage-side hardening needed; WEBL-3 stays a core-only issue.
- **Core `Storage` trait**: the default `exists` is `Ok(self.retrieve(key).await.is_ok())` (`crates/umbral-core/src/storage.rs:273-275`), silently mapping backend errors to `false` (same shape as STORAGE-5, one level up). Noted for the core slice; not counted here.
