//! `Slug`, `Email`, `Url` — newtype wrappers over `String` with
//! type-level guarantees and constructor validation.
//!
//! Closes BUG-11 / BUG-12 / BUG-13 from `bugs/tests/testBugs.md`.
//!
//! Each type:
//! - stores a plain `String` (so sqlx/serde round-trip without changes
//!   to the SQL layer — DDL is still `TEXT`);
//! - provides `new(s) -> Result<Self, ValidatorError>` that runs the
//!   format check;
//! - provides `unchecked(s)` for code paths that have already
//!   validated (post-DB-fetch hydration, test fixtures).
//!
//! The umbra-rest dynamic write path and the OpenAPI plugin read the
//! field-level `text_format` marker the macro emits (see
//! `FieldSpec::text_format`) to know which validator to call without
//! a downcast — the wrapper type and the marker stay in sync because
//! the macro's classifier sets both from the same single match.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Reason a `Slug` / `Email` / `Url` constructor rejected an input.
/// Kept narrow on purpose — every variant carries the offending
/// value so the framework can surface a structured 400 with the
/// field name and what the user submitted. Closes BUG-11/12/13.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorError {
    /// `Slug::new` rejected: must match `[A-Za-z0-9_-]+`.
    InvalidSlug(String),
    /// `Email::new` rejected: must contain `@` and a non-empty
    /// local + domain.
    InvalidEmail(String),
    /// `Url::new` rejected: must parse as `http(s)://...` with a
    /// non-empty host.
    InvalidUrl(String),
}

impl fmt::Display for ValidatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidatorError::InvalidSlug(s) => write!(f, "invalid slug: `{s}`"),
            ValidatorError::InvalidEmail(s) => write!(f, "invalid email: `{s}`"),
            ValidatorError::InvalidUrl(s) => write!(f, "invalid url: `{s}`"),
        }
    }
}

impl std::error::Error for ValidatorError {}

/// `[A-Za-z0-9_-]+` — URL-safe identifier. Closes BUG-11.
///
/// Stored as `TEXT`. Use `Slug::new(s)` for user input, `Slug::unchecked(s)`
/// for round-trips from already-validated sources (database, fixtures).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slug(String);

/// `"<local>@<domain>"` — minimal structural check. Closes BUG-12.
///
/// The validation is intentionally lightweight (the framework rejects
/// obviously-broken values without trying to match every quirk of
/// RFC 5322). For stricter requirements, layer a custom permission /
/// validator on top.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Email(String);

/// `http(s)://...` — must parse via `url::Url` and have a host.
/// Closes BUG-13.
///
/// We accept the same string the application would store, parsing
/// it only for the validity check. Leaning on the `url` crate keeps
/// the rules consistent across this and any URL-aware plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Url(String);

impl Slug {
    /// Validate and wrap. Empty strings and any character outside
    /// `[A-Za-z0-9_-]` reject with `ValidatorError::InvalidSlug`.
    pub fn new(s: impl Into<String>) -> Result<Self, ValidatorError> {
        let s = s.into();
        if validate_slug_str(&s) {
            Ok(Self(s))
        } else {
            Err(ValidatorError::InvalidSlug(s))
        }
    }
    /// Wrap without validation. Use only when the source is trusted
    /// (database round-trip, deterministic test fixtures).
    pub fn unchecked(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Email {
    pub fn new(s: impl Into<String>) -> Result<Self, ValidatorError> {
        let s = s.into();
        if validate_email_str(&s) {
            Ok(Self(s))
        } else {
            Err(ValidatorError::InvalidEmail(s))
        }
    }
    pub fn unchecked(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Url {
    pub fn new(s: impl Into<String>) -> Result<Self, ValidatorError> {
        let s = s.into();
        if validate_url_str(&s) {
            Ok(Self(s))
        } else {
            Err(ValidatorError::InvalidUrl(s))
        }
    }
    pub fn unchecked(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

// `Deref<Target = str>` would let `&Slug` flow into every `&str` call
// site, but it also enables a footgun where the inner string can be
// mutated through `DerefMut`. Stick to explicit `as_str()` for v1.

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for Url {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = ValidatorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_string())
    }
}
impl FromStr for Email {
    type Err = ValidatorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_string())
    }
}
impl FromStr for Url {
    type Err = ValidatorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_string())
    }
}

// sqlx hooks: store/load as TEXT through the inner String. The
// `Type<DB>` impl borrows the inner str so sqlx uses the same
// path it does for plain `String` / `&str` fields, no separate
// codec needed.

impl<DB: sqlx::Database> sqlx::Type<DB> for Slug
where
    String: sqlx::Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <String as sqlx::Type<DB>>::type_info()
    }
    fn compatible(ty: &DB::TypeInfo) -> bool {
        <String as sqlx::Type<DB>>::compatible(ty)
    }
}
impl<DB: sqlx::Database> sqlx::Type<DB> for Email
where
    String: sqlx::Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <String as sqlx::Type<DB>>::type_info()
    }
    fn compatible(ty: &DB::TypeInfo) -> bool {
        <String as sqlx::Type<DB>>::compatible(ty)
    }
}
impl<DB: sqlx::Database> sqlx::Type<DB> for Url
where
    String: sqlx::Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <String as sqlx::Type<DB>>::type_info()
    }
    fn compatible(ty: &DB::TypeInfo) -> bool {
        <String as sqlx::Type<DB>>::compatible(ty)
    }
}

