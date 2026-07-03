//! The feature-gated, unified S3 [`Storage`] backend.
//!
//! ONE impl of the core async [`Storage`] trait that serves BOTH the media
//! (`"default"`) and static (`"staticfiles"`) instances. Adapted from
//! umbral-static's `StaticStorage`-shaped S3 backend, lifted onto the
//! unified `Storage` trait: the existing `rust-s3` blocking calls (the
//! same crate + version, `s3` 0.35) are wrapped in `spawn_blocking` so the
//! async trait methods never block the runtime. S3-compatible stores
//! (MinIO, Cloudflare R2, DigitalOcean Spaces) work through the same path
//! via a custom `UMBRAL_STATIC_ENDPOINT`.
//!
//! Built only under the `s3` cargo feature.
//!
//! ## Configuration (env)
//!
//! | Var | Meaning | Required |
//! |---|---|---|
//! | `UMBRAL_S3_BUCKET` | bucket name | yes |
//! | `UMBRAL_S3_REGION` | region (`us-east-1`) | yes (unless endpoint) |
//! | `UMBRAL_S3_ENDPOINT` | custom endpoint (MinIO/R2/Spaces) | no |
//! | `UMBRAL_S3_ACCESS_KEY` / `UMBRAL_S3_SECRET_KEY` | explicit credentials | no |
//! | `UMBRAL_S3_SESSION_TOKEN` | STS session token | no |
//! | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | credentials (fallback chain) | no |
//! | `UMBRAL_S3_PREFIX` | key prefix under the bucket | no |
//! | `UMBRAL_S3_PUBLIC_BASE` | public URL base for `url()` | no |
//! | `UMBRAL_S3_PATH_STYLE` | truthy → path-style addressing (MinIO) | no |
//! | `UMBRAL_S3_PRESIGN_TTL` | presign URLs (seconds) for private buckets | no |
//!
//! The `UMBRAL_S3_*` names work for ANY S3-compatible provider (AWS, MinIO,
//! Cloudflare R2, Backblaze B2, DigitalOcean Spaces). The legacy
//! `UMBRAL_STATIC_BUCKET` / `_REGION` / `_ENDPOINT` / `_PREFIX` / `_PUBLIC_BASE`
//! names are still accepted as a deprecated fallback (a one-time warning is
//! logged when only the old name is present). The static *pipeline* settings
//! `UMBRAL_STATIC_URL` / `UMBRAL_STATIC_ROOT` are a separate concern and are
//! NOT affected by this rename.

use std::sync::Arc;

use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use umbral::storage::{ByteStream, Storage, StorageError, StoredFile};

/// An S3 (or S3-compatible) [`Storage`] backend. Construct via
/// [`S3Storage::from_env`] or [`S3Storage::builder`].
///
/// Implements the full [`Storage`] surface: `store` (key-generating) /
/// `put` (exact-key) → `put_object`; `retrieve` / `retrieve_stream` →
/// `get_object`; `delete` → `delete_object`; `exists` → `head_object`;
/// `url` → the public base join, or the bare `<prefix><key>` path when no
/// base is set. Keys are built by [`Self::object_key`] (the
/// `<prefix>/<key>` join), unit-tested without a live bucket.
pub struct S3Storage {
    bucket: Arc<Bucket>,
    /// Optional key prefix (`static/`) prepended to every object key.
    prefix: String,
    /// Optional absolute public base (scheme + host, no trailing slash)
    /// used by [`Storage::url`]. `None` → the bare key path.
    public_base: Option<String>,
    /// When `Some(ttl)`, [`Storage::url`] returns a presigned GET URL valid
    /// for `ttl` seconds instead of a public/base URL.
    ///
    /// Precedence: `presign_ttl` (signed, time-limited — for **private**
    /// buckets) takes precedence over `public_base` (public/CDN). Presigning
    /// requires real credentials to produce a URL the provider will accept;
    /// with dummy creds the URL is syntactically valid but won't authorise.
    presign_ttl: Option<u32>,
}

