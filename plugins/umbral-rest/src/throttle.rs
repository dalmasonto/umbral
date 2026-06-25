//! Throttling: how *often* may the caller hit this resource?
//!
//! Authentication ([`crate::auth`]) identifies the caller, permission
//! ([`crate::permission`]) decides what they may do — throttling decides
//! how fast. It's the third request-time gate (after auth and
//! permission), run **after** auth
//! resolves but **before** the handler: a request that passes auth and
//! permission can still be rejected with **429 Too Many Requests** if the
//! caller is over their rate.
//!
//! ## The contract
//!
//! A throttle implements [`Throttle`]: `check(&ThrottleContext) -> Result<(),
//! ThrottleDenied>`. `Ok(())` lets the request proceed; `Err(ThrottleDenied)`
//! short-circuits it into a 429 with a `Retry-After` header. The check is
//! synchronous — every built-in just hits an in-memory
//! [`RateLimiter`](umbral::ratelimit::RateLimiter), no DB round-trip.
//!
//! Throttles are **opt-in**. A `RestPlugin` with none configured imposes
//! no limits (existing APIs don't suddenly start 429-ing). Add them with
//! [`RestPlugin::default_throttle`](crate::RestPlugin::default_throttle)
//! (applies to every resource) or
//! [`ResourceConfig::throttle`](crate::ResourceConfig::throttle) (one
//! table). Multiple throttles **stack**: all must pass, and the FIRST to
//! deny wins.
//!
//! ## Built-ins
//!
//! - [`AnonRateThrottle`] — limits only **anonymous** requests, keyed by
//!   client IP. Authenticated requests pass through untouched.
//! - [`UserRateThrottle`] — limits only **authenticated** requests, keyed
//!   by user id. Anonymous requests pass through.
//! - [`ScopedRateThrottle`] — limits requests for a named scope, keyed by
//!   `scope` + (user id when authenticated, else IP).
//!
//! Custom throttles — per-org quotas, burst-vs-sustained tiers — implement
//! the trait directly and attach the same way.

use std::time::Duration;

use umbral::ratelimit::{Rate, RateLimiter};

use crate::auth::Identity;

/// What the dispatch hands a throttle on every request.
///
/// The throttle reads whichever fields it keys on: [`AnonRateThrottle`]
/// uses `client_ip`, [`UserRateThrottle`] uses `identity`,
/// [`ScopedRateThrottle`] uses `scope` plus one of the two.
#[derive(Debug, Clone, Copy)]
pub struct ThrottleContext<'a> {
    /// Whoever auth resolved. `None` is anonymous.
    pub identity: Option<&'a Identity>,
    /// The caller's IP, as resolved from proxy headers
    /// (`X-Forwarded-For` / `X-Real-IP`). `None` when unresolvable.
    pub client_ip: Option<&'a str>,
    /// The throttle scope — the dispatch passes the resource/action
    /// label (e.g. `"post:list"`). [`ScopedRateThrottle`] only acts when
    /// this matches its configured scope; the others ignore it.
    pub scope: &'a str,
}

/// A throttle denial. Carries the `retry_after` hint the dispatch turns
/// into a `Retry-After` header (seconds, rounded up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleDenied {
    /// Time until a slot frees for this caller. `None` when the limiter
    /// couldn't compute one (the dispatch then omits `Retry-After`).
    pub retry_after: Option<Duration>,
}

/// The throttle contract. `Ok(())` allows; `Err(ThrottleDenied)` →
/// 429. Synchronous because the built-ins only touch an in-memory
/// counter — same shape as [`Permission`](crate::permission::Permission).
pub trait Throttle: Send + Sync + 'static {
    /// Decide whether this request is within rate. A throttle that
    /// doesn't apply to the request (an [`AnonRateThrottle`] seeing an
    /// authenticated caller) returns `Ok(())` — a no-op pass.
    fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied>;
}

/// Map a [`RateDecision`](umbral::ratelimit::RateDecision) to the throttle
/// result. Allowed → `Ok`; denied → `Err` carrying the retry hint.
fn decide(limiter: &RateLimiter, key: &str) -> Result<(), ThrottleDenied> {
    let d = limiter.check(key);
    if d.allowed {
        Ok(())
    } else {
        Err(ThrottleDenied {
            retry_after: d.retry_after,
        })
    }
}

// =========================================================================
// AnonRateThrottle — anonymous only, keyed by IP.
// =========================================================================

