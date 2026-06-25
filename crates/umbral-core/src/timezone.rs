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
            tracing::warn!(
                tz = name,
                "umbral::timezone: unknown IANA tz `{name}` — falling back to UTC"
            );
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
pub fn naive_local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    let tz = active_tz();
    if tz == Tz::UTC {
        return Some(naive.and_utc());
    }
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(_, _) => None,
        chrono::LocalResult::None => None,
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
