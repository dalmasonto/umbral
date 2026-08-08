# Direct-to-object-storage uploads and CDN integration (design)

Status: draft (planning/gaps5.md #58 tf#271, and #60 tf#273)
Date: 2026-08-08
Realizes Stage 2 (self-hosted platform posture) from `docs/decisions/2026-08-08-product-north-star.md`.

This document extends the shipped `umbral-storage` plugin. It does not restate the plugin; it names the exact surface it builds on and adds two capabilities that stop uploads and downloads from having to pass through the Rust app:

- Part 1 (#58): first-class direct-to-object-storage uploads, so a browser PUTs bytes straight to S3 (or an S3-compatible store) instead of streaming them through an axum handler, with a completion callback that finalizes the tracking row and triggers the existing processor pipeline.
- Part 2 (#60): a CDN integration layer, so public bytes are served from an edge cache with real cache-invalidation, private bytes are gated by the CDN's own signing scheme, and image transforms happen at the edge.

Neither part changes the `Storage` trait's existing contract or the `StoragePlugin` builder's existing methods; both add optional, additive surface. A storage-free app, and a storage app that wants neither direct uploads nor a CDN, compiles and runs exactly as today, per the thin-core rule.

## The ground truth this builds on (real names, `plugins/umbral-storage/src/`)

The design has to fit the code that exists. The load-bearing pieces:

- The core `Storage` trait (`crates/umbral-core/src/storage.rs`): `store(filename, content_type, bytes) -> StoredFile`, `store_at(key, ...)`, `put(key, ...)` / `put_stream`, `store_stream`, `retrieve(key) -> Vec<u8>` / `retrieve_stream(key) -> ByteStream`, `exists(key) -> bool`, `delete(key)`, and the SYNC `url(key) -> String`. `StoredFile { key, url, size }`. `StorageError { NoBackend, NotFound, TooLarge, UnsupportedType, Io, Backend, Unsupported }`. The additive methods (`store_at`, `put`) default to `StorageError::Unsupported`, which is the exact pattern this doc reuses for the new presign methods.
- The ambient registry: `set_storage_named(DEFAULT, ...)` / `storage_named(name)` / `storage()`; `DEFAULT = "default"` (media), `STATICFILES = "staticfiles"` (collected assets).
- The S3 backend (`s3.rs`, feature `s3`): `S3Storage` + `S3StorageBuilder` with `.region()`, `.endpoint()`, `.prefix()`, `.public_base()`, `.credentials(access, secret, token)`, `.path_style(bool)`, `.presign(ttl_secs)`, plus `S3Storage::from_env()` reading `UMBRAL_S3_*`. Presigned GET already exists: `url()` calls `presign_blocking()` -> `bucket.presign_get(object_key, ttl, None)` (rust-s3 0.35, blocking calls wrapped so the reactor is present, per gaps4 #59). `public_base` is the public/CDN URL base; `presign_ttl` takes precedence over it for private buckets.
- The plugin (`lib.rs`): `StoragePlugin::media("/media", "./media")` (FS backend), `.media_with_storage(mount, Arc<dyn Storage>)`, `.media_s3(mount, S3Storage)`; `.save()` / `.save_stream()` / `.save_deferred()`; `.on_upload(processor)`; the access gate (`MediaAccessFn`, `.media_access()`, `.media_access_identity()`, `.media_access_owner()`); `.media_signed_urls()` + `signed_media_url(mount, key, ttl)`; `.accept()` / `.max_size()`; `.cleanup_on_delete::<M>()`.
- The tracking model (`media.rs`): `MediaFile { id, key, filename, content_type, size, uploaded_at, status, owner }` with `status` in `{"ready", "processing", "failed"}` (`STATUS_READY` / `STATUS_PROCESSING` / `STATUS_FAILED`); `MediaSaveOutcome { file, url }`; `set_media_owner(key, owner)`; the `Processor` pipeline installed ambiently at `on_ready` via `set_processors`.
- The media gate (`media_gate.rs`): `key_for_path`, `signed_media_url`, and `allows(signed_urls, access, query, headers, key)` shared by the FS serve layer and the non-FS proxy route (gaps4 #58) that streams gated bytes through `retrieve_stream` when a custom/S3 backend is gated.

The through-line for both parts: today, on a gated custom/S3 backend, both the upload (`save_stream` buffers/streams through the app) AND the gated download (the gaps4 #58 proxy re-reads bytes through `retrieve_stream`) pass through the Rust process. That is correct and safe, but it makes the app the byte path for large media. #58 removes the app from the upload path; #60 removes it from the public download path. The gated proxy stays as the fallback for backends that cannot presign.

---

# Part 1 (gaps5 #58): first-class direct-to-object-storage uploads

## What #58 asks for, against what exists

gaps5 #58: "storage supports S3 and signed media URLs, but upload flows still largely pass through the Rust app. Add presigned POST/PUT, multipart uploads, resumable uploads, client SDK helpers, and completion callbacks."

Today an upload always reaches the app: a multipart form POST or a `save_stream` call streams the whole body through axum, through the `SizeLimitedStorage` / `TypeLimitedStorage` decorators, into the backend, and only then writes the `media_file` row. For a 2 GB video on a single web replica that is a memory/latency tax the object store was built to avoid. The `Storage` trait already presigns GETs (`presign_get` under `url()`); #58 is the write-side symmetry plus the finalize step that keeps `media_file` truthful when the bytes never touched the app.

## New optional trait surface: presign the write, additively

The presign methods are added to `Storage` the same way `store_at` and `put` were: async methods with a default body returning `StorageError::Unsupported`, so every existing backend keeps compiling and only a backend that can presign (S3 and S3-compatible) overrides them. The FS backend does NOT override them; it keeps the through-the-app path (there is nothing to presign to on a local disk).

```rust,ignore
// crates/umbral-core/src/storage.rs, on `trait Storage`
/// A time-bounded credential a client uses to upload bytes DIRECTLY to the
/// backend, bypassing the app. Default: Unsupported (FS/custom backends).
async fn presign_put(
    &self,
    key: &str,
    content_type: &str,
    opts: PresignUploadOpts,   // ttl, max_size, required content-type
) -> Result<PresignedUpload, StorageError> {
    let _ = (key, content_type, opts);
    Err(StorageError::Unsupported("this backend cannot presign uploads (presign_put)".into()))
}

/// A browser-form POST policy (multiple fields + a base64 policy document),
/// for uploads from an HTML form / fetch without a preset key. Default: Unsupported.
async fn presign_post(
    &self,
    key_prefix: &str,
    opts: PresignUploadOpts,
) -> Result<PresignedPost, StorageError> { /* default Unsupported */ }

// Multipart / resumable: the four S3 multipart calls, each Unsupported by default.
async fn create_multipart(&self, key: &str, content_type: &str) -> Result<MultipartUpload, StorageError>;
async fn presign_part(&self, upload: &MultipartUpload, part_number: u16, opts: PresignUploadOpts) -> Result<PresignedUpload, StorageError>;
async fn complete_multipart(&self, upload: &MultipartUpload, parts: &[CompletedPart]) -> Result<StoredFile, StorageError>;
async fn abort_multipart(&self, upload: &MultipartUpload) -> Result<(), StorageError>;
```

New value types, all serde-serializable so a handler returns them straight to the client as JSON:

- `PresignedUpload { url, method, headers, expires_at }` - a single presigned URL the client PUTs to, plus the exact headers it must echo (Content-Type, and any x-amz-* the signature covers).
- `PresignedPost { url, fields }` - the S3 POST form: the target URL and the map of hidden form fields (`key`, `policy`, `x-amz-signature`, ...) the browser submits alongside the file. This is the path for an unauthenticated-key upload where S3's POST policy conditions (content-length-range, content-type prefix, key prefix) enforce the caps at the edge, not the app.
- `MultipartUpload { key, upload_id }`, `CompletedPart { part_number, etag }` - the S3 multipart handshake, which IS the resumable primitive: a client that loses its connection re-lists the uploaded parts and continues, and never re-sends a completed part.
- `PresignUploadOpts { ttl, max_size, content_type }` - the caps baked INTO the signature so they are enforced by S3, not merely requested. `max_size` becomes the POST policy's `content-length-range`; `content_type` becomes an exact/prefix condition. This is the direct-upload analogue of `SizeLimitedStorage` / `TypeLimitedStorage`: on a through-the-app upload the decorators enforce mid-stream, on a direct upload the presign conditions enforce at the object store. Both caps come from the same `StoragePlugin` config (`.max_size()`, `.accept()`) so the two paths never diverge.

The S3 impl wraps rust-s3's `presign_put` / `presign_post` / the multipart calls, driven off-runtime with the same `spawn_blocking` / `block_in_place` discipline `presign_get` already uses (gaps4 #59) so the sync-over-async presign never wedges the reactor.

## The plugin-level flow: a two-call handshake plus finalize

`StoragePlugin` gains a direct-upload mode and mounts (opt-in) a small pair of routes under the media mount. The app keeps ownership of the policy decision (who may upload, into what key namespace, how big); S3 keeps ownership of the bytes.

```rust,ignore
StoragePlugin::new()
    .media_s3("/media", S3Storage::from_env()?)   // a presign-capable backend
    .accept_images()                              // -> presign content-type condition
    .max_size(50 * 1024 * 1024)                   // -> presign content-length-range
    .direct_uploads(                              // NEW: opt in to the direct path
        DirectUploads::new()
            // who is allowed to START an upload (reuses the identity seam gaps4 #42)
            .authorize(|caller, _req| async move { caller.is_some() })
            // stamp the owner on the finalized row, exactly like set_media_owner today
            .owner_from_identity(),
    );
```

`.direct_uploads(...)` mounts two routes on the media router (only when the backend reports presign support at boot; otherwise it logs and the app falls back to `save_stream`):

1. `POST <mount>/uploads/sign` - the app runs the `authorize` closure (the SAME `Option<Identity>` resolution `media_access_identity` uses, so no new auth dep), mints a key, calls `presign_put` / `presign_post` / `create_multipart` on the backend with the plugin's caps, and inserts a `media_file` row with `status = "processing"` (the file does not exist yet). It returns the `PresignedUpload` / `PresignedPost` / `MultipartUpload` JSON. The `"processing"` row is the reservation: its URL 404s until the bytes land and finalize runs, which is byte-identical to how `save_deferred` already uses `"processing"`.

2. `POST <mount>/uploads/complete` - the client calls this after its direct PUT/POST/multipart succeeds. The app VERIFIES the object actually landed (`storage.exists(key)` -> a real S3 HEAD, and reads back the true size), then flips the `media_file` row to `status = "ready"` (or, when processors are registered, kicks the existing `Processor` pipeline via the same ambient `set_processors` path `save`/`save_deferred` use, leaving `status = "processing"` until they finish, then `"ready"` / `"failed"`). Finalize is idempotent on the `media_file` id so a double-call, or a race with the S3-event path below, converges to one row. The completion response is the finalized `MediaSaveOutcome`, so a client gets the same shape whether it uploaded directly or through the app.

The honest reason finalize must HEAD-verify rather than trust the client: a client that POSTs `complete` for a key it never actually uploaded must not mint a "ready" row for a missing object. `exists()` reading the real object (and its real size, overwriting the client-declared size) is the trust boundary. The presign conditions already made it impossible to upload something oversized or of the wrong type; finalize confirms something landed at all.

## Completion via S3 event notification (the callback that needs no client cooperation)

The client-driven `complete` call is the common path, but a client can crash between the S3 PUT and the `complete` call, leaving a `"processing"` row over an object that DID land. So finalize is also reachable from the object store's own event stream: an S3 (or R2/MinIO) `s3:ObjectCreated:*` notification delivered to an umbral endpoint.

This is where #58 meets the webhook infrastructure from `docs/decisions/2026-08-08-unified-authz-dsl-and-webhooks.md` Part 2: the bucket is configured to POST object-created events (directly, or via SNS/SQS -> HTTP) to `POST <mount>/uploads/notify`. That endpoint verifies the notification's authenticity (the bucket's shared secret / signature, verified the same way `umbral_webhooks::verify_signature` verifies an outgoing webhook, so the two directions share one HMAC scheme), extracts the object key, and runs the SAME idempotent finalize as the `complete` route. The result: whether the client remembers to call back or not, every landed object converges to a finalized `media_file` row and its processors run once. A reconcile command (`umbral storage reconcile-uploads --older-than 1h`) sweeps `"processing"` rows whose objects exist but were never finalized, as the belt-and-braces backstop for a bucket with no event notifications configured at all.

## The client helper

The direct path is only "first-class" if the app author does not hand-roll the three-step browser dance. We ship a tiny, dependency-free JS/TS helper (served as a static asset, not an npm package, matching umbral's no-external-CDN posture) that wraps: call `sign`, PUT/POST the `File` to the returned URL (or run the multipart loop with resumable retry on part failure), call `complete`, and surface progress events. It is `~150 lines`, framework-agnostic, and does exactly what the two routes above expect - so "direct upload" is `umbralUpload(file, "/media/uploads")` on the client and the two routes on the server, nothing else. A Rust-side helper is unnecessary: the server side is the two route handlers the plugin already mounts.

## The gated-proxy path stays

For a backend that cannot presign (the FS backend, or a custom `Storage` with no direct endpoint), `.direct_uploads(...)` logs at boot that direct uploads are unavailable and the app keeps using `save` / `save_stream` through the app - unchanged. And for gated DOWNLOADS on a non-presign backend, the gaps4 #58 proxy route (streaming through `retrieve_stream` behind `allows(...)`) stays exactly as shipped. Direct uploads are an optimization for presign-capable backends, never a requirement; the through-the-app path remains the correct, always-available default, and remains the only path the security decorators (`SizeLimitedStorage`, `TypeLimitedStorage`, the active-content neutralizer in `store`) run inline on.

## What Part 1 deliberately does not do

- It does not remove or change `save` / `save_stream` / `save_deferred`. They are the through-the-app path and stay the default; direct uploads are opt-in per plugin.
- It does not presign on the FS backend. There is nothing to presign to; `presign_*` stays `Unsupported` there and the plugin says so at boot.
- It does not invent a resumable protocol. S3 multipart IS the resumable primitive; a non-S3 resumable scheme (tus) is a later backend concern, not core surface.
- It does not move the security decorators. On a direct upload the caps are enforced by the presign conditions (S3-side); the app's inline decorators still guard every through-the-app byte. Finalize HEAD-verifies rather than re-scanning bytes it never saw - malware/DLP scanning of directly-uploaded bytes is a processor that reads the object back (gaps5 #59's job), not #58's.

---

# Part 2 (gaps5 #60): CDN integration and cache invalidation

## What #60 asks for, against what exists

gaps5 #60: "collectstatic hashed assets exist and docs suggest CDN/proxy, but no CDN invalidation, signed cookies, edge cache policies, image CDN transforms, or cache purge hooks. Add CDN provider adapters and explicit cache invalidation APIs."

Today the CDN story is one string: `S3Storage`'s `public_base` (or `UMBRAL_S3_PUBLIC_BASE`), which makes `url()` return `https://cdn.example.com/<key>`. That is enough to put a CDN in front of a public bucket, but nothing invalidates the edge when a key is deleted or replaced, nothing signs an edge URL for private media (umbral's own `signed_media_url` HMAC is verified by the APP, so a signed private file still round-trips through the origin), and nothing does edge image transforms. #60 adds the adapter layer that closes those three gaps, riding the `public_base` URL rewriting that already exists.

## The `CdnProvider` trait and its adapters

A new small trait, in `umbral-storage` (it is storage-adjacent, plugin-only; `umbral-core` never learns CDNs exist), with one concrete adapter per major provider:

```rust,ignore
#[async_trait]
pub trait CdnProvider: Send + Sync {
    /// Rewrite a backend key/url to the edge URL a browser should fetch.
    fn edge_url(&self, key: &str) -> String;
    /// Invalidate/purge the given keys at the edge (after delete/replace).
    async fn purge(&self, keys: &[String]) -> Result<(), CdnError>;
    /// Sign an edge URL (or mint edge cookies) for PRIVATE media, delegating
    /// to the provider's own signing scheme so the EDGE enforces access.
    fn sign(&self, key: &str, ttl: Duration) -> Result<CdnSigned, CdnError>;
    /// Append provider-specific image-transform params to an edge URL.
    fn transform(&self, key: &str, t: &ImageTransform) -> String;
}
```

Shipped adapters (each feature-gated, each an ordinary `impl CdnProvider`, so a third party can add one identically - the plugin-contract dogfooding rule):

- `CloudFrontCdn` - purge via a CloudFront `CreateInvalidation` API call; `sign` mints CloudFront signed URLs or signed cookies (RSA with a key-pair id); `transform` targets a Lambda@Edge / CloudFront Functions image handler.
- `CloudflareCdn` - purge via the Cloudflare `purge_cache` API (by URL or by cache-tag); `sign` mints a Cloudflare signed URL (or leans on Cloudflare Access); `transform` emits the `/cdn-cgi/image/width=...,format=auto/<url>` path (Cloudflare Images / Image Resizing).
- `FastlyCdn` - purge via Fastly's instant `PURGE` (single URL) or surrogate-key purge; `sign` mints a Fastly signed URL / token; `transform` emits Fastly Image Optimizer query params (`?width=200&format=webp`).

Each adapter's credentials come from `UMBRAL_CDN_*` env (mirroring `UMBRAL_S3_*`), and each has a `from_env()` plus a builder, matching `S3Storage`'s two constructors.

## Wiring: one builder call, two behavioral changes

```rust,ignore
StoragePlugin::new()
    .media_s3("/media", S3Storage::from_env()?)
    .cdn(CloudflareCdn::from_env()?)          // NEW
    .cdn_transforms(true);                    // NEW: allow edge image transforms
```

`.cdn(provider)` does two things at `on_ready`:

1. URL rewriting. It wraps the registered backend so `url(key)` returns `provider.edge_url(key)` instead of the bare `public_base` join - the CDN domain becomes the public URL every `FileField`, template, and admin render resolves to. For a PUBLIC mount this is a pure win: bytes serve from the edge, the app and the bucket are off the hot path. For a PRIVATE mount (`.media_access*` / `.media_signed_urls()` configured), `url()` returns `provider.sign(key, ttl)` instead, so the edge - not the app - enforces the time bound. This is the CDN-native replacement for the app-verified `signed_media_url` HMAC: same "hand out a link good for N minutes" ergonomics, but the check happens at the edge, so private media no longer round-trips the origin. `signed_media_url` and the gaps4 #58 proxy stay for backends with no CDN adapter.

2. Purge hooks. The plugin's existing file-lifecycle cleanup (`cleanup_on_delete::<M>()` -> `register_cleanup`, which already fires on per-row delete and on file-key replace A->B, gaps2 #92) gains a second sink: after the backend `delete(old_key)` succeeds, it calls `provider.purge(&[old_key])`. Deterministic-key writes (`store_at`, the image-variant path) purge the rewritten key too, so a regenerated thumbnail is not served stale from the edge. Purge is best-effort and logged (a failed purge is a stale-cache warning, not a request failure), matching the best-effort posture the cleanup path already documents. A manual `umbral storage purge <key>...` command and a programmatic `cdn().purge(&keys)` cover the "invalidate this now" case the lifecycle hook does not (e.g. a bucket edited out of band).

## Edge cache policies

`.cdn(...)` also lets the plugin set the cache headers the edge honors, distinct from the browser `max_age` the static side already sets: a `CachePolicy` with `edge_ttl`, `browser_ttl`, and `stale_while_revalidate`, emitted as `Cache-Control` / `Surrogate-Control` on the origin responses (the FS serve layer, the gaps4 #58 proxy, and the `Content-Type`-carrying responses) so the CDN caches correctly and revalidates on the umbral origin. Collected static assets (the `collectstatic --hashed` path) get an immutable long-TTL policy because their key already contains a content hash; media gets a shorter, purgeable policy because its key is stable across content changes. This is the piece that makes "put a CDN in front" honest rather than "set one env var and hope".

## Image-CDN transform passthrough

The `images` feature already generates thumbnail VARIANTS eagerly (`thumbnails`, `variant_key`, written via `store_at`). Edge transforms are the complementary lazy path: instead of pre-generating every size, hand the browser a transform URL the CDN renders on first request and caches. `.cdn_transforms(true)` exposes a URL builder:

```rust,ignore
// in a template / handler, given a MediaFile key:
let src = umbral_storage::cdn_image(&key, ImageTransform::new().width(400).format(Format::WebP));
// Cloudflare -> https://cdn/cdn-cgi/image/width=400,format=webp/<edge_url>
// Fastly     -> https://cdn/<key>?width=400&format=webp
// CloudFront -> the configured image-handler path
```

`ImageTransform` is a provider-neutral spec (`width`, `height`, `fit`, `quality`, `format`); each adapter's `transform()` lowers it to that provider's URL grammar, so the app author writes one call and swapping `CloudflareCdn` for `FastlyCdn` changes only the plugin wiring, not the templates. This is the same "one spec, N adapters" shape as the `CdnProvider` trait itself, and the same relationship the eager `thumbnails` pipeline has to the lazy edge path: an app picks eager variants (deterministic keys, no edge dependency) OR edge transforms (no pre-generation, CDN does the work), or both.

## What Part 2 deliberately does not do

- It does not put a CDN in `umbral-core`. `CdnProvider` and the adapters live in `umbral-storage`; core's `Storage::url` stays the sync public-URL contract, and the CDN wrapper is a plugin-level decorator over it.
- It does not replace `public_base`. `public_base` stays the zero-adapter "public bucket behind a dumb CDN" path; `.cdn(...)` is the upgrade that adds purge, edge signing, and transforms on top.
- It does not invent a cache. The edge cache is the provider's; umbral emits the policy headers and the purge calls, and owns neither the storage of cached bytes nor the CDN account.
- It does not couple to the webhook plugin for purge. Purge is a direct provider API call on the lifecycle hook; it does not need the outbox relay (a stale edge object is a soft, retry-on-next-write condition, not a durable at-least-once delivery). The webhook coupling in Part 1 (the S3 event -> finalize) is about a durable state transition; purge is not.

---

## Summary of the contract

- #58: add optional, additive presign methods to the `Storage` trait (`presign_put`, `presign_post`, `create_multipart` / `presign_part` / `complete_multipart` / `abort_multipart`), each defaulting to `StorageError::Unsupported` and overridden only by the S3 backend (wrapping rust-s3's presign/multipart calls with the same reactor-safe `spawn_blocking` discipline `presign_get` already uses). `StoragePlugin::direct_uploads(...)` mounts a `sign` / `complete` route pair: `sign` authorizes the caller (the `media_access_identity` seam), mints a key, presigns with the plugin's `.max_size()` / `.accept()` caps baked into the signature, and reserves a `status="processing"` `media_file` row; `complete` HEAD-verifies the landed object and finalizes the row (running the existing `Processor` pipeline). An S3 event-notification route (`notify`), verified with the shared webhook HMAC scheme, finalizes uploads whose client never called back, and a reconcile command sweeps the rest. A dependency-free JS client helper wraps the browser dance. The through-the-app `save*` path and the gaps4 #58 gated proxy stay as the always-available default for non-presign backends.
- #60: add a `CdnProvider` trait in `umbral-storage` with `CloudFrontCdn` / `CloudflareCdn` / `FastlyCdn` adapters (each `from_env()` + builder, each an ordinary trait impl a third party can mirror). `StoragePlugin::cdn(provider)` rewrites `url()` to the edge URL for public media and to the provider's signed URL/cookie for private media (edge-enforced, replacing the app-verified `signed_media_url` round-trip where a CDN is present), and hooks the existing file-lifecycle cleanup so a delete/replace purges the edge (best-effort, logged), with a `umbral storage purge` command and `cdn().purge()` for manual invalidation. A `CachePolicy` emits edge/browser TTL + stale-while-revalidate headers (immutable for hashed static assets, purgeable for media). `.cdn_transforms(true)` + `cdn_image(key, ImageTransform)` passes a provider-neutral transform spec through to the CDN's image grammar, the lazy complement to the eager `thumbnails` variant pipeline. `public_base` stays the zero-adapter path; core's `Storage::url` contract is unchanged.
