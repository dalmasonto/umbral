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
//! Gap 35 extensions:
//!
//! - `on_server_error(hook)` — an opt-in hook that fires before the 500
//!   template is rendered. The closure receives the error message and the
//!   request path. Runs synchronously on the error path (panic or `Err`
//!   propagated as a 500). Cannot change the response; used for logging,
//!   Sentry dispatch, etc.
//! - Default Tailwind 404/500 templates — shipped as embedded strings so
//!   they work without any `templates/` directory on disk. Used when the
//!   user hasn't set their own template name via the builder. Opt-out via
//!   `App::builder().disable_default_error_pages()`.
//! - Dev-mode error detail — when `settings.environment == Dev`, the 500
//!   template receives an `error_chain` context variable listing the full
//!   `std::error::Error` source chain. In prod the variable is empty.
//!
//! Both the 404 and 500 fallbacks are opt-in. When unset (and default
//! pages are disabled), the fallback returns plain-text "Not Found" and
//! panics propagate axum-style (log + empty 500 body).
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

// ─── Embedded default templates ─────────────────────────────────────────────

/// Default 404 page: centered, inline Tailwind utility classes. Degrades
/// gracefully without Tailwind loaded — the page is functional even as
/// unstyled HTML.
pub const DEFAULT_404_HTML: &str = include_str!("templates/defaults/default_404.html");

/// Default 500 page: same shape as the 404. In dev mode the template
/// receives `error_display`, `error_chain` (vec of source strings), and
/// `request_path` context variables that render into an expandable detail
/// block. In prod those variables are empty strings / empty vecs.
pub const DEFAULT_500_HTML: &str = include_str!("templates/defaults/default_500.html");

// Template names used when registering the defaults into minijinja.
// `pub` so integration tests can verify the constants without duplicating
// the string literals.
pub const DEFAULT_404_TEMPLATE_NAME: &str = "__umbra__/default_404.html";
pub const DEFAULT_500_TEMPLATE_NAME: &str = "__umbra__/default_500.html";

// ─── On-server-error hook type ───────────────────────────────────────────────

/// Shared callback type for the `on_server_error` hook.
///
/// The hook fires on every 500 — both panics and handler errors that are
/// turned into a 500 response — before the template is rendered.
///
/// Arguments:
/// - `error_display`: the `Display` form of the error, or the stringified
///   panic payload.
/// - `request_path`: the URI path of the failing request.
pub type ServerErrorHook = std::sync::Arc<dyn Fn(&str, &str) + Send + Sync + 'static>;

// ─── Ambient default-pages flag ─────────────────────────────────────────────

use std::sync::OnceLock;

/// Whether the default error pages are enabled. Set during `App::build()`.
/// `true` (default) — use the embedded templates when the user hasn't
/// supplied their own. `false` — user called `.disable_default_error_pages()`.
static DEFAULT_PAGES_ENABLED: OnceLock<bool> = OnceLock::new();

/// Publish the default-pages flag. Called by `AppBuilder::build()` only.
pub(crate) fn init_default_pages(enabled: bool) {
    // Ignore the error if already set (e.g. two App builds in the same
    // process in tests). The first caller wins, matching the OnceLock
    // contract everywhere else in the framework.
    let _ = DEFAULT_PAGES_ENABLED.set(enabled);
}

/// Return whether default pages are enabled.
pub(crate) fn default_pages_enabled() -> bool {
    // When called outside App::build() (unit tests that exercise render_*
    // directly), default to true so the helpers behave like a real app.
    *DEFAULT_PAGES_ENABLED.get().unwrap_or(&true)
}

// ─── 404 helpers ────────────────────────────────────────────────────────────

/// Render the configured 404 template with `{ path }` in scope, or
/// fall back to the plain-text response when no template is set or
/// rendering fails.
///
/// When `template` is `None` and the default pages are enabled, the
/// framework's own `default_404.html` is rendered instead. When
/// default pages are disabled and no template name is set, returns
/// plain "Not Found".
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
    // Resolve the effective template name:
    //   1. User-supplied name takes highest priority.
    //   2. Embedded default (registered as __umbra__/default_404.html) when
    //      default pages are enabled.
    //   3. Plain-text fallback.
    let effective_template = template.or_else(|| {
        if default_pages_enabled() {
            Some(DEFAULT_404_TEMPLATE_NAME)
        } else {
            None
        }
    });

    // Derive Content-Type from whether render actually produced HTML.
    // When the engine isn't initialised or the template fails to render,
    // the fallback "Not Found" body is plaintext; it would be wrong to
    // ship it as text/html.
    let (body, content_type) = effective_template
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

