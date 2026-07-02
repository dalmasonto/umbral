//! A dependency-light, in-memory **sliding-window rate limiter**.
//!
//! The single sliding-window limiter in the tree: it backs umbral-rest's
//! API throttles ([`umbral_rest::throttle`]) AND umbral-auth's
//! login/register brute-force throttle (`plugins/umbral-auth/src/throttle.rs`,
//! consolidated onto this primitive — see the note below). It
//! tracks per-key timestamps in a `Mutex<HashMap<String, VecDeque<Instant>>>`
//! and answers one question: *is this key under its rate right now?*
//!
//! ```ignore
//! use std::time::Duration;
//! use umbral::ratelimit::{Rate, RateLimiter};
//!
//! let limiter = RateLimiter::new(Rate::parse("100/hour").unwrap());
//! let decision = limiter.check("203.0.113.7");
//! if !decision.allowed {
//!     // 429; tell the client when to come back
//!     let secs = decision.retry_after.map(|d| d.as_secs()).unwrap_or(0);
//! }
//! ```
//!
//! ## The window
//!
//! "Sliding window" means each `check` first prunes every recorded
//! timestamp older than `rate.period` from now, then counts what's left.
//! If the count is below `rate.num`, the call is allowed *and recorded*;
//! otherwise it's denied and the limiter computes `retry_after` as the
//! time until the oldest still-in-window entry ages out (the moment a
//! slot frees up). There's no fixed-window edge burst: the window moves
//! continuously with the clock.
//!
//! ## Scope and limits
//!
//! - **In-memory, single-process.** State lives in this process's heap.
//!   A multi-instance deployment behind a load balancer gives each
//!   replica its own counters; the effective limit is `num × replicas`.
//!   A Redis-backed store is the multi-instance follow-up (mirrors the
//!   same gap `umbral-auth`'s throttle has).
//! - **Unbounded key set.** The `HashMap` grows one entry per distinct
//!   key and entries are pruned lazily on next `check` of that key, never
//!   swept globally. For IP/user keys on a normal app this is bounded by
//!   the active client set; an adversarial key explosion is a known edge
//!   (the same shape `umbral-auth`'s throttle has) — a periodic sweep is a
//!   future hardening.
//!
//! ## Consolidated: `umbral-auth::throttle` adopts this primitive
//!
//! `umbral-auth` once shipped its own bespoke login/register throttle
//! (`plugins/umbral-auth/src/throttle.rs`) written before this primitive
//! existed, with a hand-rolled copy of the same sliding-window-per-key idea.
//! That duplicate is gone: `umbral-auth::throttle::Throttle` is now a thin
//! wrapper over [`RateLimiter`], so there's a single limiter implementation
//! in the tree. The "success forgives" path (clear a login counter after a
//! successful login) drove the [`RateLimiter::clear`] method added here.
//! Done in `planning/gaps2.md` (#90).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A rate: `num` events per `period`. Build by hand or parse the
/// `"<num>/<period>"` string with [`Rate::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Maximum number of events allowed within one `period`.
    pub num: u32,
    /// The sliding window length.
    pub period: Duration,
}

impl Rate {
    /// Construct directly from a count and a window.
    pub fn new(num: u32, period: Duration) -> Self {
        Self { num, period }
    }

