//! Small string + header helpers used across handlers.
//!
//! Nothing here owns state; the functions are pure and unit-testable.

use umbral::web::HeaderMap;

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

/// Escape a string for safe embedding inside a JavaScript string literal
/// that itself sits inside an HTML attribute — e.g. an inline
/// `onclick="fn('{{ value }}')"` handler.
///
/// HTML-entity escaping ([`html_escape`]) is the WRONG encoding here: the
/// HTML parser decodes `&#x27;` back to `'` *before* the JS engine sees the
/// handler source, so an HTML-escaped quote still breaks out of the JS
/// string. We instead map every character that is dangerous in either the
/// HTML-attribute layer or the JS-string layer to its `\uXXXX` JS
/// unicode-escape. `\uXXXX` contains no HTML metacharacter, so it survives
/// the HTML-attribute decode untouched, and the JS engine turns it back into
/// the literal character *inside* the string — never a delimiter.
///
/// Note this alone does NOT make a value safe for `innerHTML` (the escaped
/// `<` becomes a real `<` once the JS string is decoded). For a value that
/// flows into `innerHTML`, HTML-escape first and then run this over the
/// result (see `cell_edit_get`).
pub(crate) fn escape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' | '\'' | '"' | '`' | '<' | '>' | '&' | '/' | '\n' | '\r' | '\t' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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

/// Merge a `WriteError`'s per-field messages into the matching `FormField`
/// slots and return a non-empty top-banner string for the remaining
/// non-field errors (or an empty string when every failure was field-level).
///
/// This is the structured half of gaps2 #12 part 2. The caller builds the
/// `Vec<FormField>` first (with prefill values preserved), then calls this
/// to attribute each error to the right input. `non_field_errors()` go to
/// the top-of-form banner; `field_errors()` for a column that isn't in
/// `fields` (hidden/readonly) fall back to the banner too so they aren't
/// silently lost.
pub(crate) fn apply_write_error_to_fields(
    we: &umbral::orm::write::WriteError,
    fields: &mut [crate::view::FormField],
) -> String {
    // per-field messages
    let by_col = we.field_errors();
    let mut unmatched: Vec<String> = Vec::new();
    for (col, messages) in &by_col {
        if let Some(f) = fields.iter_mut().find(|f| &f.name == col) {
            // Join multiple messages with "; " — the same shape the
            // up-front validate_form errors use (single string per slot).
            let msg = messages.join("; ");
            if f.error.is_empty() {
                f.error = msg;
            } else {
                f.error.push_str("; ");
                f.error.push_str(&msg);
            }
        } else {
            // Column hidden from form or not on this model — escalate to
            // the top banner so the error is never silently dropped.
            for m in messages {
                unmatched.push(format!("`{col}`: {m}"));
            }
        }
    }
    // non-field errors always go to the banner
    let mut banner_parts: Vec<String> = we.non_field_errors();
    banner_parts.extend(unmatched);
    banner_parts.join("; ")
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
            // `sqlx::Error::Protocol("umbral::orm::write: <message>")`.
            // The new path lands in the `Write(WriteError)` arm
            // below with the structure intact; this branch survives
            // for any future call site that re-introduces the
            // string-flattening shape (custom validators in
            // third-party plugins, etc.).
            if let Some(tail) = msg.strip_prefix("umbral::orm::write: ") {
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
        // gaps2 #12: structured umbral-validator failure. Today we
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
            // The Display impl prefixes with `umbral::orm::write: `
            // (see `crates/umbral-core/src/orm/write.rs` Display).
            // Strip and capitalise to match the rendered shape the
            // legacy sqlx::Error::Protocol path produced.
            let tail = msg.strip_prefix("umbral::orm::write: ").unwrap_or(&msg);
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

/// Parse a datetime string in any of the shapes umbral hands to
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

/// Absolute datetime humanizer:
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

/// Relative time humanizer:
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
pub(crate) fn parse_unique_violation_column(msg: &str) -> Option<String> {
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
    use super::{escape_js, html_escape, is_unique_violation, parse_unique_violation_column};

    // --------------------------------------------------------------------
    // escape_js — the JS-string / inline-event-handler encoder (XSS fix).
    // --------------------------------------------------------------------

    /// The classic reflected/stored breakout payload for an inline handler
    /// `onclick="fn('{{ v }}')"`: a bare `'` must NOT survive as a raw quote
    /// (which would close the JS string), and the HTML parser must not be
    /// able to reconstitute one either (so no `&#x27;` / `&apos;` forms).
    #[test]
    fn escape_js_neutralises_single_quote_breakout() {
        let payload = "');alert(document.cookie);('";
        let out = escape_js(payload);
        assert!(
            !out.contains('\''),
            "raw single quote must not survive escape_js: {out}"
        );
        // The quote is emitted as its JS unicode escape, which the JS engine
        // decodes to a string char, never a delimiter.
        assert!(
            out.contains("\\u0027"),
            "single quote should become \\u0027: {out}"
        );
        // And no HTML-entity form that could decode back to a quote.
        assert!(!out.contains("&#x27;") && !out.contains("&apos;"));
    }

    /// Double quote (the HTML attribute delimiter) and backslash are escaped
    /// so the value can't break out of `onclick="…"` or the JS string.
    #[test]
    fn escape_js_escapes_attr_delimiter_and_backslash() {
        let out = escape_js(r#"a"b\c"#);
        assert!(
            !out.contains('"'),
            "raw double quote must be escaped: {out}"
        );
        // The only backslashes left are the ones introducing \uXXXX escapes.
        assert!(out.contains("\\u0022"), "\" → \\u0022: {out}");
        assert!(out.contains("\\u005c"), "\\ → \\u005c: {out}");
    }

    /// Angle brackets / ampersand are escaped so a value can't form a
    /// `</script>` breakout or an HTML entity inside the handler source.
    #[test]
    fn escape_js_escapes_markup_chars() {
        let out = escape_js("</script><img>&");
        assert!(!out.contains('<') && !out.contains('>') && !out.contains('&'));
        assert!(out.contains("\\u003c") && out.contains("\\u003e"));
    }

    /// Ordinary text passes through unchanged — the encoder is a no-op for
    /// safe input, so it doesn't corrupt legitimate labels.
    #[test]
    fn escape_js_passes_safe_text_through() {
        assert_eq!(escape_js("Hello world 42"), "Hello world 42");
    }

    /// The `cell_edit_get` sink is doubly nested: the value lands in a JS
    /// string that is then assigned to `innerHTML`. The correct encoding is
    /// `escape_js(html_escape(v))`. Verify a `<img onerror>` payload can
    /// neither break the JS string NOR reach innerHTML as live markup: after
    /// the JS engine decodes the string, the content is still HTML-escaped
    /// (`&lt;img …`), so innerHTML renders it as inert text.
    #[test]
    fn escape_js_of_html_escape_is_safe_for_innerhtml_sink() {
        let payload = r#"<img src=x onerror=alert(1)>'"#;
        let encoded = escape_js(&html_escape(payload));
        // No raw quote → can't close the JS string literal.
        assert!(!encoded.contains('\''), "no raw quote: {encoded}");
        // No raw `<` → after JS-string decode the innerHTML content is the
        // HTML-escaped `&lt;img …`, i.e. inert text, not a live element.
        assert!(!encoded.contains('<'), "no raw `<`: {encoded}");
        // The angle bracket is carried as the escaped HTML entity's `&`.
        assert!(
            encoded.contains("\\u0026lt;"),
            "html-escaped `<` (&lt;) should be JS-escaped as \\u0026lt;: {encoded}"
        );
    }

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
