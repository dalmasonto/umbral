//! Small string + header helpers used across handlers.
//!
//! Nothing here owns state; the functions are pure and unit-testable.

use umbra::web::HeaderMap;

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