impl S3Storage {
    /// Start a builder. `bucket` is required; `endpoint` / `region` /
    /// `prefix` / `public_base` refine it.
    pub fn builder(bucket: impl Into<String>) -> S3StorageBuilder {
        S3StorageBuilder {
            bucket: bucket.into(),
            region: None,
            endpoint: None,
            prefix: String::new(),
            public_base: None,
            credentials: None,
            path_style: false,
            presign_ttl: None,
        }
    }

    /// Build an `S3Storage` from `UMBRAL_S3_*` env vars (with `UMBRAL_STATIC_*`
    /// back-compat). Credentials come from explicit `UMBRAL_S3_ACCESS_KEY` /
    /// `UMBRAL_S3_SECRET_KEY` (+ optional `UMBRAL_S3_SESSION_TOKEN`) if both are
    /// set, otherwise the standard AWS credential chain. Returns a descriptive
    /// error string when the bucket name is missing or the bucket handle can't
    /// be built.
    pub fn from_env() -> Result<Self, String> {
        let bucket_name = env_s3("UMBRAL_S3_BUCKET", "UMBRAL_STATIC_BUCKET")
            .ok_or_else(|| "UMBRAL_S3_BUCKET is required for the s3 storage backend".to_string())?;

        let endpoint = env_s3("UMBRAL_S3_ENDPOINT", "UMBRAL_STATIC_ENDPOINT");
        let region_var = env_s3("UMBRAL_S3_REGION", "UMBRAL_STATIC_REGION");
        let region = match endpoint {
            Some(endpoint) if !endpoint.is_empty() => Region::Custom {
                region: region_var.unwrap_or_else(|| "us-east-1".into()),
                endpoint,
            },
            _ => region_var
                .ok_or_else(|| {
                    "UMBRAL_S3_REGION (or UMBRAL_S3_ENDPOINT) is required for the s3 storage backend"
                        .to_string()
                })?
                .parse::<Region>()
                .map_err(|e| format!("invalid UMBRAL_S3_REGION: {e}"))?,
        };

        // Explicit creds let a user point storage at one provider's keys
        // WITHOUT colliding with `AWS_*` used elsewhere; else the AWS chain.
        let credentials = match (
            std::env::var("UMBRAL_S3_ACCESS_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            std::env::var("UMBRAL_S3_SECRET_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
        ) {
            (Some(access), Some(secret)) => {
                let token = std::env::var("UMBRAL_S3_SESSION_TOKEN")
                    .ok()
                    .filter(|s| !s.is_empty());
                Credentials::new(Some(&access), Some(&secret), token.as_deref(), None, None)
                    .map_err(|e| format!("invalid UMBRAL_S3_ACCESS_KEY/SECRET_KEY: {e}"))?
            }
            _ => Credentials::default()
                .map_err(|e| format!("could not resolve AWS credentials: {e}"))?,
        };

        let mut bucket = Bucket::new(&bucket_name, region, credentials)
            .map_err(|e| format!("could not open bucket `{bucket_name}`: {e}"))?;
        if truthy(std::env::var("UMBRAL_S3_PATH_STYLE").ok().as_deref()) {
            bucket.set_path_style();
        }

        let prefix = normalise_prefix(
            &env_s3("UMBRAL_S3_PREFIX", "UMBRAL_STATIC_PREFIX").unwrap_or_default(),
        );
        let public_base = env_s3("UMBRAL_S3_PUBLIC_BASE", "UMBRAL_STATIC_PUBLIC_BASE")
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string());
        let presign_ttl = std::env::var("UMBRAL_S3_PRESIGN_TTL")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok());

        Ok(Self {
            bucket: Arc::from(bucket),
            prefix,
            public_base,
            presign_ttl,
        })
    }

    /// The S3 object key for a logical `rel_path`: the configured prefix
    /// joined to the forward-slash logical path. Kept separate from the
    /// upload so it can be unit tested without a live bucket.
    pub fn object_key(prefix: &str, rel_path: &str) -> String {
        format!("{prefix}{}", rel_path.trim_start_matches('/'))
    }

    /// Upload `bytes` under the EXACT logical `key`, recording
    /// `content_type`. Runs the blocking `rust-s3` call off-runtime.
    async fn put_object(
        &self,
        key: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<(), StorageError> {
        let object_key = Self::object_key(&self.prefix, key);
        let bucket = self.bucket.clone();
        let content_type = content_type.to_string();
        tokio::task::spawn_blocking(move || {
            bucket
                .put_object_with_content_type_blocking(&object_key, &bytes, &content_type)
                .map(|_| ())
                .map_err(|e| {
                    StorageError::Backend(format!("put_object `{object_key}` failed: {e}"))
                })
        })
        .await
        .map_err(|e| StorageError::Backend(format!("s3 put join error: {e}")))?
    }
}

