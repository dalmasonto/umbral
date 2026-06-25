# Outline — Static and media

| | |
|---|---|
| **Status** | Outline. Promotes alongside `web-layer.md` or when admin needs file fields. |
| **Maps to milestone** | M11 |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `04-orm-model-and-fields.md`, outlines `web-layer.md`, `admin.md`, `forms.md`, `tasks.md` |

## Purpose

`umbral::storage` owns two related-but-distinct concerns the framework lumps under one spec because they share a backend abstraction. **Static** assets are developer-shipped files (CSS, JS, images, fonts) that travel with the codebase, get collected into a single root at deploy time, and are served by nginx or a CDN in production. **Media** files are user-uploaded blobs whose paths live in database columns (`FileField`, `ImageField`) and whose bytes live in a pluggable storage backend (`FilesystemStorage` default, `S3Storage` later as a separate crate). Both ride on the same `Storage` trait, but their lifecycles diverge: static is build-time and read-only, media is request-time and mutable. The outline pins down `collectstatic`, the dev-time `/static/` mount, the field types' on-disk semantics, and the ambient `umbral::storage::default()` handle that hides the backend from calling code.

## Key concepts

### Static files

A plugin ships a `static/` directory inside its crate (the layout convention from `02-plugin-contract.md`). `umbral-cli collectstatic` walks every registered plugin, copies (or symlinks under `--link`) each `static/` tree into a single `STATIC_ROOT` directory configured in `Settings`, and namespaces by plugin name to avoid collisions (`STATIC_ROOT/blog/main.css`, `STATIC_ROOT/admin/admin.js`). At dev time, `umbral-cli runserver` mounts `STATIC_ROOT` (or, in `Environment::Dev`, the per-plugin `static/` dirs directly so a CSS edit shows up without re-running `collectstatic`) at `/static/` via a tower-http `ServeDir` layer. In production, the framework does *not* serve static — nginx or a CDN does, with `STATIC_ROOT` as its docroot. The framework's job ends at producing the directory.

### Media files

`FileField` is a `String` column under the hood (per `04-orm-model-and-fields.md`'s field-types table): the database stores a path string interpreted by the active storage backend; the bytes live in the backend. `ImageField` is `FileField` plus extracted metadata (width, height, and optional content type) cached either on the row or in sibling columns. Uploads come in through the web layer's `Multipart` extractor (`web-layer.md`); the storage write is a separate step the handler invokes explicitly:

```rust
let path = umbral::storage::default().write(&upload).await?;
post.cover = path;            // FileField stores the storage path
post.save().await?;
```

### Storage backends

The `Storage` trait is the dynamic seam — `Box<dyn Storage>` is the type held by `umbral::storage`'s `OnceLock`. The trait is intentionally narrow:

```rust
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    async fn read(&self, path: &str) -> Result<Bytes>;
    async fn write(&self, path: &str, bytes: Bytes) -> Result<String>;
    async fn delete(&self, path: &str) -> Result<()>;
    fn url(&self, path: &str) -> String;
}
```

`FilesystemStorage` (the default) writes under a `MEDIA_ROOT` and returns `/media/{path}` URLs the dev server mounts under a `ServeDir` similar to static. `S3Storage` (a separate crate, post-M11) implements the same trait against an S3-compatible bucket and returns either public or presigned URLs. Either way, `FileField` sees a path string and `url()` is the only place the backend's URL convention leaks.

### The ambient handle

`umbral::storage::default()` reads the `Box<dyn Storage>` from a per-module `OnceLock`, set in `App::builder()` via the pattern from `01-app-and-settings.md`. Multi-backend setups (a public bucket for user uploads, a private one for invoices) register additional backends under aliases — `umbral::storage::backend("private")` — mirroring the multi-database story so a `FileField` can declare `#[umbral(storage = "private")]` when it needs a non-default backend.

## Promote-to-deep trigger

Promote alongside `web-layer.md` (which owns multipart parsing and dev-time `ServeDir` mounting) or when `admin.md` needs to render and accept `FileField` / `ImageField` form widgets — whichever fires first.

## Open questions

- **URL generation: signed URLs vs always-public paths.** `FilesystemStorage` returns `/media/{path}`; `S3Storage` could return either a public URL or a time-limited presigned URL. Whether `Storage::url()` takes a `&UrlOptions` (TTL, response headers) or stays a plain `&str -> String` is the API shape question; the deep spec resolves it once a private-bucket use case is concrete.
- **File naming strategy.** Three candidates: keep the original filename (collision-prone), generate a UUID v7 prefix (sortable, unique, opaque), or content-hash the bytes (deduplicates, breaks rename). The default is likely UUID v7 with `#[umbral(upload_to = "...")]` controlling the directory prefix, but the matrix isn't decided.
- **`ImageField` metadata extraction: synchronous on upload vs background task.** Probing dimensions on the upload request is simple but blocks the handler on a slow decoder for large images. Enqueueing an `umbral-tasks` job (`tasks.md`) keeps the request fast but means `width` / `height` are momentarily NULL. The deep spec picks per-field via an opt-in attribute.
- **Dev-vs-prod serving boundaries.** `runserver` mounts `/static/` and `/media/` in dev; production serves both via nginx. The line between "framework convenience" and "production misconfiguration" needs a system check that warns if `Environment::Prod` is paired with the dev-style mount being active.
- **`collectstatic` manifest / fingerprinting.** A manifest storage backend that hashes filenames for cache busting is a common need. Whether umbral ships this in v1 or defers to the CDN's own cache-busting story is open; the answer depends on how many users deploy without a CDN.

## Cross-links

- Deep specs that constrain this: `04-orm-model-and-fields.md` (`FileField`, `ImageField` field types and their `FieldSpec` entries), `01-app-and-settings.md` (ambient `OnceLock` handles, `STATIC_ROOT` / `MEDIA_ROOT` settings, multi-backend registration shape), `02-plugin-contract.md` (every plugin's `static/` directory convention; `collectstatic` walks the registered plugin set).
- Sibling outlines: `web-layer.md` (multipart parsing, dev-time `ServeDir` mounts for `/static/` and `/media/`), `admin.md` (`FileField` and `ImageField` form widget rendering and upload handling), `forms.md` (`FileInput` widget and validation of upload size / content type), `tasks.md` (the async pattern for `ImageField` metadata extraction and any post-upload processing like thumbnailing).
