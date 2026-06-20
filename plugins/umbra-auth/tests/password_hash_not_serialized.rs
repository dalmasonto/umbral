//! Type-level guarantee: `AuthUser::password_hash` must never appear in
//! serialized JSON output.
//!
//! The argon2 hash is blocked by the REST block-list at runtime, but
//! that runtime defence requires the caller to remember to apply it.
//! The `#[serde(skip_serializing)]` attribute on the field is the
//! type-level guarantee that no serialization path — REST, template
//! context, debug logging, etc. — can accidentally emit it.

use chrono::Utc;
use umbra_auth::AuthUser;

/// Serializing `AuthUser` must NOT include `password_hash` in the output.
/// A known hash value is used so we can assert it is truly absent (not
/// just that the key is missing but the value leaked some other way).
#[test]
fn password_hash_absent_from_serialized_json() {
    let user = AuthUser {
        id: 1,
        username: "alice".to_string(),
        email: "alice@example.com".to_string(),
        password_hash: "$argon2id$v=19$m=19456,t=2,p=1$SENTINEL_HASH_VALUE".to_string(),
        is_active: true,
        is_staff: false,
        is_superuser: false,
        date_joined: Utc::now(),
        last_login: None,
    };

    let value = serde_json::to_value(&user).expect("AuthUser must be serializable");

    // The hash itself must not appear anywhere in the output.
    let serialized = value.to_string();
    assert!(
        !serialized.contains("SENTINEL_HASH_VALUE"),
        "password_hash value leaked into serialized output: {serialized}"
    );

    // The key must be absent from the top-level object.
    let obj = value.as_object().expect("serialized form must be a JSON object");
    assert!(
        !obj.contains_key("password_hash"),
        "password_hash key present in serialized output: {serialized}"
    );

    // Sanity check: normal fields must still be present so we know
    // serialization is actually running (not just producing `{}`).
    assert_eq!(
        obj.get("username").and_then(|v| v.as_str()),
        Some("alice"),
        "username must be present in serialized output"
    );
    assert_eq!(
        obj.get("email").and_then(|v| v.as_str()),
        Some("alice@example.com"),
        "email must be present in serialized output"
    );
}

/// Deserialization must still work: the field is only `skip_serializing`,
/// not `skip`. Code that reads an `AuthUser` from a JSON payload (e.g. an
/// internal cache round-trip or a future admin import) must still be able
/// to populate `password_hash`.
#[test]
fn password_hash_still_deserializes() {
    let json = serde_json::json!({
        "id": 2,
        "username": "bob",
        "email": "bob@example.com",
        "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$ANOTHER_SENTINEL",
        "is_active": true,
        "is_staff": false,
        "is_superuser": false,
        "date_joined": "2024-01-01T00:00:00Z",
        "last_login": null
    });

    let user: AuthUser = serde_json::from_value(json).expect("must deserialize");
    assert_eq!(user.password_hash, "$argon2id$v=19$m=19456,t=2,p=1$ANOTHER_SENTINEL");
    assert_eq!(user.username, "bob");
}
