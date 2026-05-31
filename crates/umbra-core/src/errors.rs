//! Django-shaped 404 / 500 page helpers.
//!
//! Two pieces of [`AppBuilder`](crate::app::AppBuilder) state plug
//! together with the existing templates engine to deliver Django's
//! "drop a `404.html` in your templates dir" experience:
//!
//! - `not_found_template(name)` — installs a fallback that renders
//!   the named template with `{ path }` in scope and returns 404.
//! - `server_error_template(name)` — wraps the router with a
//!   panic-catching tower-http layer that renders the named template
//!   on any handler panic and returns 500.
//!
//! Both are opt-in. When unset, the fallback returns plain-text
//! "Not Found" and panics propagate axum-style (default tower-http
//! behaviour is to log the panic and return 500 with an empty body).
//!
//! The 404 path composes with [`SlashRedirect`](crate::slash::SlashRedirect)
//! — if the redirect probe finds an alternate, it 308s; otherwise the
//! configured not-found template renders. Users get one consistent
//! 404 page across normal misses and slash-redirect dead-ends.

use std::any::Any;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use minijinja::context;

/// Render the configured 404 template with `{ path }` in scope, or
/// fall back to the plain-text response when no template is set or
/// rendering fails.
///
/// Used by:
///
/// - [`crate::slash::slash_redirect_fallback`] for the no-alternate
///   branch.
/// - The standalone not-found fallback installed when only
///   `not_found_template` is set (no slash redirect).
///
/// The template gets the request path as `path` so it can render
/// `The page {{ path }} doesn't exist.` without the user wiring
/// extractors. Other request state isn't exposed yet — the v1 shape
/// is intentionally narrow.
pub fn render_not_found(template: Option<&str>, path: &str) -> Response<Body> {
    // Derive Content-Type from whether render actually produced HTML,
    // NOT from whether a template name was supplied. When the engine
    // isn't initialised or the template fails to render, the
    // fallback "Not Found" body is plaintext; it would be wrong to
    // ship it as text/html.
    let (body, content_type) = template
        .and_then(|name| crate::templates::render(name, &context! { path => path }).ok())
        .map(|html| (html, "text/html; charset=utf-8"))
        .unwrap_or_else(|| ("Not Found".to_string(), "text/plain; charset=utf-8"));

    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        content_type.parse().expect("valid content-type"),
    );
    response
}

/// Build an axum fallback handler that renders the configured 404
/// template. Used when `not_found_template` is set but
/// `slash_redirect` is `Off` — `App::build` skips the slash redirect
/// path and installs this directly.
pub fn not_found_fallback(
    template: Option<String>,
) -> impl Fn(
    Request<Body>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response<Body>> + Send>>
+ Clone
+ Send
+ Sync
+ 'static {
    move |req: Request<Body>| {
        let template = template.clone();
        Box::pin(async move {
            let path = req.uri().path().to_owned();
            render_not_found(template.as_deref(), &path)
        })
    }
}

/// Build the panic-handler closure for
/// `tower_http::catch_panic::CatchPanicLayer::custom`.
///
/// Renders the configured `server_error_template` with no context
/// (the panic message is intentionally hidden from end users — it
/// goes to logs, not the response body). Returns a generic 500 if
/// the template fails to render OR if no template is configured.
///
/// The handler takes the panic payload as `Box<dyn Any + Send>` per
/// tower-http's `ResponseForPanic` trait shape.
pub fn server_error_panic_handler(
    template: Option<String>,
) -> impl Fn(Box<dyn Any + Send + 'static>) -> Response<Body> + Clone + Send + Sync + 'static {
    move |err: Box<dyn Any + Send + 'static>| {
        // Extract a human-readable panic message for the log line.
        // tower-http already logs the backtrace; we just need
        // something for the user-facing tracing event.
        let panic_message = if let Some(s) = err.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        tracing::error!(
            panic_message = %panic_message,
            "handler panicked; serving 500 page",
        );

        // Same Content-Type derivation as `render_not_found`: pick
        // text/html ONLY when render actually succeeded. A configured
        // template that fails to render falls back to plaintext, and
        // shipping that plaintext as text/html would be a spec lie.
        let (body, content_type) = template
            .as_deref()
            .and_then(|name| {
                crate::templates::render(name, &context! { /* deliberately empty */ }).ok()
            })
            .map(|html| (html, "text/html; charset=utf-8"))
            .unwrap_or_else(|| {
                (
                    "Internal Server Error".to_string(),
                    "text/plain; charset=utf-8",
                )
            });

        (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, content_type)],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_not_found_returns_plain_text_when_no_template() {
        let resp = render_not_found(None, "/missing");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/plain"));
    }

    #[test]
    fn render_not_found_falls_back_to_plain_text_when_template_missing() {
        // No templates engine initialised in this test — render() errors
        // out, so we should land on the plain-text fallback even though
        // a template name was provided.
        let resp = render_not_found(Some("nonexistent.html"), "/x");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
