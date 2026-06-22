//! The feature-gated, unified S3 [`Storage`] backend.
//!
//! ONE impl of the core async [`Storage`] trait that serves BOTH the media
//! (`"default"`) and static (`"staticfiles"`) instances. Adapted from
//! umbra-static's `StaticStorage`-shaped S3 backend, lifted onto the
//! unified `Storage` trait: the existing `rust-s3` blocking calls (the
//! same crate + version, `s3` 0.35) are wrapped in `spawn_blocking` so the
//! async trait methods never block the runtime. S3-compatible stores
//! (MinIO, Cloudflare R2, DigitalOcean Spaces) work through the same path
//! via a custom `UMBRA_STATIC_ENDPOINT`.
//!
//! Built only under the `s3` cargo feature.
//!
//! ## Configuration (env)
//!
//! | Var | Meaning | Required |
//! |---|---|---|
//! | `UMBRA_STATIC_BUCKET` | bucket name | yes |
//! | `UMBRA_STATIC_REGION` | region (`us-east-1`) | yes (unless endpoint) |
//! | `UMBRA_STATIC_ENDPOINT` | custom endpoint (MinIO/R2) | no |
//! | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | credentials | from env chain |
//! | `UMBRA_STATIC_PREFIX` | key prefix under the bucket | no |
//! | `UMBRA_STATIC_PUBLIC_BASE` | public URL base for `url()` | no |

use std::sync::Arc;

use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use umbra::storage::{ByteStream, Storage, StorageError, StoredFile};

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
        }
    }

    /// Build an `S3Storage` from `UMBRA_STATIC_*` env vars (and the
    /// standard AWS credential chain). Returns a descriptive error string
    /// when the bucket name is missing or the bucket handle can't be built.
    pub fn from_env() -> Result<Self, String> {
        let bucket_name = std::env::var("UMBRA_STATIC_BUCKET")
            .map_err(|_| "UMBRA_STATIC_BUCKET is required for the s3 storage backend".to_string())?;

        let region = match std::env::var("UMBRA_STATIC_ENDPOINT") {
            Ok(endpoint) if !endpoint.is_empty() => Region::Custom {
                region: std::env::var("UMBRA_STATIC_REGION").unwrap_or_else(|_| "us-east-1".into()),
                endpoint,
            },
            _ => std::env::var("UMBRA_STATIC_REGION")
                .map_err(|_| {
                    "UMBRA_STATIC_REGION (or UMBRA_STATIC_ENDPOINT) is required for the s3 storage \
                     backend"
                        .to_string()
                })?
                .parse::<Region>()
                .map_err(|e| format!("invalid UMBRA_STATIC_REGION: {e}"))?,
        };

        let credentials =
            Credentials::default().map_err(|e| format!("could not resolve AWS credentials: {e}"))?;

        let bucket = Bucket::new(&bucket_name, region, credentials)
            .map_err(|e| format!("could not open bucket `{bucket_name}`: {e}"))?;

        let prefix = normalise_prefix(&std::env::var("UMBRA_STATIC_PREFIX").unwrap_or_default());
        let public_base = std::env::var("UMBRA_STATIC_PUBLIC_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string());

        Ok(Self {
            bucket: Arc::from(bucket),
            prefix,
            public_base,
        })
    }

    /// The S3 object key for a logical `rel_path`: the configured prefix
    /// joined to the forward-slash logical path. Kept separate from the
    /// upload so it can be unit tested without a live bucket.
    pub fn object_key(prefix: &str, rel_path: &str) -> String {
        format!("{prefix}{}", rel_path.trim_start_matches('/'))
    }

    /// Generate a collision-resistant key from `filename` (the
    /// `store`-side path), sanitised of separators.
    fn generated_key(filename: &str) -> String {
        let safe_name: String = filename
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | '\0'))
            .take(120)
            .collect();
        format!("{}-{safe_name}", uuid::Uuid::new_v4())
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
                .map_err(|e| StorageError::Backend(format!("put_object `{object_key}` failed: {e}")))
        })
        .await
        .map_err(|e| StorageError::Backend(format!("s3 put join error: {e}")))?
    }
}

/// Builder for [`S3Storage`] — bucket + optional region / endpoint /
/// prefix / public base. Credentials come from the standard AWS chain.
pub struct S3StorageBuilder {
    bucket: String,
    region: Option<String>,
    endpoint: Option<String>,
    prefix: String,
    public_base: Option<String>,
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

    /// Build the backend, resolving credentials from the standard AWS
    /// chain.
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

        let credentials =
            Credentials::default().map_err(|e| format!("could not resolve AWS credentials: {e}"))?;
        let bucket = Bucket::new(&self.bucket, region, credentials)
            .map_err(|e| format!("could not open bucket `{}`: {e}", self.bucket))?;

        Ok(S3Storage {
            bucket: Arc::from(bucket),
            prefix: self.prefix,
            public_base: self.public_base,
        })
    }
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

#[umbra::storage::async_trait]
impl Storage for S3Storage {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let key = Self::generated_key(filename);
        self.put_object(&key, content_type, bytes.to_vec()).await?;
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

    fn url(&self, key: &str) -> String {
        let object_key = Self::object_key(&self.prefix, key);
        match &self.public_base {
            Some(base) => format!("{base}/{object_key}"),
            None => object_key,
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
}
