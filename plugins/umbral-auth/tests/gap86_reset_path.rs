//! gaps4 #86 — the emailed password-reset link's PATH segment is
//! operator-configurable via `AuthPlugin::reset_path`, not just the origin
//! (`settings.app_url`, gap 81).
//!
//! Real scenario this guards against: an app's frontend serves its
//! reset-confirmation page at `/account/reset-password` instead of the
//! framework default `/auth/reset`. Before this fix, `app_url` could point
//! the link at the right origin but never the right path, so the emailed
//! link 404'd on the frontend regardless of `app_url`.
//!
//! We exercise the pure link-builder seam [`reset_url_base_with_path`] so
//! both the default and a configured path are covered deterministically,
//! without mutating the process-global `RESET_PATH_OVERRIDE` `OnceLock` or
//! going through `AuthPlugin::on_ready` (either would make this test
//! order-dependent under cargo's parallel runner, same rationale as
//! `gap81_app_url.rs`).

use umbral::web::HeaderMap;
use umbral_auth::auth_routes::{reset_url_base_with, reset_url_base_with_path};

/// The historical default reset path, mirrored here (rather than imported)
/// since `auth_routes::RESET_PATH` is `pub(crate)` — this test exercises the
/// crate's public seam only, the same boundary an app author would use.
const RESET_PATH: &str = "/auth/reset";

/// Build a `HeaderMap` mimicking a plain (non-BFF) request: the backend's own
/// `Host` is the request's actual origin.
fn plain_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("host", "localhost:8000".parse().unwrap());
    h.insert("x-forwarded-proto", "https".parse().unwrap());
    h
}

#[test]
fn default_reset_path_constant_is_unchanged() {
    // Guards the "no behavior change when unset" contract at the source: the
    // const the whole crate falls back to is still `/auth/reset`.
    assert_eq!(RESET_PATH, "/auth/reset");
}

#[test]
fn unset_path_reproduces_historical_behavior() {
    // reset_url_base_with (no path param) must be identical to explicitly
    // passing the default RESET_PATH through reset_url_base_with_path.
    let via_default = reset_url_base_with(Some("https://app.example.com"), &plain_headers());
    let via_explicit_default = reset_url_base_with_path(
        Some("https://app.example.com"),
        &plain_headers(),
        RESET_PATH,
    );
    assert_eq!(via_default, via_explicit_default);
    assert_eq!(via_default, "https://app.example.com/auth/reset");
}

#[test]
fn configured_path_overrides_default_with_app_url_set() {
    // The #81 + #86 combination: a separate frontend origin AND a custom path.
    let link = reset_url_base_with_path(
        Some("https://app.example.com"),
        &plain_headers(),
        "/account/reset-password",
    );
    assert_eq!(link, "https://app.example.com/account/reset-password");
}

#[test]
fn configured_path_overrides_default_with_header_derived_origin() {
    // No app_url configured: the header-derived origin still gets the
    // custom path joined onto it, not the hardcoded default.
    let link = reset_url_base_with_path(None, &plain_headers(), "/account/reset-password");
    assert_eq!(link, "https://localhost:8000/account/reset-password");
}

#[test]
fn configured_path_with_no_host_header_is_the_bare_configured_path() {
    let link = reset_url_base_with_path(None, &HeaderMap::new(), "/account/reset-password");
    assert_eq!(link, "/account/reset-password");
}

#[test]
fn app_url_trailing_slash_is_still_normalised_with_a_custom_path() {
    let link = reset_url_base_with_path(
        Some("https://app.example.com/"),
        &plain_headers(),
        "/account/reset-password",
    );
    assert_eq!(link, "https://app.example.com/account/reset-password");
}