    /// Parse a rate string: `"<num>/<period>"`.
    ///
    /// `num` is a positive integer; `period` is one of (case-insensitive):
    ///
    /// | period token | window |
    /// |---|---|
    /// | `sec`, `s`, `second` | 1 second |
    /// | `min`, `m`, `minute` | 60 seconds |
    /// | `hour`, `h` | 3600 seconds |
    /// | `day`, `d` | 86400 seconds |
    ///
    /// A bare number with no separator is also accepted as a per-second
    /// rate (the `"<num>"` shorthand), e.g. `"5"` ≡ `"5/sec"`. Anything
    /// else — empty string, non-numeric count, zero count, unknown period
    /// — returns `Err` with a short message.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use umbral_core::ratelimit::Rate;
    /// assert_eq!(Rate::parse("100/hour").unwrap().num, 100);
    /// assert_eq!(Rate::parse("10/min").unwrap().period, Duration::from_secs(60));
    /// assert!(Rate::parse("oops").is_err());
    /// ```
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty rate string".to_string());
        }
        let (num_part, period_part) = match s.split_once('/') {
            Some((n, p)) => (n.trim(), p.trim()),
            // Bare number → per-second (shorthand).
            None => (s, "sec"),
        };
        let num: u32 = num_part
            .parse()
            .map_err(|_| format!("invalid rate count `{num_part}` in `{s}`"))?;
        if num == 0 {
            return Err(format!("rate count must be positive in `{s}`"));
        }
        let period = match period_part.to_ascii_lowercase().as_str() {
            "sec" | "s" | "second" => Duration::from_secs(1),
            "min" | "m" | "minute" => Duration::from_secs(60),
            "hour" | "h" => Duration::from_secs(3600),
            "day" | "d" => Duration::from_secs(86_400),
            other => return Err(format!("unknown rate period `{other}` in `{s}`")),
        };
        Ok(Self { num, period })
    }
}

/// The verdict for one [`RateLimiter::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateDecision {
    /// `true` when the request is under the limit (and was recorded);
    /// `false` when it's over (and was NOT recorded).
    pub allowed: bool,
    /// On a denial, how long until a slot frees up — the time until the
    /// oldest in-window entry ages out. `None` when `allowed` is `true`.
    pub retry_after: Option<Duration>,
    /// The configured ceiling (`Rate::num`). Useful for an
    /// `X-RateLimit-Limit` header.
    pub limit: u32,
    /// How many requests remain in the current window AFTER this one.
    /// `0` on a denial.
    pub remaining: u32,
}

/// An in-memory sliding-window rate limiter, keyed by an arbitrary
/// string (IP, user id, scope-qualified key — the caller decides).
///
/// Cheap to clone the configured [`Rate`]; the shared counter map sits
/// behind a `Mutex` so a single `RateLimiter` can back many concurrent
/// requests. Wrap in an `Arc` to share across handlers.
#[derive(Debug)]
pub struct RateLimiter {
    rate: Rate,
    buckets: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// Build a limiter enforcing `rate`.
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// The configured rate.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Check (and, if allowed, record) one request for `key` against the
    /// configured rate, using the real wall clock.
    ///
    /// See [`Self::check_at`] for the deterministic, clock-injectable
    /// variant the tests drive.
    pub fn check(&self, key: &str) -> RateDecision {
        self.check_at(key, Instant::now())
    }

    /// Clock-injectable core: identical to [`Self::check`] but the caller
    /// supplies `now`. Private-ish (crate-visible) so deterministic tests
    /// can advance time without sleeping; production always routes through
    /// [`Self::check`] with `Instant::now()`.
    pub fn check_at(&self, key: &str, now: Instant) -> RateDecision {
        let window = self.rate.period;
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let entries = buckets.entry(key.to_string()).or_default();

        // Prune everything older than the window — the "sliding" step.
        // `now.checked_duration_since` guards against a clock that didn't
        // advance (or a stamp in the future); treat un-orderable stamps
        // as in-window (conservative: never silently drop a recent hit).
        while let Some(front) = entries.front() {
            match now.checked_duration_since(*front) {
                Some(age) if age >= window => {
                    entries.pop_front();
                }
                _ => break,
            }
        }

        let count = entries.len() as u32;
        if count < self.rate.num {
            entries.push_back(now);
            RateDecision {
                allowed: true,
                retry_after: None,
                limit: self.rate.num,
                remaining: self.rate.num - count - 1,
            }
        } else {
            // Over the limit. A slot frees when the OLDEST in-window entry
            // ages out: that's `window - (now - oldest)`. The prune above
            // guarantees the front is still within the window, so the
            // subtraction is non-negative; saturate to be safe.
            let retry_after = entries
                .front()
                .and_then(|oldest| now.checked_duration_since(*oldest))
                .map(|age| window.saturating_sub(age))
                .unwrap_or(window);
            RateDecision {
                allowed: false,
                retry_after: Some(retry_after),
                limit: self.rate.num,
                remaining: 0,
            }
        }
    }

