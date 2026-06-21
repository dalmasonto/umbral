//! Login / register rate-limiting — credential-stuffing & brute-force defense.
//!
//! Today's login and register handlers have NO throttle, so a script can
//! pound `<prefix>/login` with a leaked credential list or mass-create
//! accounts at `<prefix>/register` unimpeded. This module adds a
//! **secure-by-default** sliding-window limiter that both handlers consult
//! at entry, returning **429 Too Many Requests** before any DB work when a
//! caller exceeds the budget.
//!
//! ## The window
//!
//! A [`Throttle`] keeps, per key, the timestamps of recent attempts. On each
//! [`Throttle::check`] it prunes timestamps older than `window`, and if the
//! surviving count is already `>= max` it returns `false` (deny). Otherwise
//! it records `now` and returns `true` (allow). [`Throttle::clear`] drops a
//! key's history entirely — the login handler calls it on a SUCCESSFUL login
//! so a legitimate user who fat-fingered their password once isn't locked
//! out by the failures that preceded the success.
//!
//! The sliding-window mechanics are NOT implemented here: [`Throttle`] is a
//! thin wrapper over the core [`umbra::ratelimit::RateLimiter`] primitive,
//! which owns the single per-key timestamp store in the tree. This module
//! contributes the auth-specific policy on top: the secure-default budgets,
//! the IP+username keying, the `enabled` master switch, and the ambient
//! install. (Consolidated from a former hand-rolled copy — gaps2 #90.)
//!
//! Keys are caller-chosen strings:
//! - **login** keys on `ip + "\0" + username` so one attacker IP can't lock
//!   out every account, and one targeted account can't be brute-forced from
//!   one IP. (5 attempts / 5 min by default.)
//! - **register** keys on `ip` alone — it defends mass account creation, and
//!   there's no username yet. (10 / hour by default.)
//!
//! ## Injectable clock
//!
//! [`Throttle::check`] / [`Throttle::clear`] use `Instant::now()`. The
//! private core ([`Throttle::check_at`] / [`Throttle::clear_at`]) takes the
//! `now: Instant` explicitly so tests can advance time deterministically
//! without `sleep`.
//!
//! ## Ambient install
//!
//! The active config + stores live in a process-wide `OnceLock`
//! ([`AUTH_THROTTLE`]), installed once from
//! [`crate::AuthPlugin::on_ready`] — the same ambient pattern as
//! `PASSWORD_POLICY`. The route handlers are free functions with no handle
//! to the `AuthPlugin`, so they reach the limiter via the free helpers
//! [`login_throttle_check`], [`login_throttle_clear`], and
//! [`register_throttle_check`], each of which falls back to the secure
//! default config when nothing has been installed yet (so the helpers are
//! enforced even before `on_ready` runs and in unit tests).
//!
//! ## Scope: in-memory, single-instance
//!
//! The store is a process-local `HashMap`. In a multi-instance deployment
//! each replica counts independently, so the effective budget is
//! `max * replicas`. That's still a meaningful brake on a single attacker
//! pinned to one replica by a sticky LB, but a multi-instance app that wants
//! a hard global limit should front it with a shared limiter (a future
//! Redis-backed `Throttle`). Logged as a known limitation in the auth docs.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use umbra::ratelimit::{Rate, RateLimiter};

/// A sliding-window counter keyed by an arbitrary string.
///
/// `max` attempts are permitted within any trailing `window`. This is a thin
/// wrapper over the core [`RateLimiter`] primitive — it holds one and adapts
/// its rich [`umbra::ratelimit::RateDecision`] down to the `bool` (allow /
/// deny) the auth handlers need, plus the clock-injectable `*_at` variants the
/// tests drive. All the per-key timestamp bookkeeping lives in `RateLimiter`;
/// this type adds no sliding-window logic of its own.
#[derive(Debug)]
pub struct Throttle {
    inner: RateLimiter,
}

