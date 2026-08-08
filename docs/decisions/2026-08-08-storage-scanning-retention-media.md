# Storage: scanning/quarantine, retention/legal-hold, and a fuller media pipeline

Status: draft (proposes gaps5 #59 malware scanning, #61 retention/lifecycle/legal-hold, and #62 the media processing pipeline; the final call is the maintainer's)
Date: 2026-08-08
Drafts: planning/gaps5.md #59 (tf #272), planning/gaps5.md #61 (tf #274), planning/gaps5.md #62 (tf #275)
Relates: planning/gaps5.md #34 (docs/decisions/2026-08-08-search-and-data-governance.md, the data-governance metadata this ties into), docs/decisions/2026-08-08-product-north-star.md (Stage 2 self-hosted platform posture)

## Framing

Three storage items in one doc because they share one spine: all three are extensions of the **processor seam and the `MediaFile` tracking row that `umbral-storage` already owns**, and all three lean on `umbral-tasks` (queue plus beat) for their asynchronous work rather than adding a second delivery mechanism. Like the search and governance items, they are plugins/config over primitives that already exist, not new engines. None of them belong in `umbral-core`; scanning, retention, and heavy media transcoding are all optional and app-policy-shaped, so they ride the plugin the storage crate already is.

### What already exists (the real substrate, from `plugins/umbral-storage/src/lib.rs` and `media.rs`)

Everything below builds on the following concrete surface, so the designs parameterize and extend it rather than reinvent it:

- **The `MediaFile` tracking model** (`media.rs`), one row per tracked upload: `id`, `key` (the storage key), `filename`, `content_type`, `size`, `uploaded_at`, `status`, and `owner: Option<String>` (the owner's PK string, gaps4 #57). Every column is `#[umbral(noedit)]`. It is contributed as a migration only when a media side is configured (`StoragePlugin::models`).
- **The `status` lifecycle**: `STATUS_READY` (`"ready"`, the default a plain upload lands in), `STATUS_PROCESSING` (`"processing"`, a background task or a deferred write is in flight), and `STATUS_FAILED` (`"failed"`, a processor or deferred write errored). `status` defaults to `"ready"` so the column is an additive, backfill-safe migration. A `status` change persisted through the ORM fires `post_save:media_file`, which an app forwards to the frontend with `RealtimePlugin::new().expose::<MediaFile>(...)` - umbral-storage never imports realtime.
- **The `Processor` seam**: `type Processor` is an `Arc`'d async fn over a `MediaFile`. Processors are registered with `StoragePlugin::on_upload(...)`, run in registration order after a file's bytes land in storage, and are installed *ambiently* at `on_ready` (`media::set_processors`) so EVERY save path triggers them - `save`, `save_stream`, `save_deferred`, and the admin/form multipart upload through the `MediaTracking` decorator. While processors run the row is `"processing"`; on all-ok it flips to `"ready"`, on any error to `"failed"`. Processing runs via an in-process `tokio::spawn` today, and the docstring already states the crash-durability escape hatch: **have the processor enqueue an `umbral-tasks` job instead of doing the work inline.** That escape hatch is the load-bearing seam for all three items here.
- **The `images` feature** (`images.rs`): `thumbnails(&[Thumbnail])` returns a `Processor` that writes one resized variant per spec at a **derived key** (`variant_key(original, "thumb")` -> `…__thumb.png`), so a variant's URL is a pure function of the original's - no extra column, no join. It decodes on `spawn_blocking` under an ambient concurrency cap, never upscales, always preserves aspect ratio, and uses `store_at` (not `store`) to honor the derived-key contract. Non-image uploads pass through untouched.
- **The storage decorators**: `SizeLimitedStorage` (mid-stream `max_size` cap, default `DEFAULT_MAX_UPLOAD_SIZE` = 25 MiB), `TypeLimitedStorage` (the `accept`/`accept_images` allow-list, sniffing the bytes, not the declared `Content-Type`; `IMAGE_TYPES` deliberately excludes SVG), and `MediaTracking` (records the `media_file` row on the form/admin path). They stack as `MediaTracking(TypeLimited(SizeLimited(backend)))`, so any decorator added here is inherited by every upload route including ones written later.
- **The access seam**: `MediaAccessFn` plus `media_access` / `media_access_identity` / `media_access_owner` / `media_signed_urls` / `signed_media_url`, enforced on the FS `ServeDir` guard layer and on the non-FS proxy route (gaps4 #58). `set_media_owner(key, owner)` stamps the `owner` column.
- **The `Storage` trait**: `store`, `store_at`, `retrieve`, `retrieve_stream`, resolves ambiently via `umbral::storage::storage()`. Plugin code goes through it, never raw `sqlx` on app tables; the only raw SQL allowed is schema DDL owned by a plugin's own migration.

The honest gaps, and therefore the reason these three items exist: an upload's bytes are never **scanned** (a malicious file lands ready-to-serve), a `MediaFile` row has no **retention or lifecycle** concept (blobs live forever, no quota, no legal hold), and the media pipeline stops at same-format **thumbnails** (no `srcset` variants, no EXIF stripping, no AVIF/WebP transcoding, no video/audio, no moderation hook).

---

## Part 1: Scanning, quarantine, and moderation (gaps5 #59)

### The gap

Today an upload's bytes land in storage and the `MediaFile` row is `"ready"` (or, with processors, `"processing"` then `"ready"`) with no content inspection beyond the size cap and the `accept` type allow-list. The type allow-list sniffs the magic bytes, so it stops an `.exe` renamed to `avatar.png`, but it does not stop a genuine PNG carrying a malware payload, a PDF with an embedded exploit, or content that violates policy. For any app taking uploads from untrusted users (a marketplace, a support portal, a tenant document store), "the file is scanned before anyone can download it" is table stakes.

### Design: a scanner is a `Processor`, quarantine is a `status`, approval is a task

The whole item fits the existing seam. A scanner is nothing more than a `Processor` that reads the bytes and decides a verdict; quarantine is a new value of the `status` column; async approval is an `umbral-tasks` job. Concretely:

**1. A quarantine status on the `MediaFile` row.** Extend the lifecycle vocabulary with two additive states alongside `ready` / `processing` / `failed`:

- `STATUS_QUARANTINED` (`"quarantined"`) - the bytes are stored but flagged: a scanner returned a positive or the file awaits human moderation. A quarantined file is **not served**: the access seam (`MediaAccessFn` on the FS guard and the non-FS proxy) consults the row's status and returns the same `forbidden_media()` 403 it uses for a denied access check, so quarantine is enforced on the read path the plugin already gates, not as a second mechanism. This is a framework-owned status gate, distinct from the app's own `media_access` closure; both must pass.
- `STATUS_REJECTED` (`"rejected"`) - a moderator (or a scanner policy) has denied the file permanently. Same non-serve behavior; distinguished from `quarantined` so the admin can filter "awaiting review" vs "already declined."

Because `status` is already `noedit` with a `default = "ready"`, adding two accepted values is a code change, not a schema change; the column stays `max_length = 16` (both fit). Existing rows keep backfilling to `"ready"`.

**2. Scanners as a scanner-kind `Processor`, run before the file is releasable.** A scanner has the shape:

```rust
StoragePlugin::new()
    .media("/media", "./media")
    .scan(ClamAvScanner::from_env())        // TCP/socket to a clamd daemon
    .scan(VendorScanner::new(api_key))      // a cloud scanning API
    .quarantine_policy(QuarantinePolicy::HoldUntilClean)  // default: hold new uploads as "processing"->"quarantined"/"ready"
```

`scan(...)` registers a scanner ahead of the ordinary `on_upload` processors. A scanner returns a `Verdict` (`Clean`, `Infected { signature }`, `Suspicious { reason }`, `Undetermined`). The plugin maps the verdict to the row's terminal status: `Clean` continues to `ready` (or on into the media processors), `Infected`/`Suspicious` moves the row to `quarantined`, an unreachable scanner under `HoldUntilClean` leaves it `quarantined` (fail closed) while `AllowOnScannerError` lets it proceed with a logged warning (an operator posture choice, defaulting to fail-closed). The scanner reads bytes through `umbral::storage::storage().retrieve(&media.key)` exactly as `thumbnails` does, and the CPU/IO-bound scan runs on `spawn_blocking` or, for a network scanner, a bounded async call - the same concurrency-cap discipline the image processor already uses.

Two built-in adapters, each feature-gated so an app compiles only what it uses:

- **`ClamAvScanner`** (feature `clamav`) - speaks the clamd `INSTREAM` protocol over TCP or a unix socket to a ClamAV daemon the operator runs (the north-star Stage-2 deployment reference documents it as a sidecar). Thin client, no bundled engine, no signature database in-process.
- **`VendorScanner`** (feature `scan-http`) - a `reqwest`-based adapter to a cloud scanning/DLP API (VirusTotal-style, or an org's own endpoint), behind a small `Scanner` trait so a shop can drop in its vendor. DLP content matchers (a credit-card / secret regex sweep) are the same `Scanner` trait returning `Suspicious`.

The `Scanner` trait is the extension point; ClamAV and the HTTP vendor are two implementations, and a third-party scanner is structurally identical, matching the plugin ethos.

**3. Crash-durable scanning via `umbral-tasks`.** The in-process `tokio::spawn` the processor seam uses is fine for a fast local clamd, but a slow vendor API or a large file wants durability: a worker crash mid-scan must not silently leave a file `processing` forever. The sanctioned pattern (already documented on `on_upload`) is that the scanner **enqueues an `umbral-tasks` job** (`scan(table, pk)`) rather than scanning inline; the worker runs the scan and persists the terminal status through the ORM. This reuses the queue's existing retry/backoff (a transient scanner outage retries) and its durability (a restart resumes pending scans), and it means the upload request returns immediately with the row in `processing`/`quarantined`, never blocking on the scanner. `save_deferred` already returns immediately with a `processing` row, so the deferred-upload path and the scan-queue path compose naturally.

**4. Admin moderation workflow.** The `MediaFile` model is already admin-registerable. The plugin adds a moderation surface over it: an admin list filtered to `status IN ('quarantined','rejected')`, with row actions `approve` (-> `ready`, the file becomes servable and any downstream media processors run) and `reject` (-> `rejected`, permanently non-served, optionally purged). These are ordinary admin actions writing the `status` column through the ORM, so they fire `post_save:media_file` and a realtime-exposed board updates live. Who may moderate is the admin's existing `Widget::permission` enforcement; the approval decision itself is one ORM write, not a bespoke state machine.

### What is deferred (#59)

- A full moderation **queue with SLA timers, escalation, and multi-reviewer consensus** - v1 is a two-action admin list; a richer workflow is the same shape as the DSAR workflow engine (gaps5 #86) and can be layered later.
- **Perceptual-hash / CSAM-database matching** and ML content classification - these are `Scanner` implementations an org plugs in, not framework-bundled engines.
- **Re-scanning on signature-database updates** (sweep old `ready` files when clamd's DB updates) - a beat job in the same family as the retention sweep below; logged, not shipped in v1.

---

## Part 2: Retention, lifecycle, and legal hold (gaps5 #61)

### The gap and the tie to #34

A `MediaFile` row and its blob live forever. There is no retention horizon, no tenant quota, no legal hold, and no purge job. This is the storage-side counterpart of the data-governance metadata in #34 (docs/decisions/2026-08-08-search-and-data-governance.md): #34 defines **retention classes, `RetentionAction`, the legal-hold table, and the beat-driven retention sweep for ORM model columns**; this item applies that exact machinery to the `MediaFile` row and its backing blob, so storage retention is not a parallel invention but the same governance vocabulary extended to files. Where #34's sweep anonymizes a column, storage retention additionally has a blob to delete - that is the only new verb.

### Design: retention labels on the media row, tied into the #34 registry

**1. Retention labels on `MediaFile`.** A tracked upload gets a governance-classified retention, reusing #34's retention-class registry rather than a storage-local one:

- The `MediaFile` model's `status`-adjacent metadata gains a nullable `retention: Option<String>` column (the retention-class name from #34's registry) and a nullable `expires_at: Option<DateTime<Utc>>` (computed from the class's duration at upload time, or left `NULL` for classes with no horizon). Both additive and backfill-safe (`NULL` = "no retention policy", the current behavior).
- A media side names a default retention class, and an upload site can override per file:

```rust
StoragePlugin::new()
    .media("/media", "./media")
    .default_retention("user_uploads")     // a class registered on GovernancePlugin (#34)
    .tenant_quota(TenantQuota::per_owner(5 * GIB))   // see below
```

The `retention` value keys into #34's `GovernancePlugin` class registry (`Retention::days(...).on_delete(RetentionAction::...)`), so the durations and actions live in ONE place and the media row points at them - identical to how a `#[umbral(retention = "customer_data")]` column points at a class. The boot check that #34 already runs (a `retention` naming an unregistered class is a config error) covers the media default too.

**2. Bucket lifecycle mapping.** For an S3-backed media side (`media_s3` / `media_with_storage`), a retention class can additionally project onto **object-store lifecycle rules** so cold blobs transition/expire in the bucket without the app streaming every byte back through a purge job. This is a backend-specific feature the ORM does not model, so it is the sanctioned exception: gated on the S3 backend, expressed through the S3 client's lifecycle-configuration API in the plugin, with the FS backend falling back to the beat-driven purge below (never a silent divergence - the FS branch does the same deletion, just app-side). The retention CLASS is the source of truth; the bucket rule is a projection of it, so the two cannot drift.

**3. Tenant quotas.** `TenantQuota` caps the total bytes an owner (the `owner` PK string) may hold. It is enforced at upload time as a decorator in the same `MediaTracking(TypeLimited(SizeLimited(...)))` stack - a `QuotaLimitedStorage` that sums the owner's `SELECT COALESCE(SUM(size),0) FROM media_file WHERE owner = ? AND status != 'rejected'` through the ORM's `count`/aggregate surface and refuses the write with a typed `MediaError::QuotaExceeded` before buffering the body. Because it is a decorator, every upload route inherits it, exactly as the size and type caps do. (If the aggregate-sum-by-owner shape is not already on the ORM's terminal surface, that is an ORM gap to file per CLAUDE.md, not a raw-SQL workaround.)

**4. Legal hold.** A legal hold suspends retention-driven deletion for a set of files regardless of their retention class. This reuses #34's `governance_legal_hold` table directly - a hold keyed by subject id and/or `(table, pk)` scope covers `media_file` rows the same way it covers any other table. The storage purge job and any DSAR delete consult the hold table first and **skip held rows**, logging the skip rather than deleting silently. A file whose `owner` matches a held subject is protected; the blob is never purged while the hold stands.

**5. Soft-delete windows.** Deleting a tracked file becomes a two-phase operation: a soft delete stamps the row (`status = "trashed"`, plus a `trashed_at`) and starts a recovery window; the blob is not removed yet. The existing `cleanup_on_delete` blob-removal fires only when the window elapses (or on an explicit hard delete/`purge`), so an accidental delete is recoverable and a "trash" UI is possible. This composes with the existing FileField cleanup (gaps2 #92, remove the OLD blob on replace) - a replaced blob enters the same window rather than being destroyed immediately.

**6. Beat-driven purge jobs.** A scheduled `umbral-tasks` beat job (the same substrate #33's external sync and #34's retention sweep use) runs periodically:

- Finds `media_file` rows past `expires_at` (or past their soft-delete window), skips any under legal hold, and applies the retention-class `RetentionAction`: `HardDelete` removes the row through the ORM AND deletes the blob through `storage().` (the file-specific verb #34's column sweep does not have), `Anonymize` crypto-shreds a `Masked`-wrapped blob key / tombstones metadata while leaving an audit stub, `Retain` leaves legally-mandated files in place.
- Every action is logged for the audit trail (the same trail #34/#86 assemble), and blob deletion is best-effort with a logged secondary error, never a silent `.ok()`.

All row-level reads/writes go through the ORM; the only raw SQL is the S3 lifecycle-config call (backend-specific exception) and any DDL in the plugin's own migration (the new `MediaFile` columns and, if needed, a hold-scope index).

### What is deferred (#61)

- **Cross-region / residency-aware retention** (retention that differs by `residency` tag) - depends on #34's residency routing (gaps5 #85), deferred by the north star.
- **Immutable / WORM object-lock** (S3 Object Lock compliance mode) - a bucket-config extension of the lifecycle mapping; logged as a follow-up once the lifecycle projection ships.
- **A retention/purge dry-run report UI** beyond the CLI - the beat job and a `umbral storage retention` inspect command ship first.

---

## Part 3: A fuller media processing pipeline (gaps5 #62)

### The gap

The `images` feature ships `thumbnails(&[Thumbnail])` - same-format resized variants at derived keys. That is the right seam and the right key contract, but the pipeline stops there: no responsive `srcset` set with a manifest, no EXIF stripping (an uploaded photo leaks GPS/camera metadata to every viewer), no next-gen format transcoding (AVIF/WebP), no video/audio handling, and no moderation hook wired into the variant flow. #62 extends the existing `images` processor, it does not replace it - the derived-key contract, the no-upscale rule, the `spawn_blocking` + concurrency cap, and the pass-through-non-images behavior all carry forward.

### Design: variant policies over the existing processor seam

**1. A variants/`srcset` policy.** Generalize `thumbnails(&[Thumbnail])` to a `MediaPipeline` builder that emits a named set of variants and records enough to build an HTML `srcset`:

```rust
StoragePlugin::new()
    .media("/media", "./media")
    .on_upload(
        MediaPipeline::images()
            .strip_exif()                                   // default ON for images
            .srcset(&[320, 640, 1280])                      // width-based responsive set
            .transcode(FormatPolicy::PreferAvif.then_webp()) // next-gen with fallback
            .keep_original(),                                // the source is still served
    )
```

Each width variant lands at a derived key (`variant_key(original, "w640")` -> `…__w640.avif`) so, as today, a variant's URL is a pure function of the original's - the template builds the `srcset` string from the original key and the width list with **no extra column and no join**. Where the app needs the exact generated set (e.g. some widths were clamped away by the no-upscale rule), the pipeline can optionally record a small variant manifest keyed to the `MediaFile`; the default derived-key path needs none. This preserves the "no second lookup" property that makes the current thumbnails design good.

**2. EXIF stripping, on by default for images.** Re-encoding through the `image` crate already drops most metadata; the pipeline makes it explicit and guaranteed (`strip_exif()` defaulting ON), so an uploaded JPEG's GPS coordinates and camera serial never reach a viewer. This is a privacy default, not an opt-in - the same "secure/private by default" posture as CSRF-on and the fail-closed owner gate. An app that genuinely needs to preserve EXIF (a photography portfolio) opts out explicitly.

**3. AVIF/WebP transcoding policy.** `FormatPolicy` decides the output codec: `PreferAvif.then_webp()` emits AVIF with a WebP fallback variant, `Keep` preserves the source format (today's behavior), `Force(fmt)` pins one. AVIF/WebP encoding is CPU-heavy, so it runs on `spawn_blocking` under the existing concurrency cap, and it is feature-gated (the AVIF encoder is a heavy dependency) so an app that does not transcode never compiles it. The derived key carries the format extension, so the fallback variant is addressable without a lookup.

**4. Optional video/audio.** Behind a separate `media-av` feature, a variant policy can shell out to an operator-provided `ffmpeg` (the plugin does not bundle a codec; it drives an external binary the Stage-2 deployment reference documents) to produce a poster frame, a web-friendly transcode, or an audio waveform. This is durability-sensitive and slow, so it is **always** the enqueue-an-`umbral-tasks`-job path, never inline `tokio::spawn` - a 200 MB video transcode must survive a worker restart. The row sits at `processing` until the worker finishes, and realtime exposure notifies the frontend, exactly the deferred-upload flow that already exists.

**5. Moderation hooks.** The pipeline exposes a hook that runs a `Scanner`/moderation check (Part 1's `Scanner` trait) as a pipeline stage: a variant-generation pipeline can gate on a content-moderation verdict, moving the row to `quarantined` before any variant is served. This is where #62 and #59 meet - the media pipeline and the scanner pipeline are both ordered `Processor` chains over the same `MediaFile`, so composing them ("strip EXIF, generate srcset, then run image moderation, hold if flagged") is just ordering stages, not wiring two subsystems.

### Why this stays on the existing seam

Every stage above is a `Processor` (or a job the processor enqueues), registered through `on_upload`, installed ambiently at `on_ready`, driving the same `processing`/`ready`/`failed`/`quarantined` lifecycle, reading and writing blobs through the `Storage` trait, and writing derived keys with `store_at`. Nothing here is a new subsystem; it is the thumbnails processor grown up. That is the point: the framework already gave the app the seam and one part to put in it (thumbnails); #62 fills the seam with the rest of the parts a real media app needs, so the app does not re-solve EXIF/srcset/transcoding itself.

### What is deferred (#62)

- **On-the-fly / lazy variant generation** (generate a width the first time it is requested, cache it) - v1 is eager at upload; lazy generation is a follow-up over the same derived-key contract.
- **A CDN-integrated image-transform URL API** (`?w=640&fmt=avif` transform proxy) - depends on the lazy path; logged.
- **ML-based smart cropping / focal-point detection** - a codec-adjacent feature an app plugs in; not framework-bundled.

---

## Why these are three items in one doc

They share one substrate and one asynchronous engine. Scanning is a `Processor` returning a verdict; a fuller media pipeline is a richer set of `Processor` stages; retention is a `MediaFile` label swept by a beat job. All three drive the same `MediaFile.status` lifecycle, enforce on the same access/serve seam, read and write blobs through the same `Storage` trait, and use `umbral-tasks` (queue plus beat) for anything durable - never a second delivery mechanism. And all three tie back to the governance metadata of #34: quarantine and moderation are content governance, retention/legal-hold literally reuses #34's registry and hold table, and EXIF stripping is the same privacy-by-default posture the confidentiality tiers embody. Keeping them in one doc records that shared shape: umbral grows storage depth by extending the processor seam and the tracking row it already owns, never by bolting on a parallel media service.
