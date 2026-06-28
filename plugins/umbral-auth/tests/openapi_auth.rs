//! Unit test: `openapi_paths` exposes all eight auth endpoints.
//!
//! Pure function call — no App boot, no DB, no network.

#[test]
fn openapi_lists_the_new_auth_endpoints() {
    let paths = umbral_auth::auth_routes_openapi_for_test("/api/auth");
    let keys: Vec<&str> = paths.iter().map(|(p, _)| p.as_str()).collect();
    for p in [
        "/api/auth/verify-email",
        "/api/auth/resend-verification",
        "/api/auth/password-forgot",
        "/api/auth/password-reset",
    ] {
        assert!(keys.contains(&p), "openapi missing {p}; got {keys:?}");
    }
}
