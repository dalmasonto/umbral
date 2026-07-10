//! Gap 106 — IANA timezone resolution for the marshalling layer.
//!
//! The framework's database storage is UTC-everywhere
//! (`TIMESTAMPTZ` on Postgres, ISO-8601 text on SQLite). The
//! configured `Settings::time_zone` only affects the marshalling
//! boundary:
//!
//! - **Write path** — naive datetimes arriving from HTML
//!   `<input type="datetime-local">` (`2026-06-03T22:24`) are
//!   interpreted in the configured tz then converted to UTC for
//!   storage. With `time_zone = None` the input is treated as UTC,
//!   matching the historical behaviour.
//!
//! - **Read path** — stored UTC values are converted back to the
//!   configured tz before rendering for admin forms (so the user
//!   sees wall-clock time, not UTC). REST endpoints are unaffected;
//!   their JSON output stays RFC-3339 UTC by design.
//!
//! Misconfiguration (an unknown tz name) is logged at `WARN` and
//! falls back to UTC. We never panic on a tz string — the cost of a
//! typo in production is "users see UTC times for a release," not
//! "boot crashes." The `tz_or_utc` helper is the single source of
//! that fallback.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Record `name` as an unknown-tz we've warned about; returns `true` the
/// first time a given name is seen and `false` on every subsequent call.
///
/// `tz_or_utc` runs on the per-value datetime marshalling path, so a typo'd
/// `UMBRAL_TIME_ZONE` would otherwise emit one `warn!` per row. Deduping by
/// name keeps the log to a single line per distinct bad value (matching the
/// "one-shot warning" the module doc promises) while still surfacing a second
/// *different* typo. Pure + testable: first call `true`, repeats `false`.
fn should_warn_unknown_tz(name: &str) -> bool {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = WARNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    seen.insert(name.to_string())
}

/// Resolve the configured `Settings::time_zone` to a `chrono_tz::Tz`.
/// Returns `Tz::UTC` when settings are unset, when the value is
/// `None`, or when the IANA name doesn't resolve.
pub fn active_tz() -> Tz {
    let Some(settings) = crate::settings::get_opt() else {
        return Tz::UTC;
    };
    let Some(name) = settings.time_zone.as_deref() else {
        return Tz::UTC;
    };
    tz_or_utc(name)
}

/// Parse an IANA name, falling back to UTC with a one-shot warning
/// on failure. Public for callers that need the raw lookup (e.g.
/// per-user tz overrides surfaced from the session).
pub fn tz_or_utc(name: &str) -> Tz {
    match name.parse::<Tz>() {
        Ok(tz) => tz,
        Err(_) => {
            if should_warn_unknown_tz(name) {
                tracing::warn!(
                    tz = name,
                    "umbral::timezone: unknown IANA tz `{name}` — falling back to UTC"
                );
            }
            Tz::UTC
        }
    }
}

/// Interpret a naive datetime in the active tz and convert to UTC.
///
/// Returns `None` for ambiguous local times (the autumn DST overlap
/// hour, e.g. 2024-11-03 01:30 in America/New_York) — the caller
/// should surface a validation error rather than silently pick one
/// of the two possible UTC instants.
///
/// In `Tz::UTC` mode (`time_zone = None`), this is a straight
/// `naive.and_utc()` — no DST ambiguity is possible so it always
/// returns `Some`.
///
/// Prefer [`naive_local_to_utc_checked`] on any path that can report an error:
/// this signature throws away *why* the conversion failed, and the two reasons
/// need different messages.
pub fn naive_local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    naive_local_to_utc_checked(naive).ok()
}

/// Why a naive local datetime has no single UTC instant.
///
/// A wall-clock reading is not a moment in time until you say where the clock
/// is, and twice a year even that isn't enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalTimeError {
    /// The clocks went back, so this reading happens twice. Carries both
    /// candidate instants, earlier first, so an error message can offer them.
    Ambiguous {
        earlier: DateTime<Utc>,
        later: DateTime<Utc>,
    },
    /// The clocks went forward, so this reading never happens at all.
    Nonexistent,
}

/// Interpret `naive` as wall-clock time in the active timezone and convert it to
/// UTC, reporting *why* when there is no single answer.
///
/// The two failures are the DST transitions, and neither has a defensible
/// fallback:
///
/// - **Ambiguous** — `2026-11-01T01:30` in `America/New_York` is both `05:30Z`
///   (EDT) and `06:30Z` (EST). Picking one silently corrupts half the rows.
/// - **Nonexistent** — `2026-03-08T02:30` in the same zone never occurs; the
///   local clock jumps `02:00 → 03:00`.
///
/// In `Tz::UTC` mode (`time_zone = None`) neither can happen, so this always
/// succeeds.
pub fn naive_local_to_utc_checked(naive: NaiveDateTime) -> Result<DateTime<Utc>, LocalTimeError> {
    let tz = active_tz();
    if tz == Tz::UTC {
        return Ok(naive.and_utc());
    }
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(a, b) => Err(LocalTimeError::Ambiguous {
            earlier: a.with_timezone(&Utc),
            later: b.with_timezone(&Utc),
        }),
        chrono::LocalResult::None => Err(LocalTimeError::Nonexistent),
    }
}

/// Convert a stored UTC datetime to wall-clock time in the active
/// tz. With `time_zone = None` this is the identity function and
/// the returned naive value equals the UTC input's naive part.
pub fn utc_to_naive_local(utc: DateTime<Utc>) -> NaiveDateTime {
    let tz = active_tz();
    if tz == Tz::UTC {
        return utc.naive_utc();
    }
    utc.with_timezone(&tz).naive_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn unknown_tz_falls_back_to_utc() {
        let tz = tz_or_utc("Not/A/Real/Zone");
        assert_eq!(tz, Tz::UTC);
    }

    #[test]
    fn unknown_tz_warns_once_per_distinct_name() {
        // First sighting of a distinct bad name warns; repeats are
        // suppressed so the per-value marshalling path can't flood logs.
        let name = "Bogus/Zone/For/Dedup/Test";
        assert!(should_warn_unknown_tz(name), "first sighting should warn");
        assert!(
            !should_warn_unknown_tz(name),
            "repeat sighting of the same name must not warn again"
        );
        // A different bad name is still surfaced.
        assert!(should_warn_unknown_tz("Another/Bogus/Zone"));
    }

    #[test]
    fn naive_round_trip_through_utc_is_identity() {
        // With Tz::UTC (default — settings absent in test process)
        // the round-trip is identity by construction.
        let naive = NaiveDate::from_ymd_opt(2026, 6, 7)
            .unwrap()
            .and_hms_opt(13, 30, 0)
            .unwrap();
        let utc = naive_local_to_utc(naive).expect("Tz::UTC is unambiguous");
        let back = utc_to_naive_local(utc);
        assert_eq!(naive, back);
    }
}
