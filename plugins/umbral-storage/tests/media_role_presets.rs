//! Task 8: `media_access_staff` / `media_access_roles` — thin presets over
//! `media_access_cached`. A stub `Authentication` reads caller shape off
//! request headers so each test drives a known `Identity` through
//! `MediaCaller::resolve` without a real auth backend.

use std::sync::Arc;

use http::HeaderMap;
use umbral::auth::{FnAuthentication, Identity, set_default_authentication};
use umbral_storage::StoragePlugin;

/// Installed once per test binary (OnceLock); reads caller shape from
/// headers so each test can drive a different `Identity` through it.
fn install_stub_auth() {
    set_default_authentication(Arc::new(FnAuthentication::new(
        |headers: HeaderMap| async move {
            let uid = headers.get("x-user")?.to_str().ok()?.to_string();
            let is_staff = headers.get("x-staff").is_some();
            let is_superuser = headers.get("x-superuser").is_some();
            let mut identity = Identity::user(uid)
                .with_staff(is_staff)
                .with_superuser(is_superuser);
            if let Some(v) = headers.get("x-roles") {
                let roles: Vec<serde_json::Value> = v
                    .to_str()
                    .ok()?
                    .split(',')
                    .map(|r| serde_json::Value::String(r.to_string()))
                    .collect();
                identity = identity.with_extra("roles", serde_json::Value::Array(roles));
            }
            if headers.get("x-roles-wrong-type").is_some() {
                identity = identity.with_extra("roles", serde_json::Value::String("admin".into()));
            }
            Some(identity)
        },
    )));
    // Also register the ambient tagged cache the presets go through via
    // `media_access_cached`; missing cache would just fall back to
    // recompute-every-time, but installing it exercises the real path.
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    h
}

#[tokio::test]
async fn media_access_staff_allows_staff() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_staff();
    let access = plugin.resolve_access().expect("access fn");
    let allowed = access(&headers(&[("x-user", "1"), ("x-staff", "1")]), "k").await;
    assert!(allowed, "a staff caller must be allowed");
}

#[tokio::test]
async fn media_access_staff_denies_non_staff() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_staff();
    let access = plugin.resolve_access().expect("access fn");
    let denied = access(&headers(&[("x-user", "2")]), "k2").await;
    assert!(!denied, "a non-staff, non-superuser caller must be denied");
}

#[tokio::test]
async fn media_access_staff_allows_superuser_non_staff() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_staff();
    let access = plugin.resolve_access().expect("access fn");
    let allowed = access(&headers(&[("x-user", "3"), ("x-superuser", "1")]), "k3").await;
    assert!(allowed, "a superuser must be allowed even without is_staff");
}

#[tokio::test]
async fn media_access_roles_allows_when_role_present_in_extras() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_roles(["editor", "admin"]);
    let access = plugin.resolve_access().expect("access fn");
    let allowed = access(
        &headers(&[("x-user", "4"), ("x-roles", "viewer,editor")]),
        "k4",
    )
    .await;
    assert!(
        allowed,
        "extras[\"roles\"] containing an allowed role must grant access"
    );
}

#[tokio::test]
async fn media_access_roles_denies_when_role_absent() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_roles(["admin"]);
    let access = plugin.resolve_access().expect("access fn");
    let denied = access(&headers(&[("x-user", "5"), ("x-roles", "viewer")]), "k5").await;
    assert!(!denied, "a caller without any allowed role must be denied");
}

#[tokio::test]
async fn media_access_roles_allows_superuser_without_matching_role() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_roles(["admin"]);
    let access = plugin.resolve_access().expect("access fn");
    let allowed = access(
        &headers(&[("x-user", "6"), ("x-superuser", "1"), ("x-roles", "viewer")]),
        "k6",
    )
    .await;
    assert!(allowed, "a superuser must bypass the role check");
}

/// `extras["roles"]` present but the WRONG type (a bare string, not an
/// array) must resolve to an empty role set — not panic — so the preset
/// simply denies (`MediaCaller::resolve`'s `.as_array()` returns `None`).
#[tokio::test]
async fn media_access_roles_wrong_extras_type_resolves_to_empty_and_denies() {
    install_stub_auth();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_roles(["admin"]);
    let access = plugin.resolve_access().expect("access fn");
    let denied = access(
        &headers(&[("x-user", "7"), ("x-roles-wrong-type", "1")]),
        "k7",
    )
    .await;
    assert!(
        !denied,
        "a non-array extras[\"roles\"] must not panic and must resolve to no roles"
    );
}