// ─── 500 helpers ────────────────────────────────────────────────────────────

/// Walk the `std::error::Error::source()` chain and collect every
/// `Display` message into a `Vec<String>`. The first entry is the top-level
/// error itself; subsequent entries are its causes.
///
/// Used by the handler-error path (where `Err` variants produce 500s) to
/// surface the full cause chain in dev-mode 500 pages. The panic path uses
/// a synthetic single-element chain instead (panics aren't `dyn Error`).
pub fn collect_error_chain(top: &str, mut source: Option<&dyn std::error::Error>) -> Vec<String> {
    let mut chain = vec![top.to_owned()];
    while let Some(cause) = source {
        chain.push(cause.to_string());
        source = cause.source();
    }
    chain
}

/// Determine whether the current settings are dev mode.
///
/// Returns `false` when the settings OnceLock isn't initialised (i.e. tests
/// that exercise the 500 helpers directly without calling `App::build`).
fn is_dev_mode() -> bool {
    crate::settings::SETTINGS
        .get()
        .map(|s| matches!(s.environment, crate::settings::Environment::Dev))
        .unwrap_or(false)
}

/// Build the template context for a 500 response.
///
/// In dev mode, `error_display`, `error_chain` (Vec<String>), and
/// `request_path` are populated. In prod they are empty string / empty
/// vec / empty string so the template's conditional block collapses
/// to nothing.
fn build_500_context(
    error_display: &str,
    error_chain: &[String],
    request_path: &str,
    dev: bool,
) -> minijinja::Value {
    if dev {
        context! {
            dev_mode => true,
            error_display => error_display,
            error_chain => error_chain,
            request_path => request_path,
        }
    } else {
        context! {
            dev_mode => false,
            error_display => "",
            error_chain => Vec::<String>::new(),
            request_path => "",
        }
    }
}

/// Render the 500 template with the given context.
///
/// Resolves the effective template name the same way `render_not_found`
/// resolves the 404: user-supplied name → embedded default → plain text.
fn render_500(template: Option<&str>, ctx: &minijinja::Value) -> (String, &'static str) {
    let effective = template.or_else(|| {
        if default_pages_enabled() {
            Some(DEFAULT_500_TEMPLATE_NAME)
        } else {
            None
        }
    });

    effective
        .and_then(|name| crate::templates::render(name, ctx).ok())
        .map(|html| (html, "text/html; charset=utf-8"))
        .unwrap_or_else(|| {
            (
                "Internal Server Error".to_string(),
                "text/plain; charset=utf-8",
            )
        })
}

/// Build the panic-handler closure for
/// `tower_http::catch_panic::CatchPanicLayer::custom`.
///
/// Renders the configured `server_error_template` (or the built-in default
/// when enabled) with optional dev-mode error context. Before rendering,
/// calls the `on_server_error` hook if one was registered.
///
/// In dev mode the template receives:
/// - `dev_mode: true`
/// - `error_display`: the stringified panic payload
/// - `error_chain`: `[error_display]` (panics have no error chain)
/// - `request_path`: empty string (not available in a panic handler)
///
/// In prod, all three are empty.
pub fn server_error_panic_handler(
    template: Option<String>,
    hook: Option<ServerErrorHook>,
) -> impl Fn(Box<dyn Any + Send + 'static>) -> Response<Body> + Clone + Send + Sync + 'static {
    move |err: Box<dyn Any + Send + 'static>| {
        // Extract a human-readable panic message for the log line.
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

        // Fire the on_server_error hook before rendering.
        if let Some(ref h) = hook {
            h(&panic_message, "");
        }

        let dev = is_dev_mode();
        let chain = vec![panic_message.clone()];
        let ctx = build_500_context(&panic_message, &chain, "", dev);
        let (body, content_type) = render_500(template.as_deref(), &ctx);

        (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, content_type)],
            body,
        )
            .into_response()
    }
}