    /// Forget every recorded request for `key`, resetting its window so the
    /// next [`check`](Self::check) starts from a clean budget.
    ///
    /// The "success forgives" primitive: a caller that wants a prior burst of
    /// denied attempts to stop counting after some positive outcome (e.g.
    /// umbral-auth clears the login counter on a SUCCESSFUL login so a user who
    /// fat-fingered their password isn't locked out) calls this to drop the
    /// key's history. A no-op if the key was never seen.
    pub fn clear(&self, key: &str) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        buckets.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_each_period() {
        assert_eq!(Rate::parse("1/sec").unwrap().period, Duration::from_secs(1));
        assert_eq!(Rate::parse("1/s").unwrap().period, Duration::from_secs(1));
        assert_eq!(
            Rate::parse("1/second").unwrap().period,
            Duration::from_secs(1)
        );
        assert_eq!(
            Rate::parse("1/min").unwrap().period,
            Duration::from_secs(60)
        );
        assert_eq!(
            Rate::parse("1/hour").unwrap().period,
            Duration::from_secs(3600)
        );
        assert_eq!(
            Rate::parse("1/day").unwrap().period,
            Duration::from_secs(86_400)
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Rate::parse("").is_err());
        assert!(Rate::parse("oops").is_err());
        assert!(Rate::parse("10/fortnight").is_err());
        assert!(Rate::parse("0/sec").is_err());
        assert!(Rate::parse("abc/min").is_err());
    }

    #[test]
    fn third_request_in_window_denied() {
        let limiter = RateLimiter::new(Rate::parse("2/min").unwrap());
        let t0 = Instant::now();
        let d1 = limiter.check_at("a", t0);
        assert!(d1.allowed);
        assert_eq!(d1.remaining, 1);
        let d2 = limiter.check_at("a", t0 + Duration::from_secs(1));
        assert!(d2.allowed);
        assert_eq!(d2.remaining, 0);
        let d3 = limiter.check_at("a", t0 + Duration::from_secs(2));
        assert!(!d3.allowed);
        assert!(d3.retry_after.is_some());
        // Slot frees 60s after the FIRST hit, i.e. 58s from t0+2s.
        assert_eq!(d3.retry_after.unwrap(), Duration::from_secs(58));
    }

    #[test]
    fn distinct_keys_are_independent() {
        let limiter = RateLimiter::new(Rate::parse("1/min").unwrap());
        let t0 = Instant::now();
        assert!(limiter.check_at("a", t0).allowed);
        // Key "b" has its own bucket — not affected by "a" being full.
        assert!(limiter.check_at("b", t0).allowed);
        // "a" is now over its 1/min.
        assert!(!limiter.check_at("a", t0).allowed);
    }

    #[test]
    fn allowed_again_after_window_elapses() {
        let limiter = RateLimiter::new(Rate::parse("1/min").unwrap());
        let t0 = Instant::now();
        assert!(limiter.check_at("a", t0).allowed);
        assert!(!limiter.check_at("a", t0 + Duration::from_secs(30)).allowed);
        // 61s later the original hit has aged out of the 60s window.
        assert!(limiter.check_at("a", t0 + Duration::from_secs(61)).allowed);
    }

    #[test]
    fn clear_forgets_a_key() {
        let limiter = RateLimiter::new(Rate::parse("1/min").unwrap());
        let t0 = Instant::now();
        assert!(limiter.check_at("a", t0).allowed);
        // Over budget within the window.
        assert!(!limiter.check_at("a", t0).allowed);
        // Clearing the key drops its history, so the next check is allowed.
        limiter.clear("a");
        assert!(limiter.check_at("a", t0).allowed);
        // A clear on an unknown key is a harmless no-op.
        limiter.clear("never-seen");
    }
}
