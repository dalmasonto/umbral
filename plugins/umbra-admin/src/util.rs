//! Small string + header helpers used across handlers.
//!
//! Nothing here owns state; the functions are pure and unit-testable.

use umbra::web::HeaderMap;

use crate::AdminError;

/// Quote a SQL identifier by doubling embedded `"` characters. The caller
/// wraps the result in literal double quotes. We never interpolate
/// user-supplied values into SQL — only column / table names from the
/// model registry — so the surface area for an injection is the column
/// name itself, and this guards against a backend-specific identifier
/// containing a quote.
pub(crate) fn q(name: &str) -> String {
    name.replace('"', "\"\"")
}

/// HTML-escape the five characters minijinja would escape in an
/// autoescaped block. Used in handlers that build small HTML fragments
/// outside the template engine (palette items, inline-edit responses).
pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Percent-encode the unreserved set per RFC 3986. Used for flash-message
/// query params on post-action redirects. The full `percent-encoding`
/// crate would be one more dep; this is 10 lines and exact.
pub(crate) fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Was this request issued by HTMX? We branch on this to return a
/// fragment for in-place swaps versus redirecting for direct navigation.
pub(crate) fn is_htmx(headers: &HeaderMap) -> bool {
    headers
        .get("hx-request")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Turn an `AdminError` into a string safe to show the user inside an
/// inline form-error span. SQL errors are logged in full but the
/// surfaced text is sanitised:
///   - **UNIQUE constraint violations** translate to "A record with
///     this `<col>` already exists." so OneToOne / unique-field
///     duplicates surface a real error instead of "database error".
///     Parses both backends:
///       SQLite — "UNIQUE constraint failed: table.col"
///       Postgres — "duplicate key value violates unique constraint"
///                  + `Key (col)=(value) already exists.` in detail
///   - Everything else falls back to the generic "database error" so
///     no schema details leak (same posture as the original).
pub(crate) fn sanitise_form_error(e: &AdminError) -> String {
    match e {
        AdminError::Sqlx(sqlx_err) => {
            tracing::error!(error = %sqlx_err, "admin: form submission database error");
            let msg = sqlx_err.to_string();
            if let Some(col) = parse_unique_violation_column(&msg) {
                return format!("A record with this `{col}` already exists.");
            }
            if is_unique_violation(&msg) {
                return "A record with one of these values already exists.".to_string();
            }
            // Legacy path (kept as a back-compat shim): pre-gaps2 #12
            // the dynamic form path flattened WriteError to
            // `sqlx::Error::Protocol("umbra::orm::write: <message>")`.
            // The new path lands in the `Write(WriteError)` arm
            // below with the structure intact; this branch survives
            // for any future call site that re-introduces the
            // string-flattening shape (custom validators in
            // third-party plugins, etc.).
            if let Some(tail) = msg.strip_prefix("umbra::orm::write: ") {
                let mut chars = tail.chars();
                return match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => "database error".to_string(),
                };
            }
            // sqlx wraps its own Protocol payloads with the variant
            // tag in the Display form ("error returned from
            // database: ..."). Surface the inner message if it
            // looks like a constraint message users can act on.
            if msg.starts_with("error returned from database:") {
                if let Some(tail) = msg.splitn(2, ':').nth(1) {
                    let trimmed = tail.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
            "database error".to_string()
        }
        // gaps2 #12: structured umbra-validator failure. Today we
        // render the Display message directly (which gives the
        // user a single readable line per failure). The per-field
        // map rendering (showing each column's error under its
        // own input) lands in part 2 of gap #12 — at that point
        // this arm flips from returning a string to threading
        // `field_errors()` / `non_field_errors()` into the form
        // template context.
        AdminError::Write(write_err) => {
            tracing::error!(error = %write_err, "admin: form submission validator error");
            let msg = write_err.to_string();
            // The Display impl prefixes with `umbra::orm::write: `
            // (see `crates/umbra-core/src/orm/write.rs` Display).
            // Strip and capitalise to match the rendered shape the
            // legacy sqlx::Error::Protocol path produced.
            let tail = msg.strip_prefix("umbra::orm::write: ").unwrap_or(&msg);
            let mut chars = tail.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => "validation failed".to_string(),
            }
        }
        AdminError::NotFound(msg) | AdminError::Render(msg) | AdminError::BadInput(msg) => {
            msg.clone()
        }
    }
}

/// Parse a datetime string in any of the shapes umbra hands to
/// templates (RFC3339 with optional sub-second precision, SQLite
/// `datetime('now')` shape `YYYY-MM-DD HH:MM:SS`, plain RFC3339
/// without tz, just a date). Returns UTC. None on parse failure
/// so the filter caller can fall back to the raw string.
fn parse_any_datetime(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
    let s = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // SQLite shape — no T, no tz.
    for fmt in &["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(naive.and_utc());
        }
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(naive.and_utc());
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|n| n.and_utc());
    }
    None
}

/// Django-style absolute datetime humanizer:
///   `2026-06-08T21:23:20.619672614+00:00` → `Jun 8, 2026 at 9:23 PM`
/// The default format hits the common case (audit trails, "joined
/// on" labels). Falls back to the raw input if parsing fails so
/// the template never renders blank.
pub(crate) fn humanize_date(raw: &str) -> String {
    match parse_any_datetime(raw) {
        Some(dt) => dt.format("%b %-d, %Y at %-I:%M %p").to_string(),
        None => raw.to_string(),
    }
}