/// Limit **anonymous** requests, keyed by client IP. Authenticated
/// requests are a no-op pass (use [`UserRateThrottle`] for those).
///
/// Limits anonymous requests by IP; configure with a rate string like
/// `"100/hour"`.
///
/// ```ignore
/// RestPlugin::default().default_throttle(AnonRateThrottle::new("100/hour"))
/// ```
pub struct AnonRateThrottle {
    limiter: RateLimiter,
}

impl AnonRateThrottle {
    /// Build from a rate string (`"100/hour"`, `"10/min"`, …).
    ///
    /// # Panics
    /// Panics if `rate` doesn't parse — a malformed rate is always a
    /// configuration bug, surfaced loudly at startup rather than silently
    /// disabling the limit. Use [`Self::try_new`] to handle the error.
    pub fn new(rate: &str) -> Self {
        Self::try_new(rate).unwrap_or_else(|e| panic!("AnonRateThrottle::new({rate:?}): {e}"))
    }

    /// Fallible constructor — returns the parse error instead of
    /// panicking.
    pub fn try_new(rate: &str) -> Result<Self, String> {
        Ok(Self {
            limiter: RateLimiter::new(Rate::parse(rate)?),
        })
    }
}

impl Throttle for AnonRateThrottle {
    fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied> {
        // Authenticated callers are out of scope for this throttle.
        if ctx.identity.is_some() {
            return Ok(());
        }
        // Key by IP. When the IP can't be resolved every un-proxied
        // caller shares one bucket — the safe side: it limits, never opens
        // a hole (mirrors umbral-auth's client_ip fallback).
        let ip = ctx.client_ip.unwrap_or("unknown");
        decide(&self.limiter, ip)
    }
}

// =========================================================================
// UserRateThrottle — authenticated only, keyed by user id.
// =========================================================================

/// Limit **authenticated** requests, keyed by the user id from
/// [`Identity`]. Anonymous requests are a no-op pass (use
/// [`AnonRateThrottle`] for those).
///
/// Limits authenticated requests by user id; configure with a rate string
/// like `"1000/day"`.
///
/// ```ignore
/// RestPlugin::default().default_throttle(UserRateThrottle::new("1000/day"))
/// ```
pub struct UserRateThrottle {
    limiter: RateLimiter,
}

impl UserRateThrottle {
    /// Build from a rate string. See [`AnonRateThrottle::new`] for the
    /// panic-on-bad-rate contract.
    pub fn new(rate: &str) -> Self {
        Self::try_new(rate).unwrap_or_else(|e| panic!("UserRateThrottle::new({rate:?}): {e}"))
    }

    /// Fallible constructor.
    pub fn try_new(rate: &str) -> Result<Self, String> {
        Ok(Self {
            limiter: RateLimiter::new(Rate::parse(rate)?),
        })
    }
}

impl Throttle for UserRateThrottle {
    fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied> {
        // Anonymous callers are out of scope for this throttle.
        let Some(id) = ctx.identity else {
            return Ok(());
        };
        // Key by user id, namespaced so it can never collide with an IP
        // bucket if a future change shares a limiter.
        let key = format!("user:{}", id.user_id);
        decide(&self.limiter, &key)
    }
}

// =========================================================================
// ScopedRateThrottle — a named scope, keyed by scope + user/IP.
// =========================================================================

/// Limit requests for a named scope, keyed by `scope` + the caller
/// (user id when authenticated, else IP). Applies to **both** anonymous
/// and authenticated callers, but only when the request's
/// [`ThrottleContext::scope`] matches the configured `scope`.
///
/// Limits requests for a named scope (e.g. `"uploads"`). The scope is the
/// resource/action
/// label the dispatch passes; attach the throttle to the resource(s) you
/// want it to govern.
///
/// ```ignore
/// // Only the `upload` table's requests count against this 10/min bucket.
/// RestPlugin::default().resource(
///     ResourceConfig::new("upload").throttle(ScopedRateThrottle::new("10/min", "uploads"))
/// )
/// ```
pub struct ScopedRateThrottle {
    limiter: RateLimiter,
    scope: String,
}

impl ScopedRateThrottle {
    /// Build from a rate string and the scope name this throttle
    /// governs. See [`AnonRateThrottle::new`] for the panic-on-bad-rate
    /// contract.
    pub fn new(rate: &str, scope: &str) -> Self {
        Self::try_new(rate, scope)
            .unwrap_or_else(|e| panic!("ScopedRateThrottle::new({rate:?}, {scope:?}): {e}"))
    }