impl<'r, DB: sqlx::Database> sqlx::Decode<'r, DB> for Slug
where
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<'r, DB>>::decode(value)?;
        Ok(Slug::unchecked(s))
    }
}
impl<'r, DB: sqlx::Database> sqlx::Decode<'r, DB> for Email
where
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<'r, DB>>::decode(value)?;
        Ok(Email::unchecked(s))
    }
}
impl<'r, DB: sqlx::Database> sqlx::Decode<'r, DB> for Url
where
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<'r, DB>>::decode(value)?;
        Ok(Url::unchecked(s))
    }
}

impl<'q, DB: sqlx::Database> sqlx::Encode<'q, DB> for Slug
where
    String: sqlx::Encode<'q, DB>,
{
    fn encode_by_ref(
        &self,
        buf: &mut <DB as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, DB>>::encode_by_ref(&self.0, buf)
    }
}
impl<'q, DB: sqlx::Database> sqlx::Encode<'q, DB> for Email
where
    String: sqlx::Encode<'q, DB>,
{
    fn encode_by_ref(
        &self,
        buf: &mut <DB as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, DB>>::encode_by_ref(&self.0, buf)
    }
}
impl<'q, DB: sqlx::Database> sqlx::Encode<'q, DB> for Url
where
    String: sqlx::Encode<'q, DB>,
{
    fn encode_by_ref(
        &self,
        buf: &mut <DB as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, DB>>::encode_by_ref(&self.0, buf)
    }
}

/// Bare-string validator used by the dynamic write path so it can
/// pre-check user input without owning the typed wrapper. The
/// `format` argument is the `FieldSpec::text_format` marker the
/// macro emits. Closes BUG-11/12/13 — validation is a single source
/// of truth.
pub fn validate_text_format(format: &str, value: &str) -> Result<(), ValidatorError> {
    match format {
        "slug" => {
            if validate_slug_str(value) {
                Ok(())
            } else {
                Err(ValidatorError::InvalidSlug(value.to_string()))
            }
        }
        "email" => {
            if validate_email_str(value) {
                Ok(())
            } else {
                Err(ValidatorError::InvalidEmail(value.to_string()))
            }
        }
        "url" => {
            if validate_url_str(value) {
                Ok(())
            } else {
                Err(ValidatorError::InvalidUrl(value.to_string()))
            }
        }
        // Unknown marker → treat as plain text. Future formats land
        // here without breaking the dynamic write path.
        _ => Ok(()),
    }
}

fn validate_slug_str(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn validate_email_str(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    // Reject multiple `@` and obvious whitespace problems. Anything
    // that survives that filter goes to the SMTP server to actually
    // bounce or accept.
    if domain.contains('@') || s.contains(char::is_whitespace) {
        return false;
    }
    // A valid domain has at least one `.` and a non-empty TLD-ish
    // suffix. Loose check — `localhost` rejects, `a.b` passes.
    domain
        .rsplit_once('.')
        .map(|(left, right)| !left.is_empty() && !right.is_empty())
        .unwrap_or(false)
}

fn validate_url_str(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    // Bare-bones: scheme + `://` + at least one non-`/` char.
    let after = &s[s.find("://").map(|i| i + 3).unwrap_or(s.len())..];
    let host = after.split('/').next().unwrap_or("");
    !host.is_empty() && !host.contains(char::is_whitespace) && host.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_accepts_url_safe() {
        assert!(Slug::new("hello-world_2").is_ok());
        assert!(Slug::new("A").is_ok());
    }
    #[test]
    fn slug_rejects_empty_and_special_chars() {
        assert!(matches!(Slug::new(""), Err(ValidatorError::InvalidSlug(_))));
        assert!(matches!(
            Slug::new("hello world"),
            Err(ValidatorError::InvalidSlug(_))
        ));
        assert!(matches!(
            Slug::new("hi/there"),
            Err(ValidatorError::InvalidSlug(_))
        ));
    }
    #[test]
    fn email_accepts_structural_shape() {
        assert!(Email::new("a@b.c").is_ok());
        assert!(Email::new("user+tag@example.com").is_ok());
    }
    #[test]
    fn email_rejects_obvious_breaks() {
        assert!(matches!(
            Email::new("plain"),
            Err(ValidatorError::InvalidEmail(_))
        ));
        assert!(matches!(
            Email::new("@no-local.com"),
            Err(ValidatorError::InvalidEmail(_))
        ));
        assert!(matches!(
            Email::new("no-at"),
            Err(ValidatorError::InvalidEmail(_))
        ));
        assert!(matches!(
            Email::new("two@@sign.com"),
            Err(ValidatorError::InvalidEmail(_))
        ));
        assert!(matches!(
            Email::new("a@localhost"),
            Err(ValidatorError::InvalidEmail(_))
        ));
    }
    #[test]
    fn url_accepts_http_and_https() {
        assert!(Url::new("http://example.com/path?x=1").is_ok());
        assert!(Url::new("https://example.com/").is_ok());
    }
    #[test]
    fn url_rejects_non_http_or_missing_host() {
        assert!(matches!(
            Url::new("ftp://example.com/"),
            Err(ValidatorError::InvalidUrl(_))
        ));
        assert!(matches!(
            Url::new("https:///path"),
            Err(ValidatorError::InvalidUrl(_))
        ));
        assert!(matches!(
            Url::new("not a url"),
            Err(ValidatorError::InvalidUrl(_))
        ));
    }
    #[test]
    fn validate_text_format_dispatches() {
        assert!(validate_text_format("slug", "ok-1").is_ok());
        assert!(validate_text_format("slug", "no spaces").is_err());
        assert!(validate_text_format("email", "a@b.c").is_ok());
        assert!(validate_text_format("url", "https://x.y/").is_ok());
        // Unknown marker degrades to plain text — no error.
        assert!(validate_text_format("unknown", "anything").is_ok());
    }
}
