//! API versioning.
//!
//! Versioning is **opt-in**. A [`RestPlugin`](crate::RestPlugin) with no
//! `.versioning(...)` call behaves exactly as before: routes mount at
//! `/api/<table>/` with no version segment and `RequestContext::version`
//! is always `None`.
//!
//! Two schemes ship:
//!
//! - [`VersioningScheme::UrlPath`] — the version is a path segment right
//!   after the base path: `/api/v1/<table>/`. Routes mount under
//!   `{base}/{version}/...` for **each** allowed version, so `/api/v1/...`
//!   and `/api/v2/...` both resolve when both are allowed. A request to a
//!   version that isn't in `allowed_versions` matches no route → **404**.
//!   The version is required in the path once this scheme is on; there is
//!   no unversioned `/api/<table>/` fallback when UrlPath versioning is
//!   configured.
//!
//! - [`VersioningScheme::AcceptHeader`] — paths stay `/api/<table>/`; the
//!   version comes from the `Accept` header's media-type `version` param
//!   (`Accept: application/json; version=v2`) or a configurable header
//!   (e.g. `X-API-Version`). An absent version falls back to
//!   `default_version`; a version that isn't in `allowed_versions` is
//!   rejected with **406 Not Acceptable**.
//!
//! Once resolved, the version is exposed on
//! [`RequestContext`](crate::resource::RequestContext) so handlers,
//! `transform`, and `computed` callbacks can branch on it.

/// Which versioning scheme a [`RestPlugin`](crate::RestPlugin) uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersioningScheme {
    /// The version is a path segment after the base path
    /// (`/api/v1/<table>/`): version in the URL path.
    UrlPath,
    /// The version comes from a request header. `header` is the header
    /// name to read; the special value `"Accept"` (case-insensitive)
    /// reads the media-type `version` param from the `Accept` header
    /// (`application/json; version=v2`). Any other name is read as a
    /// plain header value (e.g. `X-API-Version: v2`): version read from a
    /// request header.
    AcceptHeader { header: String },
}

impl VersioningScheme {
    /// Version in the URL path.
    pub fn url_path() -> Self {
        Self::UrlPath
    }

    /// Version read from the Accept header: `Accept: ...; version=<v>`.
    pub fn accept_header() -> Self {
        Self::AcceptHeader {
            header: "Accept".to_string(),
        }
    }

    /// Header-based versioning that reads a plain header, e.g.
    /// `VersioningScheme::header("X-API-Version")`.
    pub fn header(name: impl Into<String>) -> Self {
        Self::AcceptHeader {
            header: name.into(),
        }
    }
}

/// The resolved versioning configuration carried on the plugin.
///
/// Build it via [`RestPlugin::versioning`](crate::RestPlugin::versioning)
/// plus the chained `.default_version(...)`, `.allowed_version(...)`, and
/// `.allowed_versions([...])` builders.
#[derive(Debug, Clone)]
pub struct VersioningConfig {
    pub scheme: VersioningScheme,
    /// The version assumed when a request carries none (header schemes)
    /// or for documentation. `None` until set.
    pub default_version: Option<String>,
    /// The closed set of versions the API serves. A version outside this
    /// set is rejected (404 for UrlPath — no route; 406 for header
    /// schemes). Empty means "no allow-list enforced" — every syntactic
    /// version is accepted (rarely what you want; set at least one).
    pub allowed_versions: Vec<String>,
}

impl VersioningConfig {
    pub fn new(scheme: VersioningScheme) -> Self {
        Self {
            scheme,
            default_version: None,
            allowed_versions: Vec::new(),
        }
    }

    /// Set the version used when a request supplies none (header schemes).
    /// For UrlPath the version is required in the path, so this only seeds
    /// `RequestContext::version` documentation defaults / OpenAPI.
    pub fn default_version(mut self, v: impl Into<String>) -> Self {
        self.default_version = Some(v.into());
        self
    }

    /// Add one version to the allow-list.
    pub fn allowed_version(mut self, v: impl Into<String>) -> Self {
        let v = v.into();
        if !self.allowed_versions.contains(&v) {
            self.allowed_versions.push(v);
        }
        self
    }

    /// Add several versions to the allow-list.
    pub fn allowed_versions<I, S>(mut self, versions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for v in versions {
            self = self.allowed_version(v);
        }
        self
    }

    /// Is `v` an accepted version? An empty allow-list accepts everything.
    pub fn is_allowed(&self, v: &str) -> bool {
        self.allowed_versions.is_empty() || self.allowed_versions.iter().any(|a| a == v)
    }
}

/// Extract the version a header-scheme request asks for, if any.
///
/// For `Accept`: reads the `version=<v>` media-type parameter out of the
/// `Accept` header value (`application/json; version=v2` → `Some("v2")`).
/// For a plain header name: returns its trimmed value.
///
/// Returns `None` when the header / param is absent.
pub fn version_from_headers(
    headers: &umbral::web::HeaderMap,
    header_name: &str,
) -> Option<String> {
    if header_name.eq_ignore_ascii_case("accept") {
        let raw = headers.get(http::header::ACCEPT)?.to_str().ok()?;
        // `application/json; version=v2; q=0.9` — find the `version=` param.
        for part in raw.split(';').map(str::trim) {
            if let Some(val) = part
                .strip_prefix("version=")
                .or_else(|| part.strip_prefix("version ="))
            {
                let val = val.trim().trim_matches('"');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
        None
    } else {
        let raw = headers.get(header_name)?.to_str().ok()?;
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_accepts_listed_rejects_others() {
        let cfg = VersioningConfig::new(VersioningScheme::url_path())
            .allowed_versions(["v1", "v2"])
            .default_version("v1");
        assert!(cfg.is_allowed("v1"));
        assert!(cfg.is_allowed("v2"));
        assert!(!cfg.is_allowed("v3"));
    }

    #[test]
    fn empty_allow_list_accepts_any() {
        let cfg = VersioningConfig::new(VersioningScheme::url_path());
        assert!(cfg.is_allowed("v9"));
    }

    #[test]
    fn accept_header_version_param_is_read() {
        let mut h = umbral::web::HeaderMap::new();
        h.insert(
            http::header::ACCEPT,
            "application/json; version=v2".parse().unwrap(),
        );
        assert_eq!(version_from_headers(&h, "Accept"), Some("v2".to_string()));
    }

    #[test]
    fn plain_header_version_is_read() {
        let mut h = umbral::web::HeaderMap::new();
        h.insert("x-api-version", "v3".parse().unwrap());
        assert_eq!(
            version_from_headers(&h, "X-API-Version"),
            Some("v3".to_string())
        );
    }

    #[test]
    fn absent_header_yields_none() {
        let h = umbral::web::HeaderMap::new();
        assert_eq!(version_from_headers(&h, "Accept"), None);
        assert_eq!(version_from_headers(&h, "X-API-Version"), None);
    }
}