/// Builder for [`S3Storage`] — bucket + optional region / endpoint /
/// prefix / public base / explicit credentials / path-style / presign TTL.
/// Without explicit `.credentials(...)`, credentials come from the standard
/// AWS chain.
pub struct S3StorageBuilder {
    bucket: String,
    region: Option<String>,
    endpoint: Option<String>,
    prefix: String,
    public_base: Option<String>,
    credentials: Option<Credentials>,
    path_style: bool,
    presign_ttl: Option<u32>,
}

impl S3StorageBuilder {
    /// Set the region (`us-east-1`). Required unless an endpoint is set.
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Set a custom endpoint for an S3-compatible store (MinIO, R2,
    /// Spaces). When set, the region defaults to `us-east-1`.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Set a key prefix prepended to every object key.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = normalise_prefix(&prefix.into());
        self
    }

    /// Set the public URL base used by [`Storage::url`] (scheme + host).
    pub fn public_base(mut self, base: impl Into<String>) -> Self {
        self.public_base = Some(base.into().trim_end_matches('/').to_string());
        self
    }

    /// Set explicit access/secret (+ optional session `token`) credentials,
    /// bypassing the AWS chain. Useful to point storage at one provider's keys
    /// without colliding with `AWS_*` used elsewhere.
    pub fn credentials(
        mut self,
        access: impl AsRef<str>,
        secret: impl AsRef<str>,
        token: Option<&str>,
    ) -> Self {
        self.credentials = Credentials::new(
            Some(access.as_ref()),
            Some(secret.as_ref()),
            token,
            None,
            None,
        )
        .ok();
        self
    }

    /// Use path-style addressing (`endpoint/bucket/key`) instead of the
    /// virtual-hosted default. Required by MinIO and many self-hosted stores.
    pub fn path_style(mut self, on: bool) -> Self {
        self.path_style = on;
        self
    }

    /// Serve objects via presigned GET URLs valid for `ttl_secs` seconds.
    /// Takes precedence over `public_base` in [`Storage::url`]; this is how
    /// you serve private media without a public bucket.
    pub fn presign(mut self, ttl_secs: u32) -> Self {
        self.presign_ttl = Some(ttl_secs);
        self
    }

    /// Build the backend. Uses explicit [`Self::credentials`] if set,
    /// otherwise the standard AWS chain.
    pub fn build(self) -> Result<S3Storage, String> {
        let region = match self.endpoint {
            Some(endpoint) if !endpoint.is_empty() => Region::Custom {
                region: self.region.unwrap_or_else(|| "us-east-1".into()),
                endpoint,
            },
            _ => self
                .region
                .ok_or_else(|| "region (or endpoint) is required for the s3 backend".to_string())?
                .parse::<Region>()
                .map_err(|e| format!("invalid region: {e}"))?,
        };

        let credentials = match self.credentials {
            Some(c) => c,
            None => Credentials::default()
                .map_err(|e| format!("could not resolve AWS credentials: {e}"))?,
        };
        let mut bucket = Bucket::new(&self.bucket, region, credentials)
            .map_err(|e| format!("could not open bucket `{}`: {e}", self.bucket))?;
        if self.path_style {
            bucket.set_path_style();
        }

        Ok(S3Storage {
            bucket: Arc::from(bucket),
            prefix: self.prefix,
            public_base: self.public_base,
            presign_ttl: self.presign_ttl,
        })
    }
}

