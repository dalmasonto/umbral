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
    //
    // In dev mode, surface the registered-route registry so a
    // developer who hits a typoed URL can see what's actually
    // available. Production responses stay minimal — `dev_mode` is
    // false there, so the template's `{% if dev_mode %}` block
    // collapses to nothing.
    let dev_mode = crate::settings::get_opt()
        .map(|s| matches!(s.environment, crate::settings::Environment::Dev))
        .unwrap_or(false);
    let routes_ctx: Vec<minijinja::Value> = if dev_mode {
        crate::routes::get()
            .map(|reg| {
                reg.by_plugin
                    .iter()
                    .filter(|(_, specs)| !specs.is_empty())
                    .map(|(plugin, specs)| {
                        // Pre-shape each route entry for the
                        // template's loop: a path string and a
                        // pre-joined method label. Pre-joining here
                        // lets the template render the badge with a
                        // single `{{ route.method_label }}` access
                        // instead of nesting another for-loop.
                        let routes: Vec<minijinja::Value> = specs
                            .iter()
                            .map(|s| {
                                let method_label = if s.methods.is_empty() {
                                    "ANY".to_string()
                                } else {
                                    s.methods.join("·")
                                };
                                minijinja::context! {
                                    path => s.path.as_str(),
                                    methods => s.methods.clone(),
                                    method_label => method_label,
                                }
                            })
                            .collect();
                        minijinja::context! {
                            plugin => plugin.as_str(),
                            routes => routes,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let ctx = context! {
        path => path,
        dev_mode => dev_mode,
        routes_by_plugin => routes_ctx,
    };
    let (body, content_type) = effective_template
        .and_then(|name| match crate::templates::render(name, &ctx) {
            Ok(html) => Some(html),
            // Falling back to plain text is intentional (no double-fault on
            // the error path), but a broken error template should leave a
            // trace rather than silently degrade.
            Err(e) => {
                tracing::warn!(
                    "error-page template `{name}` failed to render ({e}); \
                     falling back to plain text"
                );
                None
            }
        })
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
/// If the chosen template itself errors during render (the
/// recovery-path-failed case: usually a `{% extends "wrapper.html" %}`
/// that breaks because wrapper.html shares the bug that fired the
/// original 500), the secondary error gets `tracing::error!`'d AND
/// — when dev mode is on — embedded in the plain-text fallback body
/// so the developer sees the recovery failure inline instead of
/// staring at a generic "Internal Server Error" while the real
/// chain hides in the logs.
fn render_500(template: Option<&str>, ctx: &minijinja::Value) -> (String, &'static str) {
    let effective = template.or_else(|| {
        if default_pages_enabled() {
            Some(DEFAULT_500_TEMPLATE_NAME)
        } else {
            None
        }
    });

    let Some(name) = effective else {
        return (
            "Internal Server Error".to_string(),
            "text/plain; charset=utf-8",
        );
    };

    match crate::templates::render(name, ctx) {
        Ok(html) => (html, "text/html; charset=utf-8"),
        Err(secondary) => {
            // The secondary failure WAS being silently swallowed by
            // `.ok()`. Loud-fail it instead — the operator needs to
            // see both errors (the original handler 500 already
            // logged in `render_500_middleware`, plus this one).
            tracing::error!(
                template = %name,
                error = %secondary,
                "render_500: secondary template render failed; the configured \
                 server-error template can't render itself. Likely a broken \
                 `{{% extends \"wrapper.html\" %}}` chain. Falling back to \
                 plain text.",
            );
            if is_dev_mode() {
                // In dev, include both errors in the body so the
                // user doesn't have to grep server logs to see why
                // their 500 page didn't render.
                let body = format!(
                    "Internal Server Error\n\n\
                     (dev) The configured 500 template `{name}` itself failed \
                     to render: {secondary}\n\n\
                     Check the original handler error in the server logs \
                     (line above this one) for the trigger."
                );
                (body, "text/plain; charset=utf-8")
            } else {
                (
                    "Internal Server Error".to_string(),
                    "text/plain; charset=utf-8",
                )
            }
        }
    }
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

// ─── General error pages (any status code) ──────────────────────────────────

/// State for the general error-page middleware: a status → template-name map.
/// Cloned per request (an `Arc`, cheap).
#[derive(Clone)]
pub struct RenderErrorState {
    pub templates: std::sync::Arc<std::collections::HashMap<StatusCode, String>>,
}

/// Middleware that styles error responses for ANY registered status code
/// (e.g. 429, 403, 410) the way [`render_500_middleware`] does for 500. After
/// the handler runs, if the response status has a registered template and the
/// body isn't already HTML, the body text is captured as the `message` and the
/// template is rendered in its place — preserving the original status code.
///
/// Registered via `App::builder().error_template(status, "name.html")`. 404
/// and 500 keep their dedicated paths (`not_found_template` /
/// `server_error_template`); this covers everything else a handler returns as
/// `Err((status, message))`.
pub async fn render_error_middleware(
    axum::extract::State(state): axum::extract::State<RenderErrorState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response<Body> {
    let path = req.uri().path().to_string();
    // API / AJAX clients (Accept: application/json) keep the raw status +
    // message body so they can read it programmatically; only browser
    // navigations (Accept: text/html, the default) get the styled HTML page.
    let wants_json = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false);
    let resp = next.run(req).await;

    let status = resp.status();
    let Some(template) = state.templates.get(&status).cloned() else {
        return resp;
    };
    if wants_json {
        return resp;
    }

    // Already-HTML error responses (a handler that rendered its own page) pass
    // through untouched — only bare text/plain (or no content-type) errors get
    // the styled template.
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.starts_with("text/html") {
        return resp;
    }

    // Capture the body (the handler's message). 64KB cap, same as the 500 path.
    let (_parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let message = String::from_utf8_lossy(&bytes).to_string();

    let ctx = error_context(status, &message, &path, is_dev_mode());
    let (body_str, content_type) = render_error_page(&template, status, &ctx);

    (status, [(header::CONTENT_TYPE, content_type)], body_str).into_response()
}

/// Template context for a general error page: `{ status, status_text, message,
/// request_path, dev_mode }`.
fn error_context(status: StatusCode, message: &str, path: &str, dev: bool) -> minijinja::Value {
    minijinja::context! {
        status => status.as_u16(),
        status_text => status.canonical_reason().unwrap_or(""),
        message => message,
        request_path => path,
        dev_mode => dev,
    }
}

/// Render `template` for an error page, falling back to the status' canonical
/// reason phrase as plain text if the template can't render. Mirrors the
/// loud-fail posture of [`render_500`].
fn render_error_page(
    template: &str,
    status: StatusCode,
    ctx: &minijinja::Value,
) -> (String, &'static str) {
    match crate::templates::render(template, ctx) {
        Ok(html) => (html, "text/html; charset=utf-8"),
        Err(secondary) => {
            tracing::error!(
                template = %template,
                status = %status.as_u16(),
                error = %secondary,
                "render_error_page: the configured error template failed to render; \
                 falling back to plain text",
            );
            let reason = status.canonical_reason().unwrap_or("Error");
            (reason.to_string(), "text/plain; charset=utf-8")
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_context_carries_status_reason_message_and_path() {
        let ctx = error_context(
            StatusCode::TOO_MANY_REQUESTS,
            "slow down",
            "/p/notes",
            false,
        );
        let mut env = minijinja::Environment::new();
        env.add_template(
            "t",
            "{{ status }}|{{ status_text }}|{{ message }}|{{ request_path }}|{{ dev_mode }}",
        )
        .unwrap();
        let out = env.get_template("t").unwrap().render(ctx).unwrap();
        assert_eq!(out, "429|Too Many Requests|slow down|/p/notes|false");
    }

    #[test]
    fn render_error_page_falls_back_to_plain_text_when_template_cant_render() {
        // No ambient template engine in this unit test, so `render()` errors
        // and we land on the canonical-reason plain-text fallback.
        let ctx = error_context(StatusCode::TOO_MANY_REQUESTS, "msg", "/x", false);
        let (body, ct) = render_error_page("nonexistent.html", StatusCode::TOO_MANY_REQUESTS, &ctx);
        assert!(ct.starts_with("text/plain"), "content-type: {ct}");
        assert_eq!(body, "Too Many Requests");
    }

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
    fn default_404_renders_route_panel_when_dev_mode_and_registry_populated() {
        // Render the embedded default template through a fresh
        // minijinja environment so the test doesn't depend on the
        // (OnceLock-published) global engine state. We feed the same
        // ctx shape `render_not_found` builds in dev mode and assert
        // the route list lands in the output.
        let mut env = minijinja::Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.add_template("default_404.html", DEFAULT_404_HTML)
            .unwrap();

        let ctx = minijinja::context! {
            path => "/typo",
            dev_mode => true,
            routes_by_plugin => serde_json::json!([
                {
                    "plugin": "app",
                    "routes": [
                        { "path": "/",         "methods": ["GET"],       "method_label": "GET" },
                        { "path": "/articles", "methods": ["GET","POST"], "method_label": "GET·POST" },
                    ],
                },
                {
                    "plugin": "admin",
                    "routes": [
                        { "path": "/admin/",      "methods": ["GET"],      "method_label": "GET" },
                        { "path": "/admin/login", "methods": ["GET","POST"], "method_label": "GET·POST" },
                    ],
                },
            ]),
        };
        let out = env
            .get_template("default_404.html")
            .unwrap()
            .render(&ctx)
            .unwrap();

        // minijinja's HTML autoescape encodes `/` as `&#x2f;` inside
        // text nodes — the assertion checks the escaped form (which is
        // what the browser will then unescape and display verbatim).
        assert!(
            out.contains("Dev only"),
            "dev-mode panel header should be in the output"
        );
        assert!(
            out.contains("&#x2f;admin&#x2f;login"),
            "admin route should be listed: {out}"
        );
        assert!(
            out.contains("&#x2f;articles"),
            "app route should be listed: {out}"
        );
        // Method badges land in the markup.
        assert!(
            out.contains("GET·POST"),
            "composite-method badge label should render: {out}"
        );
        // GET-coloured badge applied to the bare-GET row.
        assert!(
            out.contains("emerald"),
            "GET badge should carry the emerald tint class"
        );
    }

    #[test]
    fn default_404_omits_route_panel_when_dev_mode_is_off() {
        // Same template, but `dev_mode = false` — the panel block must
        // collapse to nothing. The page should still render the path
        // and the action buttons (those are outside the gated block).
        let mut env = minijinja::Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.add_template("default_404.html", DEFAULT_404_HTML)
            .unwrap();

        let ctx = minijinja::context! {
            path => "/typo",
            dev_mode => false,
            routes_by_plugin => Vec::<minijinja::Value>::new(),
        };
        let out = env
            .get_template("default_404.html")
            .unwrap()
            .render(&ctx)
            .unwrap();

        assert!(
            !out.contains("Dev only"),
            "production response must not surface the route registry"
        );
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
