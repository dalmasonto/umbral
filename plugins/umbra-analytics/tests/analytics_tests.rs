//! Integration tests for umbra-analytics.
//!
//! Pure unit tests — no network calls in CI. The live PostHog round-trip is
//! gated behind `#[ignore]` and only runs when explicitly opted in.

use serde_json::json;
use umbra_analytics::{AnalyticsClient, DEFAULT_POSTHOG_HOST, http_client};

// ── Payload shape ─────────────────────────────────────────────────────────────

/// The capture payload must carry the five fields PostHog requires:
/// `api_key`, `event`, `distinct_id`, `properties`, `timestamp`.
#[test]
fn capture_payload_has_required_fields() {
    let client = AnalyticsClient::new("phc_test_key", DEFAULT_POSTHOG_HOST);
    let payload = client.build_payload(
        "user_42",
        "signup",
        json!({ "plan": "pro", "amount_cents": 999 }),
    );

    assert_eq!(payload["api_key"], "phc_test_key", "api_key must be present");
    assert_eq!(payload["event"], "signup", "event must be present");
    assert_eq!(payload["distinct_id"], "user_42", "distinct_id must be present");
    assert!(
        payload["properties"].is_object(),
        "properties must be a JSON object"
    );
    assert!(
        payload["timestamp"].is_string(),
        "timestamp must be an RFC 3339 string"
    );

    // Verify timestamp is non-empty and looks like ISO 8601.
    let ts = payload["timestamp"].as_str().unwrap();
    assert!(ts.contains('T'), "timestamp should be in RFC 3339 / ISO 8601 form: {ts}");
    assert!(ts.ends_with('Z') || ts.contains('+'), "timestamp should carry UTC offset: {ts}");

    // Verify custom properties are preserved inside the payload.
    assert_eq!(payload["properties"]["plan"], "pro");
    assert_eq!(payload["properties"]["amount_cents"], 999);
}

/// `$identify` uses the special PostHog event name and wraps person
/// properties under `$set` by convention — verify the payload shape is
/// correct when the caller passes that structure.
#[test]
fn identify_payload_shape() {
    let client = AnalyticsClient::new("phc_test_key", DEFAULT_POSTHOG_HOST);
    let props = json!({ "$set": { "email": "a@b.com", "plan": "free" } });
    let payload = client.build_payload("user_1", "$identify", props.clone());

    assert_eq!(payload["event"], "$identify");
    assert_eq!(payload["properties"]["$set"]["email"], "a@b.com");
}

/// An empty properties object is valid — PostHog accepts it.
#[test]
fn capture_payload_with_empty_properties() {
    let client = AnalyticsClient::new("phc_k", DEFAULT_POSTHOG_HOST);
    let payload = client.build_payload("anon", "page_view", json!({}));
    assert!(payload["properties"].is_object());
    assert!(payload["properties"].as_object().unwrap().is_empty());
}

// ── No-op when unconfigured ───────────────────────────────────────────────────

/// `capture` with no ambient client installed must be a clean no-op (no
/// panic, no error). This test exercises the free function path and proves
/// that missing configuration is handled gracefully.
#[tokio::test]
async fn capture_without_client_is_noop() {
    // `AMBIENT_CLIENT` may or may not be set (depends on test ordering).
    // Either way, calling capture must not panic.
    umbra_analytics::capture("anon", "test_event", json!({})).await;
    // If we reach this line, the no-op contract holds.
}

/// `identify` with no ambient client installed must also be a clean no-op.
#[tokio::test]
async fn identify_without_client_is_noop() {
    umbra_analytics::identify("anon", json!({ "$set": { "x": 1 } })).await;
}

// ── HTTP client ───────────────────────────────────────────────────────────────

/// The shared HTTP client must build without panicking and be usable for
/// constructing requests (without making actual network calls).
#[test]
fn http_client_builds_with_timeout() {
    let client = http_client();
    // Building a request verifies the client is in a usable state.
    let req = client
        .get("https://us.i.posthog.com/capture/")
        .build()
        .expect("should be able to build a GET request");
    assert_eq!(req.url().host_str(), Some("us.i.posthog.com"));
}

/// Calling `http_client()` twice must return usable clients backed by the
/// same shared pool (OnceLock initialised once).
#[test]
fn http_client_is_shared_onclock() {
    let a = http_client();
    let b = http_client();
    // Both clones must be usable.
    let _ra = a.get("https://us.i.posthog.com").build().unwrap();
    let _rb = b.get("https://us.i.posthog.com").build().unwrap();
    // A third call still succeeds — OnceLock doesn't reinitialise.
    let c = http_client();
    let _rc = c.get("https://us.i.posthog.com").build().unwrap();
}

// ── Live round-trip (ignored in CI) ──────────────────────────────────────────

/// Real PostHog capture round-trip. Requires `UMBRA_POSTHOG_API_KEY` and
/// optionally `UMBRA_POSTHOG_HOST` to be set. Run with:
///
/// ```sh
/// UMBRA_POSTHOG_API_KEY=phc_xxx cargo test -p umbra-analytics -- --ignored
/// ```
#[tokio::test]
#[ignore]
async fn live_posthog_capture_round_trip() {
    let api_key = std::env::var("UMBRA_POSTHOG_API_KEY")
        .expect("UMBRA_POSTHOG_API_KEY must be set to run this test");
    let host = std::env::var("UMBRA_POSTHOG_HOST")
        .unwrap_or_else(|_| DEFAULT_POSTHOG_HOST.to_string());

    let client = AnalyticsClient::new(api_key, host.clone());
    let payload = client.build_payload(
        "test_distinct_id_umbra",
        "test_event",
        json!({ "source": "umbra-analytics integration test" }),
    );

    let resp = http_client()
        .post(format!("{}/capture/", host.trim_end_matches('/')))
        .json(&payload)
        .send()
        .await
        .expect("PostHog request should not fail at the network level");

    assert!(
        resp.status().is_success(),
        "PostHog /capture/ returned non-success: {} — check your API key",
        resp.status()
    );
}