/// Read an S3 config value, preferring the new `UMBRAL_S3_*` `new` name and
/// falling back to the legacy `UMBRAL_STATIC_*` `old` name. When only the old
/// name is set, log a one-time deprecation warning naming both.
fn env_s3(new: &str, old: &str) -> Option<String> {
    let new_val = std::env::var(new).ok();
    let old_val = std::env::var(old).ok();
    // Warn once when the value is coming from the legacy name only.
    let new_set = new_val.as_deref().is_some_and(|s| !s.is_empty());
    let old_set = old_val.as_deref().is_some_and(|s| !s.is_empty());
    if !new_set && old_set {
        warn_deprecated(old, new);
    }
    resolve(new_val, old_val)
}

/// Pure new-then-old resolution used by [`env_s3`]; unit-tested without
/// touching the process environment.
fn resolve(new: Option<String>, old: Option<String>) -> Option<String> {
    new.filter(|s| !s.is_empty())
        .or_else(|| old.filter(|s| !s.is_empty()))
}

/// Emit a deprecation warning the first time a legacy `UMBRAL_STATIC_*` name is
/// used in place of the new `UMBRAL_S3_*` name. Deduplicated per old-name so a
/// process that reads it from several call sites warns only once.
fn warn_deprecated(old: &str, new: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap();
    if guard.insert(old.to_string()) {
        tracing::warn!(
            "umbral-storage: `{old}` is deprecated; use `{new}` instead (the old name still works \
             for now)."
        );
    }
}

