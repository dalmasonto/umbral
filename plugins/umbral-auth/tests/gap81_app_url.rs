//! gap 81 — the emailed password-reset link honours a configured public base
//! URL (`settings.app_url`) instead of being derived only from the request's
//! `Host` / `X-Forwarded-Proto` headers.
//!
//! Real failure this guards against (backend_v2): a Next.js BFF forwards
//! `POST /api/auth/password-forgot` server-side, so the backend sees
//! `Host: localhost:8000` and used to emit `https://localhost:8000/auth/reset`,
//! while the reset PAGE lives on the frontend at `http://localhost:3000/auth/reset`
//! — wrong host AND scheme, with no way to override it.
//!
//! We exercise the pure link-builder seam [`reset_url_base_with`] so both the
//! configured and the fallback branch are covered deterministically, without
//! mutating the process-global settings `OnceLock` or the shared
//! `UMBRAL_APP_URL` env var (either would make this test order-dependent under
//! cargo's parallel runner).

use umbral::web::HeaderMap;
use umbral_auth::auth_routes::reset_url_base_with;

/// Build a `HeaderMap` mimicking what the backend sees behind a BFF that
/// forwards server-side: an internal `Host` and (optionally) a forwarded proto.
fn bff_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("host", "localhost:8000".parse().unwrap());
    h.insert("x-forwarded-proto", "https".parse().unwrap());
    h
}

#[test]
fn app_url_wins_over_request_host_and_proto() {
    // The user-facing frontend origin, NOT the internal backend host.
    let link = reset_url_base_with(Some("http://localhost:3000"), &bff_headers());
    assert_eq!(
        link, "http://localhost:3000/auth/reset",
        "with app_url set the reset link must use it regardless of Host/X-Forwarded-Proto"
    );
}

#[test]
fn app_url_trailing_slash_is_normalised() {
    // A trailing slash on app_url must not double up against the path.
    let link = reset_url_base_with(Some("https://app.example.com/"), &bff_headers());
    assert_eq!(link, "https://app.example.com/auth/reset");
}

#[test]
fn app_url_with_path_prefix_is_preserved() {
    let link = reset_url_base_with(Some("https://example.com/app"), &bff_headers());
    assert_eq!(link, "https://example.com/app/auth/reset");
}

#[test]
fn empty_app_url_falls_back_to_headers() {
    // A blank/whitespace app_url must be treated as unset (fall back), never
    // producing a bare `/auth/reset` when a Host is actually present.
    let link = reset_url_base_with(Some("   "), &bff_headers());
    assert_eq!(link, "https://localhost:8000/auth/reset");
}

#[test]
fn no_app_url_is_header_derived_as_before() {
    // Fallback / no-regression: unset app_url reproduces the historical
    // `{proto}://{host}/auth/reset` behaviour exactly.
    let link = reset_url_base_with(None, &bff_headers());
    assert_eq!(link, "https://localhost:8000/auth/reset");
}

#[test]
fn no_app_url_defaults_proto_to_https() {
    // No X-Forwarded-Proto → default "https".
    let mut h = HeaderMap::new();
    h.insert("host", "example.com".parse().unwrap());
    assert_eq!(
        reset_url_base_with(None, &h),
        "https://example.com/auth/reset"
    );
}

#[test]
fn no_app_url_and_no_host_is_relative_path() {
    // No app_url and no Host header (a bare test client) → relative path,
    // matching the long-standing fallback.
    let link = reset_url_base_with(None, &HeaderMap::new());
    assert_eq!(link, "/auth/reset");
}