/// Django-style relative time humanizer:
///   `2026-06-08T21:23:20Z` → "2 hours ago" / "yesterday" / "just now"
/// The "now" reference is `chrono::Utc::now()` — UTC-only at v1
/// (matches the rest of the timestamp handling). Negative deltas
/// (a future timestamp) render as "in N <unit>" symmetrically.
pub(crate) fn naturaltime(raw: &str) -> String {
    let Some(dt) = parse_any_datetime(raw) else {
        return raw.to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(dt);
    let secs = delta.num_seconds();
    let (n, unit, past) = match secs.abs() {
        s if s < 5 => return "just now".to_string(),
        s if s < 60 => (s, "second", secs >= 0),
        s if s < 3600 => (s / 60, "minute", secs >= 0),
        s if s < 86_400 => (s / 3600, "hour", secs >= 0),
        s if s < 604_800 => (s / 86_400, "day", secs >= 0),
        s if s < 2_592_000 => (s / 604_800, "week", secs >= 0),
        s if s < 31_536_000 => (s / 2_592_000, "month", secs >= 0),
        s => (s / 31_536_000, "year", secs >= 0),
    };
    let plural = if n == 1 { "" } else { "s" };
    if past {
        format!("{n} {unit}{plural} ago")
    } else {
        format!("in {n} {unit}{plural}")
    }
}

/// Try to extract the column name from a UNIQUE constraint error
/// message. Returns the bare column (`"email"`) without the table
/// prefix so the user-facing message reads naturally regardless of
/// table naming.
///
/// Patterns recognised:
///   SQLite     `UNIQUE constraint failed: profile.user`           → `user`
///   SQLite     `UNIQUE constraint failed: profile.user, profile.x` → `user` (first wins)
///   Postgres   `Key (user)=(7) already exists.`                    → `user`
fn parse_unique_violation_column(msg: &str) -> Option<String> {
    // SQLite — strip everything before "UNIQUE constraint failed: ".
    if let Some(idx) = msg.find("UNIQUE constraint failed: ") {
        let tail = &msg[idx + "UNIQUE constraint failed: ".len()..];
        // First entry only — multi-column UNIQUE produces a comma-
        // separated list, naming the first column is good enough.
        let first = tail
            .split(',')
            .next()
            .unwrap_or(tail)
            .trim()
            .trim_end_matches(|c: char| c == ')' || c == '"' || c == '\'');
        // `table.col` → `col`. `col` (no dot) → `col` verbatim.
        let bare = first.rsplit('.').next().unwrap_or(first);
        if !bare.is_empty() {
            return Some(bare.to_string());
        }
    }
    // Postgres detail line: `Key (user)=(7) already exists.`
    if let Some(idx) = msg.find("Key (") {
        let tail = &msg[idx + "Key (".len()..];
        if let Some(end) = tail.find(')') {
            let col = tail[..end].trim();
            if !col.is_empty() {
                return Some(col.to_string());
            }
        }
    }
    None
}

/// Less precise — true when the message looks like a UNIQUE
/// violation but we couldn't isolate the column name. Used as a
/// fallback so the user at least sees "duplicate value" instead of
/// "database error" when both parsers miss.
fn is_unique_violation(msg: &str) -> bool {
    msg.contains("UNIQUE constraint failed")
        || msg.contains("duplicate key value violates unique constraint")
}

#[cfg(test)]
mod tests {
    use super::{is_unique_violation, parse_unique_violation_column};

    #[test]
    fn sqlite_unique_violation_extracts_column_after_dot() {
        let msg =
            "error returned from database: (code: 2067) UNIQUE constraint failed: profile.user";
        assert_eq!(parse_unique_violation_column(msg).as_deref(), Some("user"));
        assert!(is_unique_violation(msg));
    }

    #[test]
    fn sqlite_unique_violation_takes_first_column_of_compound_index() {
        let msg = "UNIQUE constraint failed: post.slug, post.lang";
        assert_eq!(parse_unique_violation_column(msg).as_deref(), Some("slug"));
    }

    #[test]
    fn postgres_unique_violation_extracts_column_from_key_clause() {
        let msg = "error returned from database: duplicate key value violates unique constraint \
                   \"profile_user_key\": Key (user)=(7) already exists.";
        assert_eq!(parse_unique_violation_column(msg).as_deref(), Some("user"));
        assert!(is_unique_violation(msg));
    }

    #[test]
    fn non_unique_error_returns_none() {
        let msg = "error returned from database: FOREIGN KEY constraint failed";
        assert!(parse_unique_violation_column(msg).is_none());
        assert!(!is_unique_violation(msg));
    }

    #[test]
    fn fallback_detector_catches_unparseable_unique_errors() {
        // The detail line is missing but the headline still says
        // "UNIQUE constraint failed" — `is_unique_violation` should
        // still flip true so the caller can pick the generic
        // duplicate message rather than "database error".
        let msg = "UNIQUE constraint failed";
        // Column parse fails (no `: table.col`).
        assert!(parse_unique_violation_column(msg).is_none());
        // Fallback catches it.
        assert!(is_unique_violation(msg));
    }
}
