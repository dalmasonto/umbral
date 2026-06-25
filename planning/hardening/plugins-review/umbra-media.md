# umbral-media — Holistic Review

> Date: 2026-06-16 · Read-only audit

## Verdict

umbral-media is structurally sound for a v0 filesystem-backend plugin: the `Storage` trait is clean, the dependency boundary is correct (depends only on the `umbral` facade, no circular deps), `FileField`/`ImageField` round-trip through TEXT columns, and the serve route's XSS defences (neutralise-active-content, `nosniff`) are well-tested. The two most serious gaps are both architectural: the entire upload pipeline buffers every file body into a `Vec<u8>` in the worker before writing to disk, with no streaming at any layer (multipart parser, `FsStorage::store`, `MediaPlugin::save`), and delete-on-model-delete is completely absent — the admin's `DELETE` handler, the ORM's `delete()`, and the form update path all orphan files on disk with no hook to clean them up. The deferred features (S3, image validation, access control, virus scanning) are clearly labelled; the silent gaps are the memory-buffering and the orphan problem.

---

## Completeness

| Feature | umbral-media v0 status |
|---|---|
| `FileField` / `ImageField` ORM types | **Exists** — `crates/umbral-core/src/orm/file_field.rs`; TEXT-backed newtypes with `key()`, `url()`, `is_empty()`, `Display`; serde + sqlx impls for both SQLite and Postgres |
| DB column round-trip | **Exists** — tested in `file_field_roundtrip.rs` and `file_image_field.rs` |
| `MEDIA_URL` / `MEDIA_ROOT` equivalent | **Exists** — `mount` (URL prefix) + `dir` (FS path) on `MediaPlugin`; no settings-level `MEDIA_URL`/`MEDIA_ROOT` keys, wired manually in `App::builder()` |
| `FileSystemStorage` equivalent | **Exists** — `FsStorage` in `umbral-media/src/lib.rs` |
| `Storage` trait (ABC) | **Exists** — `umbral_core::storage::Storage`; `store`, `retrieve`, `delete`, `url` |
| `default_storage` ambient singleton | **Exists** — `OnceLock<Arc<dyn Storage>>` + `set_storage`/`storage`/`storage_opt` in `umbral_core::storage` |
| File upload handling (multipart) | **Exists** — `parse_and_store_multipart` in `umbral_core::web::multipart`; admin create/update route wired |
| Custom storage backend swap | **Exists** — `MediaPlugin::with_storage(mount, Arc<dyn Storage>)` |
| Serving files in dev (`MEDIA_URL` route) | **Exists** — `ServeDir` via `tower_http`, mounted with `nest_service`; `nosniff` header set |
| `FieldFile` proxy (`.url()`, `.save()`, `.delete()`) | **Partial** — `FileField::url()` resolves via ambient storage; no `.save()` method on the field itself; no `.delete()` method; deleting a model row does NOT delete the backing file |
| File orphan cleanup on update | **Missing** — updating a `FileField` column with a new upload stores the new file but the old key stays on disk and in the `media_file` table |
| Delete-on-model-delete | **Missing** — admin `DELETE` and ORM `delete()` drop the `media_file` row but never call `Storage::delete` on the key |
| Access control (private vs public files) | **Missing** (deferred) — all stored files are publicly readable via the mount URL; no auth gate on the serve route |
| Image validation (format / magic bytes) | **Missing** (deferred) — content_type is accepted verbatim from the client; no Pillow-equivalent |
| S3-compatible backend | **Missing** (deferred) — filesystem only |
| Signed URLs | **Missing** (deferred) |
| Virus scanning | **Missing** (deferred) |
| File streaming (large upload / download) | **Missing** — entire body buffered to `Vec<u8>` at parse time; `FsStorage::store` takes `&[u8]` |
| `ImageField` admin preview | **Partial** — `widget = "image"` column metadata exists; admin template branches on it (Wave 4 comment in view.rs) |
| Upload size cap | **Partial** — enforced in `MediaPlugin::save`; bypassed on ambient/admin path (already #73) |

---

## Findings

**already #73 — Important — `storage()` panics when MediaPlugin is unregistered** — `crates/umbral-core/src/storage.rs:186`
The `.expect(...)` panics on every request that reaches `FileField::url()` if no backend was registered. Boot system-check is the fix. (already filed)

**Upload size cap bypass (already filed in security.md Important)** — `plugins/umbral-media/src/lib.rs:426-433` vs `FsStorage::store:151-183`
Cap is enforced only on `MediaPlugin::save`; the ambient/admin multipart path bypasses it. (already filed in security.md, cross-ref: already #73)

**Filename sanitization confirmed sound (already filed in security.md FYI)** — `umbral-media/src/lib.rs:161-175`
Strips `/`, `\`, `\0`; UUIDv4 prefix; neutralise-active-content `.txt` appended; `nosniff` set. Tested. (already in security.md)

---

**[NEW] Required — No file deletion on model delete (orphan accumulation)** — `plugins/umbral-admin/src/handlers/crud.rs:664-690`, `crates/umbral-core/src/orm/queryset/mod.rs` (delete paths)

When the admin deletes a row (via `DynQuerySet::for_meta(&model).filter_eq_string(...).delete()`) or the ORM's `delete()` is called, the `media_file` tracking row is NOT touched and the backing file on disk is NOT deleted. The same is true for the REST `destroy` handler. The `FileField` / `ImageField` column on the deleted model holds a storage key that now points to a live file with no owning row. At scale this is silent disk-fill.

Fix shape: a `post_delete` signal or lifecycle hook on the `Model` trait, or a delete-collector that inspects the model's `FileField`/`ImageField` columns before hard-deleting the row, reads their current keys, and calls `storage().delete(key)` + `MediaFile::objects().filter(KEY.eq(&key)).delete()` for each. The ORM already has a `gaps #68` entry for `on_delete` ORM cascade; file cleanup is the same mechanism. Fold into `gaps #68` or open a new sub-entry.

**[NEW] Required — No orphan cleanup on file-column update** — `plugins/umbral-admin/src/handlers/crud.rs:503-556`, `plugins/umbral-media/src/lib.rs:420-457`

When a record with a `FileField` is updated with a new upload, `parse_and_store_multipart` stores the new file and emits the new key. `update_row` / `update_form` then writes the new key into the column, overwriting the old one. The old key stays on disk and in the `media_file` table — nothing reads the existing column value before writing the new one and cleans it up. Two entries accumulate per overwrite.

Fix shape: before writing the new key, read the current row's file column value, and if it differs from the incoming key, call `Storage::delete(old_key)` and delete the `media_file` tracking row. This is best done at the ORM's `update_form` / `update_values` level when a column with `widget = "file"` or `widget = "image"` is in the update map — the ORM already has the model metadata needed to identify such columns. Alternatively, the admin's `update_row` path can read the old value before overwriting.

**[NEW] Required — Entire upload pipeline is fully buffered; no streaming** — `crates/umbral-core/src/web/multipart.rs:220-251`, `plugins/umbral-media/src/lib.rs:153-183`

`parse_multipart` reads every file part into `FilePart::bytes: Vec<u8>` in worker memory before returning. `parse_and_store_multipart` passes those bytes to `Storage::store(filename, ct, bytes: &[u8])`. `FsStorage::store` takes `&[u8]` and writes with `tokio::fs::write`. So a 100 MB upload occupies 100 MB of heap for the lifetime of the request handler before a single byte touches disk. Under concurrent uploads a pool of workers can exhaust heap at a small multiple of the per-request cap.

The `Storage` trait signature (`store(…, bytes: &[u8])`) is the root constraint — it was designed for simplicity at v0. Streaming would require the trait to accept `impl Stream<Item = Bytes>` instead of `&[u8]`, which is a breaking trait change.

Fix shape: redesign `Storage::store` to accept a `bytes::Bytes` or `impl AsyncRead` before the trait stabilises. Until then the ambient-path size cap (see already #73 fix) combined with axum's body-size limit are the only guards. This is a v0 known limitation; document it in `arch.md` and file a deferred spec entry.

**[NEW] Important — `MediaPlugin::save` double-inserts when `self.storage` is itself a `MediaTracking` wrapper** — `plugins/umbral-media/src/lib.rs:420-457`, `242-284`

`MediaPlugin::save` stores through `self.storage` then does its own explicit `MediaFile::objects().save(row)` insert. This is safe when `self.storage` is a plain `FsStorage` (the `new()` path) because the `MediaTracking` wrapper is only applied to the ambient singleton in `on_ready`, not to `self.storage`. However, `MediaPlugin::with_storage(mount, storage)` takes an arbitrary `Arc<dyn Storage>` — a caller can pass in a `MediaTracking`-wrapped backend. In that case `save` gets two rows: one from the inner `MediaTracking::store` and one from `save`'s own insert.

There is no runtime guard or doc-comment warning against this. The test at `plugin_save.rs:71` exercises the `with_storage` path but passes a plain `FsStorage`, so it does not catch the double-insert.

Fix shape: either (a) document that callers must NOT pass a `MediaTracking` wrapper to `with_storage`, or (b) remove `save`'s own insert and rely solely on the `MediaTracking` decorator, making tracking consistent on all paths through the decorator alone. Option (b) requires that `with_storage` always wraps in `MediaTracking` before storing in `self.storage`, which in turn means `on_ready` must NOT wrap again — achievable but needs careful sequencing.

**[NEW] Important — `with_storage` constructor silently sets `dir` to the mount string** — `plugins/umbral-media/src/lib.rs:360-368`

```rust
pub fn with_storage(mount: impl Into<String>, storage: Arc<dyn Storage>) -> Self {
    let mount = mount.into();
    Self {
        dir: PathBuf::from(&mount),   // ← mount is a URL prefix like "/media"
        mount,
        storage,
        max_size: None,
    }
}
```

`dir` is set to `PathBuf::from("/media")` (the URL prefix), not to a real filesystem path. `routes()` builds a `ServeDir::new(&self.dir)` from it. If a custom backend user calls `.routes()`, `ServeDir` tries to serve from the path `/media` on the filesystem. On most systems that path doesn't exist, so every GET returns 404 — silently, with only a `tracing::warn!` at route-build time if the path doesn't exist (`lib.rs:470`). The doc-comment says "a non-filesystem backend typically serves its own URLs, so the `routes()` `ServeDir` is a no-op", but "no-op" is misleading: it's a 404 for every request, not a no-op. There's no way to disable the `ServeDir` altogether when a custom backend handles its own URLs.

Fix shape: add an `Option<PathBuf>` for the dir, default it to `None` in `with_storage`, and in `routes()` skip `nest_service` when dir is `None` (returning `Router::new()` for custom-backend callers). Or take a dir argument in `with_storage`.

**[NEW] Important — No access control on the serve route; private-media pattern is impossible today** — `plugins/umbral-media/src/lib.rs:469-491`

The serve route is a bare `ServeDir` with no authentication middleware. Any file stored under the media dir is publicly readable if the client knows or guesses the key. UUIDs in the key name make enumeration difficult but not impossible, and the filename segment after the UUID can hint at content. There is no way to opt in to access control short of wrapping the route yourself after plugin registration.

Fix shape: a `private()` builder method on `MediaPlugin` that, when set, replaces `ServeDir` with a custom handler that gate-checks the request against the ambient auth session before streaming the file. The mechanism is deferred (acknowledged in the module docs as "signed URLs" / "public vs auth-required"), but the deferred status is not surfaced at the user call-site — a developer who needs private uploads has no warning that the plugin doesn't support it.

File a deferred spec entry. At minimum add a boot warning if `private()` is not set on a production app.

**[NEW] Optional — `MediaTracking::store` inserts with client-declared content_type without any validation** — `plugins/umbral-media/src/lib.rs:256-262`

The `content_type` stored in the `media_file` row is whatever the client sent in the multipart `Content-Type` header. The module docs say "the user-facing handler should validate against an allow-list before calling `save`" (`lib.rs:305`), but there is no enforcement hook in the plugin itself and no example handler demonstrates the allow-list. An adversary can store `content_type = "image/png"` for a file that contains executable bytes, causing the admin's file preview to render a misleading badge.

The stored content_type is display-only today (no `Content-Type` response header is set from it — `ServeDir` derives the response header from the on-disk file extension instead), so this is a data-quality issue, not a direct XSS vector. However, a future `Content-Disposition: attachment; filename=...` download endpoint that trusts the stored content_type would inherit the wrong value.

Fix shape: add an optional `allowed_content_types: Option<HashSet<String>>` to `FsStorage`/`MediaPlugin`; reject on mismatch in `store`. Document the allow-list pattern in the media docs page.

**[NEW] Optional — Test `plugin_save.rs` uses raw `sqlx::query` for DDL, violating the ORM-only rule** — `plugins/umbral-media/tests/plugin_save.rs:42-54`

```rust
sqlx::query(
    "CREATE TABLE media_file (\
        id INTEGER PRIMARY KEY AUTOINCREMENT, ...)"
)
.execute(&pool)
.await
```

The CLAUDE.md ORM-only rule allows raw SQL for schema DDL in tests that bypass the migration engine (`ensure_tables_for_tests` is the stated allowed pattern). This is that pattern. It is the lone sanctioned exception, but it should carry the required doc-comment asserting which exception applies. It currently has no such comment.

Fix shape: add a `// DDL exception: test-only table creation, schema DDL exempt per CLAUDE.md` comment above the `sqlx::query`.

**[NEW] Nit — `FsStorage` ignores its `_content_type` parameter entirely** — `plugins/umbral-media/src/lib.rs:156-158`

```rust
async fn store(
    &self,
    filename: &str,
    _content_type: &str,   // ← underscore prefix, unused
    bytes: &[u8],
```

The content type is accepted in the `Storage` trait signature (for S3 `Content-Type` metadata) but discarded in `FsStorage`. This is intentional at v0 since `ServeDir` derives the response `Content-Type` from the extension. However the underscore-prefixed parameter name is a potential future footgun: a developer adding a download endpoint might assume `store` recorded the content type and look it up from the backend — it didn't.

Fix shape: no code change needed, but add a doc-comment on the `_content_type` parameter: `// Not stored — ServeDir derives Content-Type from the on-disk extension. An S3 backend would record it as object metadata.`

**[NEW] FYI — `MediaPlugin::save` double-insert risk is self-documented but not type-enforced** — `plugins/umbral-media/src/lib.rs:505-509`

The doc-comment states "save keeps writing through the inner `self.storage` plus its own single insert, so the two entry points each record exactly one row — no double-insert." This is correct for the standard `new()` path but structurally unverifiable for `with_storage()` (see double-insert finding above). The claim is a documentation assertion, not a type-level guarantee.

---

## Tests

### Covered

- `tests/fs_storage.rs` — `FsStorage` store/retrieve/delete round-trip; collision-free keys; delete-then-retrieve `NotFound`; URL shape (relative, absolute with `public_base`); trailing-slash normalisation; path-traversal filename sanitisation.
- `tests/serve.rs` — serve route mounts at `<mount>/<key>`; missing key → 404.
- `tests/plugin_save.rs` — `MediaPlugin::save` stores bytes + inserts exactly one tracking row; ambient `MediaTracking` path inserts exactly one tracking row; `max_size` enforced on `save`.
- `crates/umbral-core/tests/file_image_field.rs` — `FileField`/`ImageField` macro classification, widget defaults, serde wire shape, `url()` fallback without backend.
- `crates/umbral-core/tests/file_field_roundtrip.rs` — ORM `create`/`get` round-trip through SQLite TEXT column; `Option<FileField>` NULL handling.
- `crates/umbral-core/tests/file_field_storage_resolution.rs` — `url()` resolves through a registered ambient backend.
- `crates/umbral-core/tests/file_field_check_missing_storage.rs` — boot fails with `field.storage_backend` error when no plugin provides storage.
- `crates/umbral-core/tests/file_field_check_present_storage.rs` — boot succeeds with a storage-providing plugin.
- `crates/umbral-core/tests/form_file_field.rs` — file/image field form validation; `<input type="file">` render; optional image field.
- `crates/umbral-core/tests/multipart_storage_merge.rs` — `parse_and_store_multipart` stores files, flattens to pairs; empty file parts skipped.
- Inline unit tests in `lib.rs` — `neutralise_active_content` for dangerous and safe extensions.

### Missing / not covered

- **File orphan on update** — no test verifies that updating a file column doesn't leave the old key on disk.
- **File deletion on model delete** — no test confirms that deleting a model row causes `Storage::delete` on its file keys.
- **Double-insert when `with_storage` receives a `MediaTracking` wrapper** — not tested; only plain `FsStorage` is passed to `with_storage` in the test suite.
- **Access control** — no test for the serve route requiring auth (feature missing, but the gap should be documented).
- **`with_storage` dir mismatch** — no test that the serve route 404s when `dir` is the URL string `"/media"` instead of a real FS path.
- **Image validation / content-type allow-list** — no test, feature absent.
- **S3 backend** — deferred; no tests.
- **Concurrent uploads** — no test for race conditions between concurrent `save` calls; given the `OnceLock`-based pool this is low risk for the DB insert path, but worth a basic concurrent round-trip test for file uniqueness.
- **`max_size` on ambient path** — `plugin_save.rs` tests `max_size` only on `MediaPlugin::save`; the ambient `MediaTracking` path (which bypasses `save`) has no size-cap test (because there is no cap on that path — the size-cap bypass is a known gap filed in already #73).
- **serve route `nosniff` header** — `tests/serve.rs` checks status and body bytes but doesn't assert the `X-Content-Type-Options: nosniff` header is present in the response.
