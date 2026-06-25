//! Regression test: `render_shell` must substitute each placeholder exactly
//! once in a single left-to-right pass, so a value that *contains* a later
//! placeholder token is never re-expanded.
//!
//! The attack scenario: if `app_name` is set to the literal string
//! `__OPENAPI_URL_JSON__`, a naïve sequential `.replace()` chain would
//! inject that token at step 3/4 and then step 5 would replace it with the
//! real OpenAPI URL — putting the server-side spec URL inside what should be
//! the (user-controlled) app-name slot.  The single-pass fix prevents this.

use umbral_playground::routes::{PlaygroundState, render_shell};

/// If `app_name` is set to `__OPENAPI_URL_JSON__` (the literal placeholder
/// token for the spec URL), the rendered shell must contain that literal
/// string verbatim in the app-name slots — NOT the expanded spec URL.
///
/// With the old sequential-replace chain this test fails because step 5
/// (`__OPENAPI_URL_JSON__` → spec URL) re-expands the token that step 4
/// injected into the app-name position.
#[test]
fn app_name_containing_later_placeholder_is_not_re_expanded() {
    let state = PlaygroundState::new(
        "/api/playground",
        // The app name is the literal text of a later placeholder token.
        "__OPENAPI_URL_JSON__",
        false,
        "/static/playground/assets",
    );

    let html = render_shell(&state);

    // The meta-attribute value must be the HTML-escaped version of
    // `__OPENAPI_URL_JSON__` (no special chars to escape, so verbatim).
    assert!(
        html.contains(r#"content="__OPENAPI_URL_JSON__""#),
        "app-name meta attribute must carry the literal token, not an expanded URL;\
         \ngot:\n{html}"
    );

    // The window global must be the JSON-encoded version of the literal
    // string `__OPENAPI_URL_JSON__` — i.e. `"__OPENAPI_URL_JSON__"` with
    // double-quotes.
    assert!(
        html.contains(r#"window.__UMBRAL_PLAYGROUND_APP__ = "__OPENAPI_URL_JSON__";"#),
        "app-name window global must carry the literal token as a JSON string;\
         \ngot:\n{html}"
    );

    // The real spec URL must appear exactly once — in the __OPENAPI_URL_JSON__
    // slot — and NOT inside the app-name positions above.
    // (We check the window-global line that should have only the spec URL.)
    let openapi_line = html
        .lines()
        .find(|l| l.contains("__UMBRAL_OPENAPI_URL__"))
        .unwrap_or("");
    // That line must not also contain the app-name literal token.
    assert!(
        !openapi_line.contains("__OPENAPI_URL_JSON__"),
        "the __UMBRAL_OPENAPI_URL__ assignment line must not contain the raw \
         placeholder token; got line: {openapi_line}"
    );
}