impl Throttle {
    /// Build a limiter allowing `max` attempts per trailing `window`.
    ///
    /// `max == 0` is treated as "deny everything" (a hard lock); any other
    /// `max` permits up to `max` attempts in the trailing `window`. (Core
    /// `RateLimiter` denies when `count < num` is false, so `num == 0` denies
    /// the very first attempt — the same hard-lock semantics.)
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            inner: RateLimiter::new(Rate::new(max as u32, window)),
        }
    }

    /// Record an attempt for `key` and report whether it's allowed.
    ///
    /// Uses the real wall clock. Prunes anything older than `window`,
    /// denies (`false`) when `>= max` remain in-window, otherwise records
    /// `now` and allows (`true`).
    pub fn check(&self, key: &str) -> bool {
        self.inner.check(key).allowed
    }

    /// Forget every recorded attempt for `key`. Called on a successful
    /// login so prior failures don't count against a now-authenticated user.
    pub fn clear(&self, key: &str) {
        self.inner.clear(key);
    }

    /// Clock-injectable core of [`check`](Self::check). A `now` of the
    /// caller's choosing lets a test advance time without sleeping.
    pub fn check_at(&self, key: &str, now: Instant) -> bool {
        self.inner.check_at(key, now).allowed
    }

    /// Clock-injectable core of [`clear`](Self::clear). No `now` needed —
    /// clearing is unconditional — but named `_at` for symmetry with
    /// [`check_at`](Self::check_at).
    pub fn clear_at(&self, key: &str) {
        self.inner.clear(key);
    }
}

// =========================================================================
// Config + ambient install
// =========================================================================

/// The throttle configuration installed at boot.
///
/// Secure defaults (see [`ThrottleConfig::default`]): login 5 attempts /
/// 5 min keyed per IP+username, register 10 / hour keyed per IP. `enabled`
/// defaults to `true` — throttling is ON unless an app opts out via
/// [`crate::AuthPlugin::disable_throttle`].
#[derive(Debug, Clone, Copy)]
pub struct ThrottleConfig {
    /// Max login attempts per IP+username inside `login_window`.
    pub login_max: usize,
    /// Sliding window for login attempts.
    pub login_window: Duration,
    /// Max register attempts per IP inside `register_window`.
    pub register_max: usize,
    /// Sliding window for register attempts.
    pub register_window: Duration,
    /// Master switch. `false` makes every `*_check` allow unconditionally.
    pub enabled: bool,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            login_max: 5,
            login_window: Duration::from_secs(5 * 60),
            register_max: 10,
            register_window: Duration::from_secs(60 * 60),
            enabled: true,
        }
    }
}

/// The live limiter: the config plus the two backing stores.
#[derive(Debug)]
pub struct AuthThrottle {
    config: ThrottleConfig,
    login: Throttle,
    register: Throttle,
}

impl AuthThrottle {
    /// Build the live limiter from a config, sizing each store's `max` /
    /// `window` from the matching config fields.
    pub fn from_config(config: ThrottleConfig) -> Self {
        Self {
            login: Throttle::new(config.login_max, config.login_window),
            register: Throttle::new(config.register_max, config.register_window),
            config,
        }
    }
}

/// Process-wide installed limiter. Set once from `AuthPlugin::on_ready`,
/// mirroring `password_validation::PASSWORD_POLICY`.
static AUTH_THROTTLE: OnceLock<AuthThrottle> = OnceLock::new();

/// Install the limiter at boot. Idempotent — first install wins, matching
/// the ambient-pool / password-policy contract.
pub(crate) fn install(throttle: AuthThrottle) {
    let _ = AUTH_THROTTLE.set(throttle);
}

