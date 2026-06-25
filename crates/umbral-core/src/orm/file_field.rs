//! `FileField` / `ImageField` — TEXT-backed handles to a file stored in
//! the ambient [`Storage`](crate::storage::Storage) backend.
//!
//! ## What this is
//!
//! A `FileField` is a thin newtype around a `String` that holds the
//! storage *key* — the opaque identifier a [`Storage`] backend returns
//! from `store` (e.g. `"ab12-photo.jpg"`). The column is plain `TEXT`;
//! the value persisted is exactly the key. The framework value-add over
//! a bare `String` is [`FileField::url`]: it resolves the key to a
//! public URL through the ambient storage backend, so a template can
//! render `<img src="{{ post.cover.url }}">` without the model author
//! threading a `Storage` handle through.
//!
//! `ImageField` is a [`FileField`] in everything but its *default
//! widget*. The storage layer treats them identically; the only
//! difference is the `#[derive(Model)]` macro tags an `ImageField`
//! column with `widget = Some("image")` (vs `Some("file")` for a
//! `FileField`), which a later wave's admin uses to render an image
//! preview instead of a plain file input. `ImageField` shares all of
//! `FileField`'s behaviour by wrapping it (`ImageField(FileField)`) and
//! deref-ing to it; the sqlx + serde impls are generated once by an
//! internal macro and applied to both, so there is no duplicated
//! encode/decode logic.
//!
//! ## Serialisation
//!
//! Both serialise *as the bare key string* (not as a `{ "key": ... }`
//! object), so REST / JSON round-trips the key transparently — a `cover`
//! field comes back as `"ab12-photo.jpg"`, the same shape a plain
//! `String` column would have. Deserialisation reads a string straight
//! into the key. The resolved URL is a *render-time* concern
//! ([`FileField::url`]), never persisted or serialised.
//!
//! ## Storage resolution is non-fatal
//!
//! [`FileField::url`] resolves through [`crate::storage::storage_opt`],
//! which returns `None` when no backend is registered. In that case
//! `url()` falls back to the raw key rather than panicking, so a
//! template render never blows up just because the media plugin wasn't
//! wired. The boot system check (`field.storage_backend`) is the loud
//! guard: a model that declares a file/image field but registers no
//! `Storage` backend fails `App::build()`, so the silent-fallback path
//! is only ever hit in tests / transitional states.

use serde::{Deserialize, Serialize};

/// A TEXT-backed handle to a file in the ambient
/// [`Storage`](crate::storage::Storage) backend.
///
/// The inner `String` is the storage *key*. Construct one from a key
/// with [`FileField::from`] (`String` or `&str`), read the key with
/// [`FileField::key`], and resolve the public URL with
/// [`FileField::url`]. See the module docs for the serialisation +
/// storage-resolution contract.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FileField(String);

impl FileField {
    /// Borrow the storage key — the value persisted in the column.
    pub fn key(&self) -> &str {
        &self.0
    }

    /// Resolve the public URL for this file through the ambient storage
    /// backend.
    ///
    /// When a backend is registered (the production posture — the boot
    /// system check enforces it for any model with a file/image field),
    /// this returns `storage.url(key)`. When none is registered, it
    /// falls back to the raw key so a template render never panics. See
    /// the module docs.
    pub fn url(&self) -> String {
        crate::storage::storage_opt()
            .map(|s| s.url(self.key()))
            .unwrap_or_else(|| self.0.clone())
    }

    /// `true` when no file is attached (the key is empty). The default
    /// `FileField` is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for FileField {
    fn from(key: String) -> Self {
        FileField(key)
    }
}

impl From<&str> for FileField {
    fn from(key: &str) -> Self {
        FileField(key.to_string())
    }
}

impl AsRef<str> for FileField {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FileField {
    /// Renders the storage key (not the resolved URL) — matches the
    /// serialised form and what a bare `String` column would print. Use
    /// [`FileField::url`] when you need the public URL.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A [`FileField`] that defaults to the `image` widget.
///
/// Behaviourally identical to `FileField` at the storage / serde / sqlx
/// layer — it derefs to the inner `FileField`, so `cover.key()`,
/// `cover.url()`, and `cover.is_empty()` all work. The only difference
/// is the default widget the `#[derive(Model)]` macro assigns
/// (`"image"` vs `"file"`), which a later wave's admin uses to render a
/// preview. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ImageField(FileField);

impl std::ops::Deref for ImageField {
    type Target = FileField;
    fn deref(&self) -> &FileField {
        &self.0
    }
}

impl From<String> for ImageField {
    fn from(key: String) -> Self {
        ImageField(FileField::from(key))
    }
}

impl From<&str> for ImageField {
    fn from(key: &str) -> Self {
        ImageField(FileField::from(key))
    }
}

impl AsRef<str> for ImageField {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl std::fmt::Display for ImageField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

// =========================================================================
// serde + sqlx: both types are TEXT-backed string newtypes.
//
// The two share one set of impls via the `impl_string_newtype!` macro
// below so there's a single source of truth for "serialise as the bare
// key; encode/decode as TEXT on both backends." `ImageField` wraps
// `FileField` rather than `String`, but the macro's `$inner` accessor
// (`.key()` / construction `From<String>`) papers over that — the wire
// shape is identical.
// =========================================================================

/// Generate the serde + sqlx impls for a TEXT-backed string newtype.
///
/// `$ty` is the wrapper; it must impl `From<String>` (to build from a
/// decoded key) and expose its key via `.key()` (to encode / serialise).
/// Applied to both `FileField` and `ImageField` so the encode/decode and
/// serialise logic lives in exactly one place.
macro_rules! impl_string_newtype {
    ($ty:ty) => {
        impl Serialize for $ty {
            /// Serialise as the bare key string so REST / JSON round-trip
            /// the key transparently (same shape as a `String` column).
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.key())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            /// Read a JSON string straight into the key.
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let key = String::deserialize(d)?;
                Ok(<$ty>::from(key))
            }
        }

        impl sqlx::Type<sqlx::Sqlite> for $ty {
            fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
                <String as sqlx::Type<sqlx::Sqlite>>::type_info()
            }
            fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
                <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
            }
        }

        impl sqlx::Type<sqlx::Postgres> for $ty {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <String as sqlx::Type<sqlx::Postgres>>::type_info()
            }
            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for $ty {
            fn decode(
                value: sqlx::sqlite::SqliteValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let s = <String as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
                Ok(<$ty>::from(s))
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $ty {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                Ok(<$ty>::from(s))
            }
        }

        impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for $ty {
            fn encode_by_ref(
                &self,
                buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(
                    &self.key().to_string(),
                    buf,
                )
            }
        }

        impl<'q> sqlx::Encode<'q, sqlx::Postgres> for $ty {
            fn encode_by_ref(
                &self,
                buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <String as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(
                    &self.key().to_string(),
                    buf,
                )
            }
        }
    };
}

impl_string_newtype!(FileField);
impl_string_newtype!(ImageField);