    /// Fallible constructor.
    pub fn try_new(rate: &str, scope: &str) -> Result<Self, String> {
        Ok(Self {
            limiter: RateLimiter::new(Rate::parse(rate)?),
            scope: scope.to_string(),
        })
    }

    /// The scope this throttle governs.
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

impl Throttle for ScopedRateThrottle {
    fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied> {
        // Only act on requests whose scope matches. The dispatch passes a
        // `resource:action` label; this throttle ignores everything else,
        // so a plugin-wide `default_throttle(ScopedRateThrottle)` would
        // only bite the named scope.
        if ctx.scope != self.scope {
            return Ok(());
        }
        // Key by scope + caller identity (user id, else IP) so the same
        // user's quota is shared across the scope but separate from other
        // users / IPs.
        let who = match ctx.identity {
            Some(id) => format!("user:{}", id.user_id),
            None => format!("ip:{}", ctx.client_ip.unwrap_or("unknown")),
        };
        let key = format!("{}:{who}", self.scope);
        decide(&self.limiter, &key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anon_ctx<'a>(ip: &'a str, scope: &'a str) -> ThrottleContext<'a> {
        ThrottleContext {
            identity: None,
            client_ip: Some(ip),
            scope,
        }
    }

    fn user_ctx<'a>(id: &'a Identity, scope: &'a str) -> ThrottleContext<'a> {
        ThrottleContext {
            identity: Some(id),
            client_ip: Some("10.0.0.1"),
            scope,
        }
    }

    #[test]
    fn anon_throttle_limits_anonymous_by_ip() {
        let t = AnonRateThrottle::new("2/min");
        assert!(t.check(&anon_ctx("1.1.1.1", "post:list")).is_ok());
        assert!(t.check(&anon_ctx("1.1.1.1", "post:list")).is_ok());
        // 3rd from the same IP → denied with a retry hint.
        let denied = t.check(&anon_ctx("1.1.1.1", "post:list"));
        assert!(denied.is_err());
        assert!(denied.unwrap_err().retry_after.is_some());
        // A different IP has its own bucket.
        assert!(t.check(&anon_ctx("2.2.2.2", "post:list")).is_ok());
    }

    #[test]
    fn anon_throttle_passes_authenticated() {
        let t = AnonRateThrottle::new("1/min");
        let alice = Identity::user(1);
        // Authenticated callers always pass, no matter how many times.
        for _ in 0..5 {
            assert!(t.check(&user_ctx(&alice, "post:list")).is_ok());
        }
    }

    #[test]
    fn user_throttle_limits_authenticated_per_user() {
        let t = UserRateThrottle::new("2/min");
        let alice = Identity::user(1);
        let bob = Identity::user(2);
        assert!(t.check(&user_ctx(&alice, "post:list")).is_ok());
        assert!(t.check(&user_ctx(&alice, "post:list")).is_ok());
        // Alice's 3rd → denied.
        assert!(t.check(&user_ctx(&alice, "post:list")).is_err());
        // Bob is independent.
        assert!(t.check(&user_ctx(&bob, "post:list")).is_ok());
    }

    #[test]
    fn user_throttle_passes_anonymous() {
        let t = UserRateThrottle::new("1/min");
        for _ in 0..5 {
            assert!(t.check(&anon_ctx("1.1.1.1", "post:list")).is_ok());
        }
    }

    #[test]
    fn scoped_throttle_keys_by_scope() {
        let t = ScopedRateThrottle::new("1/min", "uploads");
        // A request OUTSIDE the scope is never throttled.
        assert!(t.check(&anon_ctx("1.1.1.1", "post:list")).is_ok());
        assert!(t.check(&anon_ctx("1.1.1.1", "post:list")).is_ok());
        // In-scope: first passes, second denied (1/min, same IP).
        assert!(t.check(&anon_ctx("1.1.1.1", "uploads")).is_ok());
        assert!(t.check(&anon_ctx("1.1.1.1", "uploads")).is_err());
        // A different caller in the same scope is independent.
        assert!(t.check(&anon_ctx("2.2.2.2", "uploads")).is_ok());
    }

    #[test]
    fn scoped_throttle_separates_user_from_ip() {
        let t = ScopedRateThrottle::new("1/min", "uploads");
        let alice = Identity::user(1);
        assert!(t.check(&user_ctx(&alice, "uploads")).is_ok());
        // Alice's second in-scope request → denied.
        assert!(t.check(&user_ctx(&alice, "uploads")).is_err());
        // An anonymous caller in the same scope has a separate bucket.
        assert!(t.check(&anon_ctx("9.9.9.9", "uploads")).is_ok());
    }
}
