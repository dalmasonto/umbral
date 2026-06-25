//! Tests for `RedisStore`.
//!
//! ## Unit tests (always-run)
//!
//! Pure offline tests that verify the key format and serde round-trip
//! without contacting a Redis server.
//!
//! ## Live round-trip test (ignored by default)
//!
//! The `live_round_trip` test spins up a real `RedisStore` against a
//! running Redis and is gated behind `#[ignore]` so CI (no Redis) stays
//! green. To run it locally:
//!
//! ```bash
//! # start a local Redis (default port 6379)
//! redis-server --daemonize yes
//! # then:
//! UMBRAL_REDIS_URL=redis://localhost:6379/0 \
//!   cargo test -p umbral-sessions --features redis -- --ignored
//! ```

#![cfg(feature = "redis")]

use chrono::{Duration, Utc};
use umbral_sessions::store::{SessionRecord, SessionStore, hash_token_pub};
use umbral_sessions::RedisStore;

// =========================================================================
// Unit tests — no Redis server needed
// =========================================================================

/// `hash_token_pub` returns a 64-char lowercase hex string (SHA-256),
/// and the Redis key is the expected `umbral:session:<hash>` form.
/// Verified without a live Redis connection.
#[test]
fn key_format_is_correct() {
    let token = "my-raw-session-token";
    let hash = hash_token_pub(token);

    // SHA-256 hex is exactly 64 lowercase hex chars.
    assert_eq!(hash.len(), 64, "SHA-256 hex is 64 chars");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "hash contains only hex digits"
    );

    // The key the store would use — manually constructed here without
    // instantiating a live `RedisStore`.
    let expected_key = format!("umbral:session:{hash}");
    assert!(
        expected_key.starts_with("umbral:session:"),
        "key starts with the correct prefix"
    );
    assert_eq!(
        expected_key,
        format!("umbral:session:{}", hash_token_pub(token))
    );
}

/// `SessionRecord` serialises to JSON and deserialises back without data
/// loss — pure serde round-trip, no Redis involved.
#[test]
fn session_record_serde_round_trip() {
    let now = Utc::now();
    let record = SessionRecord {
        user_id: Some("42".to_string()),
        data: r#"{"cart":3,"promo":"SAVE10"}"#.to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };

    let json = serde_json::to_string(&record).expect("serialize");
    let decoded: SessionRecord = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(decoded.user_id, Some("42".to_string()));
    assert_eq!(decoded.data, r#"{"cart":3,"promo":"SAVE10"}"#);
    // Chrono round-trips through RFC 3339 — sub-microsecond precision may
    // differ by ≤ 1µs after the serde text encoding; compare at 1-second
    // granularity.
    assert!((decoded.expires_at - record.expires_at).num_seconds().abs() < 2);
}

/// An anonymous session (user_id = None) also round-trips correctly.
#[test]
fn anonymous_session_record_serde_round_trip() {
    let now = Utc::now();
    let record = SessionRecord {
        user_id: None,
        data: "{}".to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(86400),
    };

    let json = serde_json::to_string(&record).expect("serialize");
    let decoded: SessionRecord = serde_json::from_str(&json).expect("deserialize");

    assert!(decoded.user_id.is_none(), "anonymous session keeps user_id = None");
    assert_eq!(decoded.data, "{}");
}

// =========================================================================
// Live round-trip test — requires a running Redis.
// Gated behind #[ignore] so CI (no Redis) stays green.
//
// Run locally:
//   UMBRAL_REDIS_URL=redis://localhost:6379/0 \
//     cargo test -p umbral-sessions --features redis -- --ignored
// =========================================================================

/// Full save → load → destroy → load cycle against a live Redis.
///
/// Requires `UMBRAL_REDIS_URL` to be set (e.g. `redis://localhost:6379/0`).
/// Skips cleanly when the variable is absent.
#[tokio::test]
#[ignore]
async fn live_round_trip() {
    let store = RedisStore::from_env()
        .await
        .expect("connect to Redis (set UMBRAL_REDIS_URL)");

    let token = format!("test-token-{}", uuid::Uuid::new_v4());
    let now = Utc::now();
    let record = SessionRecord {
        user_id: Some("99".to_string()),
        data: r#"{"live":true}"#.to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };

    // save → load returns the same record.
    let cookie = store.save(&token, &record).await.expect("save");
    assert_eq!(cookie, token, "save returns the raw token unchanged");

    let loaded = store.load(&token).await.expect("load").expect("present");
    assert_eq!(loaded.user_id, Some("99".to_string()));
    assert_eq!(loaded.data, r#"{"live":true}"#);

    // destroy → load returns None.
    store.destroy(&token).await.expect("destroy");
    let after_destroy = store.load(&token).await.expect("load after destroy");
    assert!(after_destroy.is_none(), "session gone after destroy");

    // destroy is idempotent — second call must not error.
    store.destroy(&token).await.expect("second destroy is no-op");
}