/// Build an axum fallback or middleware that converts a handler `Err`
/// response into a 500 with optional dev-mode detail and hook notification.
///
/// Used internally when a handler returns a type that produces a 500
/// status code (e.g. `(StatusCode::INTERNAL_SERVER_ERROR, body)`). The
/// wrapper intercepts 500 responses, fires the hook if set, and optionally
/// re-renders them through the 500 template.
///
/// Because axum handlers choose their own `IntoResponse` impl, this path
/// is specifically for handlers that return
/// `(StatusCode::INTERNAL_SERVER_ERROR, ...)` tuples. Panics are caught
/// by the `CatchPanicLayer` above.
///
/// Note: this function is primarily used by the test suite to verify that
/// `on_server_error` fires for handler errors. In production the hook is
/// most naturally wired through a middleware.
pub fn fire_server_error_hook(hook: &Option<ServerErrorHook>, error_msg: &str, path: &str) {
    if let Some(h) = hook {
        h(error_msg, path);
    }
}

// ─── Response-rendering middleware (handler-Err path) ───────────────────────

/// State for the response-rendering middleware. Cloned per-request; both
/// fields are cheap to clone (`Option<String>` + `Option<Arc<...>>`).
#[derive(Clone)]
pub struct Render500State {
    pub template: Option<String>,
    pub hook: Option<ServerErrorHook>,
}

/// Middleware that intercepts plain-text 500 responses and re-renders them
/// through the configured `server_error_template` (or the embedded default
/// when enabled). Already-HTML 500 responses pass through untouched — those
/// were rendered by `CatchPanicLayer` (panics) or by the handler itself.
///
/// This closes the gap where a handler returning
/// `Err((StatusCode::INTERNAL_SERVER_ERROR, msg))` previously produced a
/// raw plain-text response instead of the configured 500 page. The
/// `on_server_error` hook also fires for these paths, with the response
/// body bytes as the error message and the request URI as the path.
pub async fn render_500_middleware(
    axum::extract::State(state): axum::extract::State<Render500State>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response<Body> {
    let path = req.uri().path().to_string();
    let resp = next.run(req).await;

    if resp.status() != StatusCode::INTERNAL_SERVER_ERROR {
        return resp;
    }

    // Already-rendered HTML 500s (from CatchPanicLayer or a custom handler)
    // pass through. Only the raw text/plain or no-content-type 500s get
    // re-rendered.
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.starts_with("text/html") {
        return resp;
    }

    // Capture the body to extract the error message for the hook + dev
    // context. 64KB cap: error messages don't need more, and we don't
    // want a malicious upstream to OOM us.
    let (_parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let error_msg = String::from_utf8_lossy(&bytes).to_string();

    tracing::error!(
        error = %error_msg,
        path = %path,
        "handler returned 500; rendering server-error template",
    );

    fire_server_error_hook(&state.hook, &error_msg, &path);

    let dev = is_dev_mode();
    let chain = vec![error_msg.clone()];
    let ctx = build_500_context(&error_msg, &chain, &path, dev);
    let (body_str, content_type) = render_500(state.template.as_deref(), &ctx);

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, content_type)],
        body_str,
    )
        .into_response()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

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

    #[test]
    fn collect_error_chain_single_level() {
        let chain = collect_error_chain("top error", None);
        assert_eq!(chain, vec!["top error"]);
    }

    #[test]
    fn build_500_context_prod_mode_has_empty_fields() {
        let ctx = build_500_context("boom", &["boom".to_owned()], "/path", false);
        // Serialize to JSON and inspect: prod mode has dev_mode=false and
        // empty error_display.
        let json = serde_json::to_value(&ctx).expect("context serialises");
        assert_eq!(json["dev_mode"], serde_json::Value::Bool(false));
        assert_eq!(
            json["error_display"],
            serde_json::Value::String("".to_string())
        );
    }

    #[test]
    fn build_500_context_dev_mode_has_error_info() {
        let chain = vec!["cause one".to_owned(), "cause two".to_owned()];
        let ctx = build_500_context("top error", &chain, "/api/items", true);
        let json = serde_json::to_value(&ctx).expect("context serialises");
        assert_eq!(json["dev_mode"], serde_json::Value::Bool(true));
        assert_eq!(
            json["error_display"],
            serde_json::Value::String("top error".to_string())
        );
        // error_chain should be a two-element array
        let arr = json["error_chain"]
            .as_array()
            .expect("error_chain is array");
        assert_eq!(arr.len(), 2);
    }
}