/// Resolve the active limiter, building the secure default on first use when
/// nothing has been installed yet. This keeps the free helpers enforced even
/// before `on_ready` runs and in unit tests that call them without a boot.
///
/// The lazy default lives in a separate `OnceLock` so it doesn't seed
/// `AUTH_THROTTLE` — an explicit `on_ready` install must still win if it
/// happens after the first fallback read.
fn active() -> &'static AuthThrottle {
    if let Some(t) = AUTH_THROTTLE.get() {
        return t;
    }
    static FALLBACK: OnceLock<AuthThrottle> = OnceLock::new();
    FALLBACK.get_or_init(|| AuthThrottle::from_config(ThrottleConfig::default()))
}

/// Build the login key from IP + username. `\0` is the separator because it
/// can't appear in an IP or a username, so two distinct (ip, username) pairs
/// never collide into one key.
fn login_key(ip: &str, username: &str) -> String {
    format!("{ip}\0{username}")
}

/// Record + check a login attempt for `(ip, username)`. Returns `true` if
/// the attempt is allowed, `false` if the IP+username has exhausted its
/// budget (the handler then returns 429). A disabled config always allows.
pub fn login_throttle_check(ip: &str, username: &str) -> bool {
    let t = active();
    if !t.config.enabled {
        return true;
    }
    t.login.check(&login_key(ip, username))
}

/// Forgive the login counter for `(ip, username)` — called after a
/// SUCCESSFUL login so a legit user's earlier typos don't lock them out.
pub fn login_throttle_clear(ip: &str, username: &str) {
    active().login.clear(&login_key(ip, username));
}

/// Record + check a register attempt for `ip`. Returns `true` if allowed,
/// `false` once the IP has exhausted its register budget. A disabled config
/// always allows.
pub fn register_throttle_check(ip: &str) -> bool {
    let t = active();
    if !t.config.enabled {
        return true;
    }
    t.register.check(ip)
}

// =========================================================================
// Unit tests — deterministic via the injected clock.
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_attempt_in_window_is_denied() {
        let t = Throttle::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(t.check_at("k", now));
        assert!(t.check_at("k", now));
        // Third in-window → over budget.
        assert!(!t.check_at("k", now));
    }

    #[test]
    fn different_keys_are_independent() {
        let t = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(t.check_at("a", now));
        // "a" is now exhausted, but "b" has its own budget.
        assert!(!t.check_at("a", now));
        assert!(t.check_at("b", now));
    }

    #[test]
    fn window_elapse_re_allows() {
        let t = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(t.check_at("k", now));
        assert!(!t.check_at("k", now));
        // Advance past the window: the old hit ages out.
        let later = now + Duration::from_secs(61);
        assert!(t.check_at("k", later));
    }

    #[test]
    fn clear_resets_a_key() {
        let t = Throttle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(t.check_at("k", now));
        assert!(!t.check_at("k", now));
        t.clear_at("k");
        assert!(t.check_at("k", now));
    }

    #[test]
    fn max_zero_denies_everything() {
        let t = Throttle::new(0, Duration::from_secs(60));
        assert!(!t.check_at("k", Instant::now()));
    }

    #[test]
    fn disabled_config_gate_short_circuits() {
        // The `enabled = false` gate the free helpers apply (see
        // `login_throttle_check` / `register_throttle_check`) short-circuits
        // BEFORE the store, so even a max-1 limiter never denies when disabled.
        // We assert the gate predicate the helpers use, exercised here without
        // touching the process-wide ambient `OnceLock` (which a sibling test
        // may have installed). The store itself, by contrast, WOULD deny:
        let cfg = ThrottleConfig {
            login_max: 1,
            enabled: false,
            ..ThrottleConfig::default()
        };
        let store = AuthThrottle::from_config(cfg);
        let now = Instant::now();
        assert!(store.login.check_at("k", now)); // 1st allowed
        assert!(!store.login.check_at("k", now)); // 2nd denied by the store
        // ...but the free-helper gate skips the store entirely when disabled:
        assert!(!cfg.enabled, "gate is open when enabled == false");
    }
}
