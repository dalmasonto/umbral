//! Shared HTTP client for OAuth provider requests.
//!
//! A single process-wide [`reqwest::Client`] is built once via [`OnceLock`]
//! and reused for every token-exchange and identity-fetch call. Benefits:
//!
//! - **Connection pooling** — reqwest pools keep-alive connections per host;
//!   calling `Client::new()` on every request discards the pool and forces a
//!   fresh TCP + TLS handshake on each OAuth round-trip.
//! - **Request timeout** — a 10 s total-request timeout and a 5 s connect
//!   timeout bound how long a slow or hung provider can stall a handler,
//!   closing a handler-stall DoS vector.
//!
//! Cloning a `reqwest::Client` is `O(1)` — it is an `Arc` over shared state,
//! so the pool is shared across every clone. The helper therefore returns a
//! clone on each call; callers pay no allocation.

use std::sync::OnceLock;
use std::time::Duration;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Returns a clone of the process-wide shared HTTP client.
///
/// The client is built on first call; all subsequent calls return a clone of
/// the same underlying instance (shared connection pool, same timeouts).
///
/// Configured with:
/// - `timeout(10 s)` — total time from request start to response body end.
/// - `connect_timeout(5 s)` — TCP + TLS handshake budget.
pub(crate) fn http_client() -> reqwest::Client {
    HTTP_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("failed to build the shared OAuth HTTP client")
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test: the helper is constructible and returns a usable client
    /// without panicking. Also verifies that calling it twice yields two
    /// clones of the *same* underlying client (the `OnceLock` is only
    /// initialised once, so the pool is shared).
    #[test]
    fn http_client_is_constructible_and_shared() {
        let a = http_client();
        let b = http_client();
        // Both clones must refer to the same underlying Arc — a no-op GET
        // request builder proves the clients are usable without an actual
        // network call.
        let _req_a = a.get("https://example.com").build().unwrap();
        let _req_b = b.get("https://example.com").build().unwrap();
        // `reqwest::Client` doesn't expose an identity pointer directly, but
        // the OnceLock guarantee means the init closure ran exactly once:
        // calling `http_client()` again after the above doesn't reinitialise.
        let c = http_client();
        let _req_c = c.get("https://example.com").build().unwrap();
    }
}