/// Truthy parse for boolean env flags: `1`/`true`/`yes`/`on` (any case).
fn truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Normalise a key prefix: drop a leading slash, ensure exactly one
/// trailing slash, and collapse an empty/whitespace prefix to `""`.
fn normalise_prefix(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('/').trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

#[umbral::storage::async_trait]
impl Storage for S3Storage {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        // The SAME stored-XSS guard the filesystem backend applies (audit
        // `plugin-storage-tasks` #1): sanitise the filename, defang
        // active-content extensions (`evil.html` → `evil.html.txt`), and
        // force the recorded Content-Type to `text/plain` when defanged.
        // Without this, a public/CDN-fronted bucket would serve an uploaded
        // `evil.html` inline as `text/html` — stored XSS on the serving
        // origin. (`store_stream` uses the trait's buffering default, which
        // delegates here, so the streaming path is covered too.)
        let (safe_name, content_type) = crate::media::neutralised_upload(filename, content_type);
        let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
        self.put_object(&key, &content_type, bytes.to_vec()).await?;
        Ok(StoredFile {
            url: self.url(&key),
            key,
            size: bytes.len() as u64,
        })
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let object_key = Self::object_key(&self.prefix, key);
        let bucket = self.bucket.clone();
        let object_key2 = object_key.clone();
        let resp = tokio::task::spawn_blocking(move || bucket.get_object_blocking(&object_key2))
            .await
            .map_err(|e| StorageError::Backend(format!("s3 get join error: {e}")))?;
        match resp {
            Ok(data) if data.status_code() == 404 => Err(StorageError::NotFound),
            Ok(data) if (200..300).contains(&data.status_code()) => Ok(data.bytes().to_vec()),
            Ok(data) => Err(StorageError::Backend(format!(
                "get_object `{object_key}` returned status {}",
                data.status_code()
            ))),
            Err(e) => Err(StorageError::Backend(format!(
                "get_object `{object_key}` failed: {e}"
            ))),
        }
    }

    async fn put(
        &self,
        key: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.put_object(key, content_type, bytes.to_vec()).await?;
        Ok(StoredFile {
            url: self.url(key),
            key: key.to_string(),
            size: bytes.len() as u64,
        })
    }

    /// Streaming exact-key put. Buffers the stream (S3's single-PUT path),
    /// then delegates to [`put`](Storage::put). A multipart streaming
    /// upload is a deferred refinement; this keeps a working `put_stream`.
    async fn put_stream(
        &self,
        key: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut body = body;
        while let Some(chunk) = futures_util::StreamExt::next(&mut body).await {
            let chunk = chunk.map_err(StorageError::Io)?;
            bytes.extend_from_slice(&chunk);
        }
        self.put(key, content_type, &bytes).await
    }

    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        let object_key = Self::object_key(&self.prefix, key);
        let bucket = self.bucket.clone();
        let object_key2 = object_key.clone();
        let resp = tokio::task::spawn_blocking(move || bucket.head_object_blocking(&object_key2))
            .await
            .map_err(|e| StorageError::Backend(format!("s3 head join error: {e}")))?;
        match resp {
            Ok((_head, 200)) => Ok(true),
            Ok((_head, 404)) => Ok(false),
            Ok((_head, status)) if (200..400).contains(&status) => Ok(true),
            Ok((_head, status)) if (400..500).contains(&status) => Ok(false),
            Ok((_head, status)) => Err(StorageError::Backend(format!(
                "head_object `{object_key}` returned status {status}"
            ))),
            Err(e) => Err(StorageError::Backend(format!(
                "head_object `{object_key}` failed: {e}"
            ))),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let object_key = Self::object_key(&self.prefix, key);
        let bucket = self.bucket.clone();
        let object_key2 = object_key.clone();
        let resp = tokio::task::spawn_blocking(move || bucket.delete_object_blocking(&object_key2))
            .await
            .map_err(|e| StorageError::Backend(format!("s3 delete join error: {e}")))?;
        match resp {
            Ok(data) if data.status_code() == 404 => Err(StorageError::NotFound),
            Ok(data) if (200..300).contains(&data.status_code()) => Ok(()),
            Ok(data) => Err(StorageError::Backend(format!(
                "delete_object `{object_key}` returned status {}",
                data.status_code()
            ))),
            Err(e) => Err(StorageError::Backend(format!(
                "delete_object `{object_key}` failed: {e}"
            ))),
        }
    }

    /// Resolve a browser-facing URL for `key`.
    ///
    /// Precedence: when `presign_ttl` is set, return a presigned GET URL
    /// (signed + time-limited — the way to serve **private** buckets); on a
    /// presign error, log and fall back to the public/base URL rather than
    /// panic. Otherwise join `public_base` (public/CDN), or return the bare
    /// `<prefix><key>` path when no base is set.
    fn url(&self, key: &str) -> String {
        let object_key = Self::object_key(&self.prefix, key);
        let public = || match &self.public_base {
            Some(base) => format!("{base}/{object_key}"),
            None => object_key.clone(),
        };
        match self.presign_ttl {
            // `url()` is sync but is called from inside async request handlers
            // (a template resolving a FileField's presigned URL). rust-s3's
            // `presign_get_blocking` does `Runtime::new().block_on`, which
            // PANICS ("Cannot start a runtime from within a runtime") on a
            // tokio worker thread. Presigning is pure local HMAC (no I/O), so
            // we drive the async `presign_get` with `futures_executor::block_on`
            // — it polls the already-ready future to completion without
            // spinning up a tokio runtime, so it's safe in any context.
            Some(ttl) => {
                match futures_executor::block_on(self.bucket.presign_get(&object_key, ttl, None)) {
                    Ok(url) => url,
                    Err(e) => {
                        // For a PRIVATE bucket the public-URL fallback will
                        // not authorize — the returned link 404s/403s rather
                        // than leaking anything. `url()` is infallible by
                        // trait contract, so a loud warning + inert fallback
                        // is the best available behaviour here.
                        tracing::warn!(
                            key = %object_key,
                            "umbral-storage: presign failed: {e} — falling back to the public \
                             URL, which will NOT authorize on a private bucket"
                        );
                        public()
                    }
                }
            }
            None => public(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_joins_prefix_and_path() {
        assert_eq!(
            S3Storage::object_key("static/", "css/app.css"),
            "static/css/app.css"
        );
    }

    #[test]
    fn object_key_with_empty_prefix_is_the_bare_path() {
        assert_eq!(S3Storage::object_key("", "js/app.js"), "js/app.js");
    }

    #[test]
    fn object_key_trims_leading_slash_on_rel_path() {
        assert_eq!(S3Storage::object_key("", "/css/app.css"), "css/app.css");
        assert_eq!(
            S3Storage::object_key("assets/", "/css/app.css"),
            "assets/css/app.css"
        );
    }

    #[test]
    fn normalise_prefix_forces_single_trailing_slash() {
        assert_eq!(normalise_prefix(""), "");
        assert_eq!(normalise_prefix("  "), "");
        assert_eq!(normalise_prefix("static"), "static/");
        assert_eq!(normalise_prefix("/static/"), "static/");
        assert_eq!(normalise_prefix("a/b"), "a/b/");
    }

    #[test]
    fn resolve_prefers_new_name_then_falls_back_to_old() {
        // New name (UMBRAL_S3_*) wins over the legacy old name.
        assert_eq!(
            resolve(Some("new-bucket".into()), Some("old-bucket".into())),
            Some("new-bucket".into())
        );
        // Old name (UMBRAL_STATIC_*) still works when the new one is absent.
        assert_eq!(
            resolve(None, Some("old-bucket".into())),
            Some("old-bucket".into())
        );
        // An empty new value is treated as unset and falls back to old.
        assert_eq!(
            resolve(Some(String::new()), Some("old-bucket".into())),
            Some("old-bucket".into())
        );
        // Neither set → None.
        assert_eq!(resolve(None, None), None);
    }

    #[test]
    fn truthy_recognises_common_true_strings() {
        for v in ["1", "true", "TRUE", "Yes", "on", "  on  "] {
            assert!(truthy(Some(v)), "{v:?} should be truthy");
        }
        for v in ["0", "false", "no", "off", ""] {
            assert!(!truthy(Some(v)), "{v:?} should be falsy");
        }
        assert!(!truthy(None));
    }

    #[test]
    fn presign_produces_a_signed_url_without_a_live_bucket() {
        // Presigning is pure local HMAC — no network needed. Dummy creds make
        // the signature, a region/endpoint make the host. The resulting URL
        // must carry the SigV4 query params even though the bucket is fake.
        let s3 = S3Storage::builder("private-bucket")
            .region("us-east-1")
            .credentials("AKIAEXAMPLE", "secretexamplekey", None)
            .presign(900)
            .build()
            .expect("builder with dummy creds + presign should build");

        let url = s3.url("media/photo.png");
        assert!(
            url.contains("X-Amz-Signature"),
            "presigned url must carry X-Amz-Signature, got: {url}"
        );
        assert!(
            url.contains("X-Amz-Expires"),
            "presigned url must carry X-Amz-Expires, got: {url}"
        );
        assert!(
            url.contains("photo.png"),
            "presigned url must reference the object key, got: {url}"
        );
    }

    #[tokio::test]
    async fn presign_url_does_not_panic_inside_a_tokio_runtime() {
        // `url()` is called from inside async request handlers (rendering a
        // template that resolves a FileField's presigned URL). The presign
        // path must NOT spin up a nested tokio runtime — rust-s3's
        // `*_blocking` does `Runtime::new().block_on`, which panics with
        // "Cannot start a runtime from within a runtime" inside an existing
        // one. Driving the async `presign_get` with `futures::executor::
        // block_on` (pure HMAC, no I/O) is runtime-safe.
        let s3 = S3Storage::builder("private-bucket")
            .region("us-east-1")
            .credentials("AKIAEXAMPLE", "secretexamplekey", None)
            .presign(900)
            .build()
            .expect("builder with dummy creds + presign should build");

        let url = s3.url("media/photo.png");
        assert!(
            url.contains("X-Amz-Signature"),
            "presigned url must sign even inside a runtime, got: {url}"
        );
    }

    #[test]
    fn url_without_presign_uses_public_base() {
        let s3 = S3Storage::builder("public-bucket")
            .region("us-east-1")
            .credentials("AKIAEXAMPLE", "secretexamplekey", None)
            .public_base("https://cdn.example.com")
            .build()
            .expect("builder should build");
        assert_eq!(s3.url("css/app.css"), "https://cdn.example.com/css/app.css");
    }

    #[test]
    fn presign_takes_precedence_over_public_base() {
        let s3 = S3Storage::builder("private-bucket")
            .region("us-east-1")
            .credentials("AKIAEXAMPLE", "secretexamplekey", None)
            .public_base("https://cdn.example.com")
            .presign(60)
            .build()
            .expect("builder should build");
        let url = s3.url("media/photo.png");
        assert!(
            url.contains("X-Amz-Signature"),
            "presign should win over public_base, got: {url}"
        );
        assert!(
            !url.starts_with("https://cdn.example.com/"),
            "presign should not return the public_base join, got: {url}"
        );
    }
}
