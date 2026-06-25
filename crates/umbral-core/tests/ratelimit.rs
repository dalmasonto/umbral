//! Integration tests for the core sliding-window [`RateLimiter`] primitive
//! (`umbral_core::ratelimit`). Drives the clock-injectable `check_at` so the
//! window behaviour is deterministic — no `sleep`, no flake.

use std::time::{Duration, Instant};

use umbral_core::ratelimit::{Rate, RateLimiter};

// --- Rate::parse -------------------------------------------------------

#[test]
fn parse_handles_every_period_token() {
    let cases = [
        ("1/sec", Duration::from_secs(1)),
        ("1/s", Duration::from_secs(1)),
        ("1/second", Duration::from_secs(1)),
        ("1/min", Duration::from_secs(60)),
        ("1/m", Duration::from_secs(60)),
        ("1/minute", Duration::from_secs(60)),
        ("1/hour", Duration::from_secs(3600)),
        ("1/h", Duration::from_secs(3600)),
        ("1/day", Duration::from_secs(86_400)),
        ("1/d", Duration::from_secs(86_400)),
    ];
    for (s, period) in cases {
        let r = Rate::parse(s).unwrap_or_else(|e| panic!("parse {s}: {e}"));
        assert_eq!(r.period, period, "period for {s}");
        assert_eq!(r.num, 1, "num for {s}");
    }
    // Case-insensitive period token.
    assert_eq!(Rate::parse("5/HOUR").unwrap().period, Duration::from_secs(3600));
    // Bare number → per-second shorthand.
    assert_eq!(Rate::parse("9").unwrap().period, Duration::from_secs(1));
    assert_eq!(Rate::parse("9").unwrap().num, 9);
}

#[test]
fn parse_rejects_bad_strings() {
    for bad in ["", "   ", "oops", "10/fortnight", "0/sec", "abc/min", "/min", "12/"] {
        assert!(Rate::parse(bad).is_err(), "{bad:?} should be a parse error");
    }
}

// --- RateLimiter -------------------------------------------------------

#[test]
fn third_check_in_window_is_denied_with_retry_after() {
    let limiter = RateLimiter::new(Rate::parse("2/min").unwrap());
    let t0 = Instant::now();

    let d1 = limiter.check_at("k", t0);
    assert!(d1.allowed);
    assert_eq!(d1.limit, 2);
    assert_eq!(d1.remaining, 1);

    let d2 = limiter.check_at("k", t0 + Duration::from_secs(10));
    assert!(d2.allowed);
    assert_eq!(d2.remaining, 0);

    // Third within the 60s window is over the limit.
    let d3 = limiter.check_at("k", t0 + Duration::from_secs(20));
    assert!(!d3.allowed);
    assert_eq!(d3.remaining, 0);
    // A slot frees 60s after the FIRST hit → 40s from t0+20s.
    assert_eq!(d3.retry_after, Some(Duration::from_secs(40)));
}

#[test]
fn distinct_keys_are_independent() {
    let limiter = RateLimiter::new(Rate::parse("1/min").unwrap());
    let t0 = Instant::now();

    assert!(limiter.check_at("alice", t0).allowed);
    // A different key is unaffected by alice's bucket being full.
    assert!(limiter.check_at("bob", t0).allowed);
    // alice is now over her 1/min.
    assert!(!limiter.check_at("alice", t0).allowed);
    // bob still independent.
    assert!(!limiter.check_at("bob", t0).allowed);
}

#[test]
fn allowed_again_after_window_elapses() {
    let limiter = RateLimiter::new(Rate::parse("1/min").unwrap());
    let t0 = Instant::now();

    assert!(limiter.check_at("k", t0).allowed);
    // Still inside the window → denied.
    assert!(!limiter.check_at("k", t0 + Duration::from_secs(59)).allowed);
    // One second past the window → the original hit aged out, allowed.
    assert!(limiter.check_at("k", t0 + Duration::from_secs(61)).allowed);
}

#[test]
fn remaining_counts_down_then_resets() {
    let limiter = RateLimiter::new(Rate::parse("3/min").unwrap());
    let t0 = Instant::now();

    assert_eq!(limiter.check_at("k", t0).remaining, 2);
    assert_eq!(limiter.check_at("k", t0).remaining, 1);
    assert_eq!(limiter.check_at("k", t0).remaining, 0);
    // Over the limit.
    let over = limiter.check_at("k", t0);
    assert!(!over.allowed);
    assert_eq!(over.remaining, 0);
    // After the window the count resets and `remaining` is full again.
    let after = limiter.check_at("k", t0 + Duration::from_secs(61));
    assert!(after.allowed);
    assert_eq!(after.remaining, 2);
}
