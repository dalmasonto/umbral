//! End-to-end coverage for the secure-by-default login / register throttle
//! (credential-stuffing & brute-force defense).
//!
//! Two layers, mirroring `password_validation.rs`:
//!
//! 1. **Window mechanics** — a `Throttle` with an injected clock: the
//!    (max+1)th in-window attempt is denied, distinct keys are independent,
//!    the window elapsing re-allows, and `clear` resets a key.
//! 2. **Free-helper level** — the SAME `login_throttle_check` /
//!    `login_throttle_clear` / `register_throttle_check` the route handlers
//!    call. With no plugin booted these fall back to the secure default
//!    config (login 5 / 5 min, register 10 / hour), so we drive them straight
//!    and assert the 6th login is denied and a `clear` forgives the counter.
//!
//! The free helpers read a process-wide ambient store shared across every
//! test in this binary, so each test uses a UNIQUE (ip, username) key to stay
//! independent — exactly how production keys per real IP + username.
//!
//! See `plugins/umbra-auth/src/throttle.rs` for the surface and `CLAUDE.md`
//! "secure-by-default" for why throttling is on with no opt-in.

use std::time::{Duration, Instant};

use umbra_auth::{
    Throttle, login_throttle_check, login_throttle_clear, register_throttle_check,
};

// --------------------------------------------------------------------- //
// Window mechanics — deterministic via the injected clock.              //
// --------------------------------------------------------------------- //

#[test]
fn over_budget_attempt_is_denied() {
    let t = Throttle::new(2, Duration::from_secs(60));
    let now = Instant::now();
    assert!(t.check_at("k", now), "1st attempt allowed");
    assert!(t.check_at("k", now), "2nd attempt allowed");
    assert!(!t.check_at("k", now), "3rd in-window attempt denied (max=2)");
}

#[test]
fn distinct_keys_have_independent_budgets() {
    let t = Throttle::new(1, Duration::from_secs(60));
    let now = Instant::now();
    assert!(t.check_at("ip-a|alice", now));
    assert!(!t.check_at("ip-a|alice", now), "alice exhausted");
    assert!(
        t.check_at("ip-b|bob", now),
        "bob has his own budget; one user's lockout never bleeds onto another"
    );
}

#[test]
fn window_elapsing_re_allows_a_key() {
    let t = Throttle::new(1, Duration::from_secs(60));
    let now = Instant::now();
    assert!(t.check_at("k", now));
    assert!(!t.check_at("k", now), "second within the window denied");
    let later = now + Duration::from_secs(61);
    assert!(
        t.check_at("k", later),
        "after the window elapses the stale hit is pruned and the key is allowed again"
    );
}

#[test]
fn clear_resets_a_key() {
    let t = Throttle::new(1, Duration::from_secs(60));
    let now = Instant::now();
    assert!(t.check_at("k", now));
    assert!(!t.check_at("k", now));
    t.clear_at("k");
    assert!(t.check_at("k", now), "clear forgives the counter");
}

// --------------------------------------------------------------------- //
// Free-helper level — the exact functions the route handlers call.      //
// No boot: helpers fall back to the secure default config (login 5/5m). //
// --------------------------------------------------------------------- //

#[test]
fn login_helper_denies_after_default_budget_then_clear_forgives() {
    // Unique key so this test is independent of every other in the binary.
    let ip = "203.0.113.7";
    let user = "throttle_login_victim";

    // The secure default is 5 login attempts / 5 min per (ip, username).
    for i in 0..5 {
        assert!(
            login_throttle_check(ip, user),
            "attempt {i} within the budget should be allowed"
        );
    }
    // 6th rapid attempt for the same (ip, user) → over budget → handler 429s.
    assert!(
        !login_throttle_check(ip, user),
        "the 6th rapid login attempt must be denied (defends credential stuffing)"
    );

    // A successful login clears the counter; a subsequent attempt isn't
    // throttled — a legit user who mistyped a few times isn't locked out.
    login_throttle_clear(ip, user);
    assert!(
        login_throttle_check(ip, user),
        "after a successful login clears the counter, the next attempt is allowed again"
    );
}

#[test]
fn login_helper_keys_per_ip_and_username() {
    let ip = "198.51.100.42";
    // Exhaust one username from this IP.
    for _ in 0..5 {
        assert!(login_throttle_check(ip, "throttle_keying_a"));
    }
    assert!(!login_throttle_check(ip, "throttle_keying_a"));
    // A DIFFERENT username from the same IP is unaffected: an attacker
    // hammering one account can't lock out another from the same IP.
    assert!(
        login_throttle_check(ip, "throttle_keying_b"),
        "a different username from the same IP keeps its own budget"
    );
}

#[test]
fn register_helper_denies_after_default_budget() {
    // Per-IP only. Secure default is 10 / hour.
    let ip = "192.0.2.99";
    for i in 0..10 {
        assert!(
            register_throttle_check(ip),
            "register attempt {i} within budget allowed"
        );
    }
    assert!(
        !register_throttle_check(ip),
        "the 11th rapid register from one IP must be denied (defends mass signup)"
    );
}
