//! The feature-gated S3 [`StaticStorage`] backend for
//! `collectstatic --storage s3` (gaps2 #55, Django's
//! `STATICFILES_STORAGE` → S3).
//!
//! Built only under the `s3` cargo feature. Uploads each collected asset
//! to a bucket via `rust-s3`'s blocking `put_object`, so the synchronous
//! [`StaticStorage::put`] needs no async runtime. S3-compatible stores
//! (MinIO, Cloudflare R2, DigitalOcean Spaces) work through the same path
//! by pointing `UMBRA_STATIC_ENDPOINT` at a custom endpoint.
//!
//! ## Configuration (env / settings)
//!
//! | Var | Meaning | Required |
//! |---|---|---|
//! | `UMBRA_STATIC_BUCKET` | bucket name | yes |
//! | `UMBRA_STATIC_REGION` | region (`us-east-1`) | yes (unless endpoint) |
//! | `UMBRA_STATIC_ENDPOINT` | custom endpoint (MinIO/R2) | no |
//! | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | credentials | from env chain |
//! | `UMBRA_STATIC_PREFIX` | key prefix under the bucket | no |
//!
//! Credentials come from `rust-s3`'s standard chain (env vars, then the
//! shared credentials file), so the framework never handles secrets
//! directly.

use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use umbra::static_files::{StaticError, StaticStorage};

/// An S3 (or S3-compatible) destination for collected static assets.
///
/// Construct via [`S3Storage::from_env`]; the `collectstatic` command
/// builds one when `--storage s3` is selected. Keys are built by
/// [`Self::object_key`] (the `<prefix>/<rel_path>` join), which is unit
/// tested without a live bucket.
pub struct S3Storage {
    bucket: Box<Bucket>,
    /// Optional key prefix (`static/`) prepended to every object key.
    /// Normalised to have no leading slash and exactly one trailing slash
    /// (or empty).
    prefix: String,
}

impl S3Storage {
    /// Build an `S3Storage` from `UMBRA_STATIC_*` env vars (and the
    /// standard AWS credential chain). Returns a descriptive error string
    /// when the bucket name is missing or the bucket handle can't be
    /// constructed.
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

        // Credentials from the standard chain (env, profile, IMDS).
        let credentials =
            Credentials::default().map_err(|e| format!("could not resolve AWS credentials: {e}"))?;

        let bucket = Bucket::new(&bucket_name, region, credentials)
            .map_err(|e| format!("could not open bucket `{bucket_name}`: {e}"))?;

        let prefix = normalise_prefix(
            &std::env::var("UMBRA_STATIC_PREFIX").unwrap_or_default(),
        );

        Ok(Self { bucket, prefix })
    }

    /// The S3 object key for a logical `rel_path`: the configured prefix
    /// joined to the forward-slash logical path. Kept separate from the
    /// upload so it can be unit tested without a live bucket.
    ///
    /// `prefix = "static/"`, `rel_path = "css/app.css"` →
    /// `"static/css/app.css"`. An empty prefix yields the bare
    /// `rel_path`. The leading slash on `rel_path` (if any) is trimmed so
    /// the key never doubles a separator.
    pub fn object_key(prefix: &str, rel_path: &str) -> String {
        format!("{prefix}{}", rel_path.trim_start_matches('/'))
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

impl StaticStorage for S3Storage {
    fn put(&self, rel_path: &str, bytes: &[u8]) -> Result<(), StaticError> {
        let key = Self::object_key(&self.prefix, rel_path);
        let content_type = mime_guess::from_path(rel_path)
            .first_or_octet_stream()
            .to_string();
        self.bucket
            .put_object_with_content_type_blocking(&key, bytes, &content_type)
            .map_err(|e| StaticError::Backend(format!("put_object `{key}` failed: {e}")))?;
        Ok(())
    }

    fn exists(&self, rel_path: &str) -> Result<bool, StaticError> {
        let key = Self::object_key(&self.prefix, rel_path);
        match self.bucket.head_object_blocking(&key) {
            Ok((_head, 200)) => Ok(true),
            Ok((_head, 404)) => Ok(false),
            // Any other status: treat 2xx/3xx as present, 4xx as absent,
            // anything else as a backend error.
            Ok((_head, status)) if (200..400).contains(&status) => Ok(true),
            Ok((_head, status)) if (400..500).contains(&status) => Ok(false),
            Ok((_head, status)) => Err(StaticError::Backend(format!(
                "head_object `{key}` returned status {status}"
            ))),
            Err(e) => Err(StaticError::Backend(format!(
                "head_object `{key}` failed: {e}"
            ))),
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
        // A leading slash on the logical path must not double the
        // separator against an empty prefix, nor against a real one.
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
