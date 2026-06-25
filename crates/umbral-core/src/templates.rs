//! Server-side HTML rendering via minijinja.
//!
//! Templates live under one or more directories on disk. At boot,
//! `App::build()` assembles an ordered search list:
//!
//! 1. The project-level directory configured via
//!    `AppBuilder::templates_dir` (default `./templates`).
//! 2. Each registered plugin's `Plugin::templates_dirs()` contributions,
//!    in topological dependency order.
//!
//! The first directory that contains a given template name wins. This
//! makes cross-plugin `{% extends "base.html" %}` work automatically —
//! the extends lookup searches every directory the same way a direct
//! render call does. Plugin A can extend `base.html` from plugin B as
//! long as B's directory appears in the search list.
//!
//! When two directories both provide a template with the same name, the
//! first-match-wins policy applies and a `tracing::warn!` is emitted at
//! boot so the collision is visible in the log. First-match-wins across
//! all template directories. Silently-overridden templates are a
//! well-known footgun, so the warning is non-optional.
//!
//! Rendering goes through one ambient accessor, [`render`], which reads
//! the engine the App builder published into an `OnceLock` during build.
//!
//! ```ignore
//! let html = umbral::templates::render("articles_list.html", &context!(articles))?;
//! ```
//!
//! ## Autoescape
//!
//! Any template whose name ends in `.html` or `.htm` renders with
//! autoescape on. Text templates (`.txt`) render verbatim. The autoescape
//! callback extension whitelist MUST stay in sync with the loader's
//! `load_directory` filter (currently `html | htm | txt`).
//!
//! ## v1 scope
//!
//! - One project-level templates directory (default `./templates/`,
//!   relative to the binary's cwd) plus per-plugin directories.
//! - Jinja2-compatible syntax via minijinja: `{% extends %}`, `{% block %}`,
//!   `{% if %}`, `{% for %}`, `{{ value }}`, the standard filter set.
//! - Autoescape for any template whose name ends in `.html` or `.htm`.
//! - Init is best-effort: if no directory exists the engine boots empty.
//!   Calls to [`render`] then return `TemplateError::Missing`.
//!
//! ## Deferred
//!
//! - Custom filters and tests registered through `Plugin::on_ready`.
//! - Hot reload in development via `minijinja-autoreload`.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;

use minijinja::{AutoEscape, Environment};
use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

tokio::task_local! {
    /// Per-request ambient user value, set by a session-aware layer
    /// (typically `umbral_sessions::UserContextLayer<U>`) and read by
    /// [`render`] to expose the current `user` in
    /// templates. `None` means an anonymous request.
    ///
    /// Outside the layer's scope, `try_with` returns `Err(AccessError)`
    /// and `render` skips the merge — explicit ctx behaviour is
    /// preserved when no layer is installed.
    pub static CURRENT_USER: Option<minijinja::Value>;

    /// Per-request CSRF token, set by `umbral-security`'s middleware and
    /// read by [`render`] to inject `csrf_token` / `csrf_input` into
    /// every template, for the `{% csrf_token %}` ergonomic. Outside
    /// the middleware's scope nothing is injected (a template that
    /// references `{{ csrf_token }}` then renders it empty under the
    /// engine's lenient-undefined behaviour).
    pub static CURRENT_CSRF: Option<String>;

    /// Lazy counterpart to `CURRENT_USER`: a resolver that produces the
    /// user value on first access, memoized. Set by an auth middleware that
    /// wants per-request laziness (resolve only if a template reads `user`).
    pub static CURRENT_USER_LAZY: LazyUser;
}

type UserFut = Pin<Box<dyn Future<Output = minijinja::Value> + Send>>;
type UserResolver = Arc<dyn Fn() -> UserFut + Send + Sync>;

/// A lazily-resolved, per-request template `user`. The `resolver` runs at
/// most once (guarded by the `OnceCell`); resolution happens synchronously
/// from inside minijinja's sync render via `block_in_place`.
///
/// The lazy value is injected into the template context as a minijinja `Object`
/// proxy ([`LazyUserProxy`]). Minijinja calls `get_value` on the proxy only
/// when the template actually accesses an attribute on `user`, so requests that
/// never render `user` skip resolution entirely.
#[derive(Clone)]
pub struct LazyUser {
    cell: Arc<tokio::sync::OnceCell<minijinja::Value>>,
    resolver: UserResolver,
}

impl LazyUser {
    pub fn new<F, Fut>(resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = minijinja::Value> + Send + 'static,
    {
        Self {
            cell: Arc::new(tokio::sync::OnceCell::new()),
            resolver: Arc::new(move || Box::pin(resolver())),
        }
    }

    /// Resolve (memoized) from a synchronous context. Requires a multi-thread
    /// tokio runtime; on a current-thread runtime or outside any runtime it
    /// logs and returns the anonymous value so callers fall back cleanly.
    fn resolve_blocking(&self) -> minijinja::Value {
        use tokio::runtime::{Handle, RuntimeFlavor};
        let Ok(handle) = Handle::try_current() else {
            return anonymous_user_value();
        };
        if handle.runtime_flavor() == RuntimeFlavor::CurrentThread {
            tracing::warn!(
                "umbral::templates: lazy `user` needs a multi-thread runtime; rendering anonymous"
            );
            return anonymous_user_value();
        }
        let cell = self.cell.clone();
        let resolver = self.resolver.clone();
        tokio::task::block_in_place(move || {
            handle.block_on(async move { cell.get_or_init(|| resolver()).await.clone() })
        })
    }

    /// Wrap this `LazyUser` in a minijinja `Value` proxy that resolves on
    /// first attribute access from inside the synchronous render loop.
    fn into_proxy_value(self) -> minijinja::Value {
        minijinja::Value::from_object(LazyUserProxy(self))
    }
}

/// A minijinja Object proxy that defers resolution of the user until the
/// template actually accesses an attribute (e.g. `{{ user.is_staff }}`).
/// Minijinja calls `get_value` for attribute access — we resolve there, not
/// at context-merge time.
struct LazyUserProxy(LazyUser);

impl std::fmt::Debug for LazyUserProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LazyUserProxy")
    }
}

impl std::fmt::Display for LazyUserProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `{{ user }}` — resolve (memoized) and delegate to the resolved
        // value's own Display so bare rendering is faithful. Uses the same
        // `resolve_blocking` path as `get_value` to ensure at-most-once
        // resolution and the same current-thread / no-runtime fallback.
        let resolved = self.0.resolve_blocking();
        std::fmt::Display::fmt(&resolved, f)
    }
}

impl minijinja::value::Object for LazyUserProxy {
    fn get_value(self: &Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        let resolved = self.0.resolve_blocking();
        resolved.get_item(key).ok()
    }

    fn is_true(self: &Arc<Self>) -> bool {
        // `{% if user %}` — resolve (memoized) and delegate to the resolved
        // value's truthiness so the proxy faithfully represents whether the
        // resolved value is truthy. Uses the same `resolve_blocking` path as
        // `get_value` so resolution is still at most once per request.
        self.0.resolve_blocking().is_true()
    }
}

/// Scope a lazy `user` resolver for the duration of `fut`.
pub async fn with_current_user_lazy<F: Future>(lazy: LazyUser, fut: F) -> F::Output {
    CURRENT_USER_LAZY.scope(lazy, fut).await
}

/// Run `fut` with the ambient template user value scoped to `user`
/// for its duration. Intended for the session-aware layer in
/// `umbral-sessions`; downstream handler code reads the value
/// transparently through [`render`].
pub async fn with_current_user<F: std::future::Future>(
    user: Option<minijinja::Value>,
    fut: F,
) -> F::Output {
    CURRENT_USER.scope(user, fut).await
}

/// Run `fut` with the ambient CSRF token scoped for its duration.
/// Intended for the CSRF middleware in `umbral-security`; downstream
/// handler code reads the value transparently through [`render`]
/// (as `{{ csrf_token }}` / `{{ csrf_input }}`) or [`current_csrf`].
pub async fn with_current_csrf<F: std::future::Future>(token: Option<String>, fut: F) -> F::Output {
    CURRENT_CSRF.scope(token, fut).await
}

/// Read the ambient CSRF token, if a middleware has scoped one for
/// this request. Non-template consumers (e.g. the admin's login form
/// builder) use this to embed the same token the middleware minted,
/// instead of minting their own.
pub fn current_csrf() -> Option<String> {
    CURRENT_CSRF.try_with(|t| t.clone()).ok().flatten()
}

/// Watched template directories captured at `init` time. Stored
/// separately so the dev-mode render path can rebuild the environment
/// from the same sources without re-publishing the OnceLock.
static WATCHED_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
use serde::Serialize;

static ENGINE: OnceLock<Environment<'static>> = OnceLock::new();

/// A plugin-contributed mutation of the template [`Environment`]: adds
/// custom filters, functions, or globals at engine-build time
/// (feature #67 - custom template tags/filters). Returned by
/// `Plugin::template_registrars` and stored process-wide so the dev-mode
/// hot-reload rebuild re-applies it.
///
/// It is `Fn` (not `FnOnce`) on purpose: in dev mode the engine is
/// rebuilt on every template edit, so each registrar runs once per build.
/// Make it owned and `'static` (no borrows of the plugin) so it survives
/// in the [`REGISTRARS`] handle past `App::build`.
pub type TemplateRegistrar = Box<dyn Fn(&mut Environment<'static>) + Send + Sync>;

/// Plugin-contributed [`TemplateRegistrar`]s captured at `init_with` time.
/// Stored separately from [`ENGINE`] so the dev-mode rebuild path (which
/// goes through [`build_env`]) re-applies them without the App builder.
static REGISTRARS: OnceLock<Vec<TemplateRegistrar>> = OnceLock::new();

/// Register the built-in default 404/500 templates into an environment.
///
/// Called from `init` before any disk directories are scanned. The names
/// use the `__umbral__/` prefix so they can never collide with a user's
/// `templates/` directory (slashes aren't meaningful to the engine's name
/// lookup — `__umbral__/default_404.html` is just a unique string key).
///
/// Because the user's disk directories are added after this call and
/// first-match-wins is enforced by the `seen` set, a user who places a
/// file named `__umbral__/default_404.html` in their own templates dir will
/// silently replace the built-in — which is the intended escape hatch.
/// (Callers who want a cleaner opt-out should use
/// `App::builder().disable_default_error_pages()` instead.)
/// gaps2 #21 — register the `img` MiniJinja filter that turns a URL
/// into a fully-formed, performance-correct `<img>` tag.
///
/// Filter signature:
///   `{{ url | img(alt="…", width=N, height=N, class="…") }}`
///
/// Output shape:
///   `<img src="<url>" alt="<alt>" loading="lazy" decoding="async"
///        width="<w>" height="<h>" class="<class>">`
///
/// Why this set of attributes:
/// - `loading="lazy"` — the gap's primary ask. Browsers defer
///   off-viewport image fetches until they're about to be needed,
///   shrinking LCP + initial bandwidth.
/// - `decoding="async"` — lets the browser decode the image off
///   the main thread; prevents render-blocking decode work on
///   slower devices.
/// - explicit `width`/`height` (when provided) reserves layout
///   space immediately so lazy-loading doesn't cause CLS
///   (cumulative layout shift). Omitted if either is missing.
/// - empty `alt=""` default is screen-reader-friendly for
///   decorative images. Callers SHOULD pass a real `alt` for
///   meaningful content images.
///
/// What's NOT included on day one (deferred to a later slice):
/// - `srcset` for responsive resolutions — needs the on-the-fly
///   resize handler (gap 21 Option C) before the filter knows
///   real asset dimensions.
/// - `<picture>` with `webp`/`avif` sources — same blocker; the
///   transcode endpoint has to exist first.
///
/// Output is wrapped in `minijinja::value::Value::from_safe_string`
/// so MiniJinja's autoescape doesn't double-escape the `<` / `>`
/// characters — the attribute values themselves still go through
/// `html_escape` so a hostile alt-text can't break out of the
/// attribute quote.
/// True when `url` is safe to place in an `<img src>`: a relative URL
/// (no scheme) or an `http`/`https` absolute URL. Any other scheme
/// (`javascript:`, `data:`, `vbscript:`, …) is rejected. Fails closed:
/// a malformed scheme (embedded control chars, spaces) is also rejected.
fn url_scheme_is_safe(url: &str) -> bool {
    let trimmed = url.trim();
    // A URL scheme is the run before the first ':' — but only if no
    // '/', '?', '#' appears first (those mean a relative path/query).
    let mut scheme_end = None;
    for (i, c) in trimmed.char_indices() {
        match c {
            ':' => {
                scheme_end = Some(i);
                break;
            }
            '/' | '?' | '#' => break,
            _ => {}
        }
    }
    let Some(end) = scheme_end else {
        return true; // no scheme → relative URL → safe
    };
    let scheme = &trimmed[..end];
    // A real scheme is alpha then [a-z0-9+.-]*. Anything else is suspicious.
    let mut chars = scheme.chars();
    let well_formed = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    if !well_formed {
        return false;
    }
    let lower = scheme.to_ascii_lowercase();
    lower == "http" || lower == "https"
}

fn register_img_filter(env: &mut Environment<'static>) {
    env.add_filter(
        "img",
        |url: String,
         kwargs: minijinja::value::Kwargs|
         -> Result<minijinja::Value, minijinja::Error> {
            let alt: String = kwargs.get::<Option<String>>("alt")?.unwrap_or_default();
            let width: Option<i64> = kwargs.get("width")?;
            let height: Option<i64> = kwargs.get("height")?;
            let class: Option<String> = kwargs.get("class")?;
            // Accept the extras even when the call doesn't pass them
            // — kwargs.get returns Ok(None) for absent keys but
            // .assert_all_used() at the end will catch a typo'd
            // `alt_text` so the user gets a clear error instead of
            // silent drop. Matches the rest of the framework's
            // strict-input posture.
            kwargs.assert_all_used()?;

            // Defense-in-depth: never emit a `javascript:` / `data:` /
            // other non-http(s) scheme into `src`. Not a live XSS (browsers
            // don't run JS from `<img src>` and the value is HTML-escaped),
            // but a hostile stored URL has no business here. A disallowed
            // scheme neutralises to an empty src (broken image) rather than
            // erroring the whole page on user data.
            let url = if url_scheme_is_safe(&url) {
                url
            } else {
                String::new()
            };

            let mut out = String::with_capacity(url.len() + 128);
            out.push_str("<img src=\"");
            html_escape_into(&mut out, &url);
            out.push_str("\" alt=\"");
            html_escape_into(&mut out, &alt);
            out.push_str("\" loading=\"lazy\" decoding=\"async\"");
            if let Some(w) = width {
                out.push_str(" width=\"");
                out.push_str(&w.to_string());
                out.push('"');
            }
            if let Some(h) = height {
                out.push_str(" height=\"");
                out.push_str(&h.to_string());
                out.push('"');
            }
            if let Some(c) = class {
                if !c.is_empty() {
                    out.push_str(" class=\"");
                    html_escape_into(&mut out, &c);
                    out.push('"');
                }
            }
            out.push('>');
            Ok(minijinja::Value::from_safe_string(out))
        },
    );
}

/// features.md #4 — register the `markdown` filter that turns a
/// CommonMark + GFM string into sanitized HTML.
///
/// Filter signature: `{{ body | markdown }}`.
///
/// Pipeline:
/// 1. `pulldown-cmark` parses the input with GFM extensions on
///    (tables, strikethrough, task lists, footnotes) and renders to
///    HTML.
/// 2. `ammonia` sanitizes that HTML — strips `<script>`, inline event
///    handlers (`onerror=`, `onclick=`), `javascript:` URLs, and any
///    tag/attribute outside its safe allowlist. This is the security
///    boundary: user-supplied markdown (plugin bodies, usage docs,
///    reviews) is rendered, never trusted.
/// 3. The result is wrapped in `Value::from_safe_string` so MiniJinja's
///    autoescape emits the generated tags as markup instead of
///    re-escaping them into `&lt;...&gt;`.
///
/// Why sanitize after rendering rather than trusting the parser: raw
/// HTML embedded in a markdown source (`<script>...`) passes straight
/// through pulldown-cmark by design. ammonia is the layer that makes
/// "render whatever the user typed" safe.
///
/// Deferred (separate slices): syntax highlighting on fenced code
/// blocks (ammonia strips the `language-*` class today) and a
/// configurable allowlist for embeds — see the gap entries.
/// Register the global `static()` template function so templates can
/// write `{{ static("admin/admin.css") }}` and get back a URL prefixed
/// with the configured `static_url`.
///
/// `static_url` is captured into the closure when the environment is
/// built (rather than read per-call) — the value is fixed for the
/// process at `App::build()` time, and minijinja functions can't reach
/// the ambient `Settings` directly. The dev-mode render path rebuilds
/// the env per render via [`build_env`], so a `static_url` change would
/// be picked up there too; in practice it never changes at runtime.
///
/// Resolution joins `static_url` and the argument with exactly one
/// slash: a leading slash on the argument (`static("/admin/x")`) is
/// trimmed so the result never double-slashes. With the default
/// `static_url = "/static/"`, `static("admin/admin.css")` yields
/// `"/static/admin/admin.css"`; with a CDN origin
/// `static_url = "https://cdn.example.com/s/"` it yields
/// `"https://cdn.example.com/s/admin/admin.css"`.
fn register_static_function(env: &mut Environment<'static>, static_url: String) {
    env.add_function("static", move |path: String| -> String {
        // Route through the manifest-aware resolver so a `--hashed`
        // collect makes `{{ static("css/app.css") }}` emit the
        // content-hashed URL. The captured `static_url` is the fixed
        // prefix; the manifest lookup is the only per-call ambient read.
        if let Some(hashed) = crate::static_files::manifest_lookup(&path) {
            return join_static_url(&static_url, hashed);
        }
        join_static_url(&static_url, &path)
    });
}

/// Join a `static_url` prefix and an asset path with exactly one slash.
///
/// `static_url` is normalised to end in a slash by [`crate::settings`];
/// the asset path may or may not lead with one, so its leading slash is
/// trimmed before the join. With `static_url = "/static/"`,
/// `join_static_url(.., "admin/admin.css")` yields
/// `"/static/admin/admin.css"`.
fn join_static_url(static_url: &str, path: &str) -> String {
    format!("{}{}", static_url, path.trim_start_matches('/'))
}

/// Resolve an asset path against the ambient `static_url`, mirroring the
/// `static()` template global outside a minijinja render.
///
/// Plugins that build their own minijinja [`Environment`] (the admin
/// engine, for one) call this to register an equivalent `static()`
/// function so their templates can write `{{ static("admin/admin.css") }}`
/// and resolve through the same unified static pipeline URL as the core
/// engine. Reads `static_url` from ambient [`crate::settings`], defaulting
/// to `/static/` when settings aren't initialised yet (bare unit tests).
pub fn resolve_static_url(path: &str) -> String {
    let static_url = crate::settings::get_opt()
        .map(|s| s.static_url.clone())
        .unwrap_or_else(|| "/static/".to_string());

    // Manifest cache-busting (hashed static-file storage): when
    // `collectstatic --hashed` has run, a `staticfiles.json` maps the
    // logical path the template wrote (`css/app.css`) to its
    // content-hashed name (`css/app.<hash>.css`). Resolving to the hashed
    // URL lets the asset carry far-future cache headers — the hash in the
    // name changes whenever the bytes do, so a stale cache can never mask
    // a new build. When no manifest is loaded (no `--hashed` run), the
    // lookup misses and we serve the plain path exactly as before.
    if let Some(hashed) = crate::static_files::manifest_lookup(path) {
        return join_static_url(&static_url, hashed);
    }

    join_static_url(&static_url, path)
}

/// Register the global `media_url()` template function so a template can
/// write `{{ media_url(plugin.logo) }}` and get back the public URL for a
/// stored file/image KEY, resolved through the ambient
/// [`crate::storage::Storage`] backend.
///
/// Mirrors the `static()` global ([`register_static_function`]) but for
/// user-uploaded media instead of developer-shipped assets:
/// `ImageField` / `FileField` serialize as the bare storage key, so
/// `{{ media_url(plugin.logo) }}` (where `plugin.logo` is the key string)
/// resolves to the storage backend's public URL.
///
/// - An empty key yields the empty string (the surrounding `{% if %}`
///   guard skips the markup).
/// - With no `Storage` backend registered, the raw key falls through
///   unchanged.
/// - A `None`/optional field serializes to null, which the template's
///   `{% if %}` guard handles before the helper is ever called.
fn register_media_url_function(env: &mut Environment<'static>) {
    env.add_function("media_url", |key: String| -> String {
        if key.is_empty() {
            return String::new();
        }
        crate::storage::storage_opt()
            .map(|s| s.url(&key))
            .unwrap_or(key)
    });
}

/// Register the `{{ querystring_with(current_query, key, value) }}` global
/// (gaps/features #65 — template pagination). Rebuilds a querystring
/// replacing one key while preserving every other parameter, the fiddly bit
/// behind a pagination nav that has to carry `?sort=name` across every
/// `?page=N` link. Backed by [`crate::pagination::querystring_with`] so the
/// encode/replace logic stays in one place and is unit-tested there. The
/// returned string has no leading `?`; the template prepends one.
fn register_querystring_with_function(env: &mut Environment<'static>) {
    env.add_function(
        "querystring_with",
        // `value` is a `minijinja::Value`, not a `String`: the nav passes
        // `page.next_page_number` / `item.n`, which are integers, and
        // minijinja does NOT auto-coerce an int arg into a `String`
        // parameter — it'd raise a type error at render. Accepting `Value`
        // and stringifying covers ints, strings, and bools uniformly.
        |current_query: String, key: String, value: minijinja::Value| -> String {
            crate::pagination::querystring_with(&current_query, &key, &value.to_string())
        },
    );
}

fn register_markdown_filter(env: &mut Environment<'static>) {
    env.add_filter("markdown", |input: String| -> minijinja::Value {
        minijinja::Value::from_safe_string(render_markdown(&input))
    });
}

/// Register the `{{ highlight_styles() }}` global: emits the generated
/// `base16-ocean.dark` token stylesheet wrapped in a `<style>` block, for a
/// base template to drop into `<head>` once. The CSS is a safe string
/// (generated by syntect from a fixed theme, no user input), so it is
/// marked safe to skip minijinja autoescape.
fn register_highlight_styles_function(env: &mut Environment<'static>) {
    env.add_function("highlight_styles", || -> minijinja::Value {
        minijinja::Value::from_safe_string(format!("<style>{}</style>", highlight_css()))
    });
}

/// features.md #67 — `{{ now() }}` / `{{ now("%Y-%m-%d") }}`. Renders the
/// current UTC time, optionally via a chrono `strftime` format string.
/// With no argument it emits RFC 3339 (e.g. `2026-06-13T10:30:00+00:00`).
/// The reference built-in tag for the custom-tag surface.
fn register_now_function(env: &mut Environment<'static>) {
    env.add_function("now", |fmt: Option<String>| -> String {
        let now = chrono::Utc::now();
        match fmt {
            Some(f) if !f.is_empty() => now.format(&f).to_string(),
            _ => now.to_rfc3339(),
        }
    });
}

/// features.md #67 — `{{ price | currency }}` / `{{ price | currency("EUR") }}`.
/// Formats a number as money: two decimals, thousands grouping, and a
/// leading symbol for the common ISO codes (USD/EUR/GBP/JPY); an unknown
/// code falls back to `1,234.56 CODE`. The reference built-in filter.
fn register_currency_filter(env: &mut Environment<'static>) {
    env.add_filter("currency", |amount: f64, code: Option<String>| -> String {
        let code = code.unwrap_or_else(|| "USD".to_string());
        let symbol = match code.as_str() {
            "USD" | "AUD" | "CAD" | "NZD" => "$",
            "EUR" => "€",
            "GBP" => "£",
            "JPY" | "CNY" => "¥",
            "KES" => "KSh ",
            _ => "",
        };
        // Sign goes outside the symbol: -$12.40, not $-12.40.
        let sign = if amount < 0.0 { "-" } else { "" };
        let body = group_thousands(amount.abs());
        if symbol.is_empty() {
            format!("{sign}{body} {code}")
        } else {
            format!("{sign}{symbol}{body}")
        }
    });
}

/// Format a float with two decimals and comma thousands separators on the
/// integer part: `1234567.5 -> "1,234,567.50"`, `-12.4 -> "-12.40"`.
fn group_thousands(amount: f64) -> String {
    let negative = amount.is_sign_negative() && amount != 0.0;
    let formatted = format!("{:.2}", amount.abs());
    let (int_part, frac_part) = formatted.split_once('.').unwrap_or((&formatted, "00"));

    let mut grouped = String::new();
    let digits: Vec<char> = int_part.chars().collect();
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*ch);
    }
    format!("{}{grouped}.{frac_part}", if negative { "-" } else { "" })
}

/// features.md #4 — register the `sanitize` filter: clean a string of
/// HTML (e.g. the output of the admin's RTE widget, which stores
/// HTML rather than markdown) down to ammonia's safe allowlist and
/// hand it to the template as a safe string.
///
/// `{{ body | sanitize }}` is the display companion to the `rte`
/// widget the way `{{ body | markdown }}` is to the `markdown` widget:
/// the stored value is HTML, so it's sanitized — never trusted — before
/// it reaches the page. A value tampered with via the REST write path
/// (which doesn't go through the editor) is made safe here.
fn register_sanitize_filter(env: &mut Environment<'static>) {
    env.add_filter("sanitize", |input: String| -> minijinja::Value {
        minijinja::Value::from_safe_string(sanitize_html(&input))
    });
}

/// Clean `input` HTML down to ammonia's safe allowlist (strips
/// `<script>`, event handlers, `javascript:` URLs, etc.). The
/// non-markdown sibling of [`render_markdown`] — use it on stored HTML
/// (the RTE widget's output).
pub fn sanitize_html(input: &str) -> String {
    ammonia::clean(input)
}

/// The class prefix syntect token spans carry (`hl-keyword`, `hl-string`,
/// `hl-source`, …). Shared by the highlighter and the generated
/// stylesheet so the two never drift.
const HL_PREFIX: &str = "hl-";

fn hl_class_style() -> ClassStyle {
    ClassStyle::SpacedPrefixed { prefix: HL_PREFIX }
}

/// The bundled syntect syntax set, loaded once. The load parses a binary
/// dump and is expensive, so it is cached for the life of the process.
fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The `base16-ocean.dark` token stylesheet, generated once from syntect's
/// bundled theme with the `hl-` class prefix. This is the single source of
/// truth for token colors — the markdown highlighter emits matching
/// classes. Returns `""` only if syntect cannot generate the CSS (it
/// always can for a bundled theme), so callers never need to handle an
/// error.
pub fn highlight_css() -> &'static str {
    static HIGHLIGHT_CSS: OnceLock<String> = OnceLock::new();
    HIGHLIGHT_CSS
        .get_or_init(|| {
            let themes = ThemeSet::load_defaults();
            match themes.themes.get("base16-ocean.dark") {
                Some(theme) => {
                    css_for_theme_with_class_style(theme, hl_class_style()).unwrap_or_default()
                }
                None => String::new(),
            }
        })
        .as_str()
}

/// Return `true` iff every character in a fence info token is safe to
/// embed verbatim in a `class="language-…"` HTML attribute value.
///
/// A legitimate language token is just a word: `rust`, `c++`, `c#`,
/// `shell`, `text/plain`, etc. It never needs `<`, `>`, `"`, `'`, `=`,
/// backticks, or whitespace. Rejecting those characters closes the
/// class-injection vector that would otherwise let a hostile fence like
/// `` ```<script>alert(1)</script> `` survive ammonia's pass (ammonia
/// allows `class` on `<code>` but does not filter the attribute VALUE).
fn fence_lang_is_safe(lang: &str) -> bool {
    !lang.is_empty()
        && lang.len() <= 64
        && lang.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '+' | '-' | '_' | '.' | '#' | '/' | '@')
        })
}

/// Render one fenced code block to safe HTML. `lang` is the fence info
/// token (`Some("rust")`) or `None` for an unlabelled / indented block.
/// With a known language the body is syntect-highlighted into `hl-` token
/// spans; otherwise — or on any highlighter error — it falls back to a
/// plain escaped block that still carries `class="language-…"` so the
/// `md-enhance.js` label keeps working. Never panics, never drops the
/// user's code.
fn highlight_code_block(lang: Option<&str>, src: &str) -> String {
    // Validate the lang token before touching it. A hostile fence info
    // string (e.g. `<script>alert(1)</script>`) must never land in the
    // `class="language-…"` attribute value even after HTML-escaping,
    // because ammonia re-parses the tree and may not re-escape `<`/`>`
    // that appear inside attribute values of allowed elements. Treating
    // an unsafe token as `None` produces a plain unlabelled code block
    // (still safe and still readable) rather than a class-injection path.
    let lang = lang.filter(|l| fence_lang_is_safe(l));

    let ss = syntax_set();
    let syntax = lang.and_then(|l| {
        ss.find_syntax_by_token(l)
            .or_else(|| ss.find_syntax_by_extension(l))
    });
    if let Some(syntax) = syntax {
        let mut generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, ss, hl_class_style());
        let mut ok = true;
        for line in LinesWithEndings::from(src) {
            if generator
                .parse_html_for_line_which_includes_newline(line)
                .is_err()
            {
                ok = false;
                break;
            }
        }
        if ok {
            // `finalize()` returns safe `<span class="hl-…">` markup —
            // pass it through unescaped.
            return wrap_code_block(lang, &generator.finalize());
        }
    }
    // Fallback: escape the raw text so it is inert, then wrap.
    let mut escaped = String::with_capacity(src.len());
    html_escape_into(&mut escaped, src);
    wrap_code_block(lang, &escaped)
}

/// Wrap inner code HTML (token spans, or escaped plain text) in
/// `<pre><code class="language-…">` so the md-enhance frame + language
/// label attach. The language token is HTML-escaped before it lands in the
/// class value (it comes straight from the fence info string).
fn wrap_code_block(lang: Option<&str>, inner: &str) -> String {
    let mut out = String::with_capacity(inner.len() + 48);
    out.push_str("<pre><code");
    if let Some(l) = lang {
        out.push_str(" class=\"language-");
        html_escape_into(&mut out, l);
        out.push('"');
    }
    out.push('>');
    out.push_str(inner);
    out.push_str("</code></pre>");
    out
}

/// Render CommonMark + GFM `input` to sanitized HTML. Pulled out of the
/// filter closure so it's unit-testable and reusable by any future
/// Rust-side caller (e.g. a REST endpoint that returns pre-rendered
/// HTML).
pub fn render_markdown(input: &str) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(input, options);

    // Rewrite the event stream: replace each code block with a single
    // pre-highlighted Html event. The fence info token selects the syntect
    // syntax; everything else passes through unchanged.
    let mut events: Vec<Event> = Vec::new();
    let mut in_code = false;
    let mut code_lang: Option<String> = None;
    let mut code_buf = String::new();
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                let highlighted = highlight_code_block(code_lang.as_deref(), &code_buf);
                events.push(Event::Html(highlighted.into()));
            }
            Event::Text(text) if in_code => code_buf.push_str(&text),
            other => events.push(other),
        }
    }

    let mut rendered = String::new();
    html::push_html(&mut rendered, events.into_iter());

    // Sanitize. `pre`/`code`/`span` are already default-allowed tags, so we
    // widen the allowlist by exactly one inert attribute — `class` on those
    // three — letting syntect's `hl-` token spans and the `language-*` label
    // survive. style / on* handlers / javascript: URLs stay stripped: this is
    // the whole "safely" surface. Built per call: ammonia::Builder isn't Sync
    // (boxed attribute_filter), so it can't be a shared static without a Mutex
    // that would serialize rendering; this costs the same as ammonia::clean.
    let mut cleaner = ammonia::Builder::default();
    cleaner.add_tag_attributes("pre", &["class"]);
    cleaner.add_tag_attributes("code", &["class"]);
    cleaner.add_tag_attributes("span", &["class"]);
    cleaner.clean(&rendered).to_string()
}

/// Tiny HTML attribute-value escape — covers the four characters
/// that can break out of a double-quoted attribute context.
/// Centralised here because the framework doesn't otherwise need
/// to ship an html_escape crate dep just for the img filter.
fn html_escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

fn register_default_templates(
    env: &mut Environment<'static>,
    seen: &mut std::collections::HashSet<String>,
) {
    let entries = [
        (
            crate::errors::DEFAULT_404_TEMPLATE_NAME,
            crate::errors::DEFAULT_404_HTML,
        ),
        (
            crate::errors::DEFAULT_500_TEMPLATE_NAME,
            crate::errors::DEFAULT_500_HTML,
        ),
    ];
    for (name, source) in entries {
        if seen.contains(name) {
            continue; // already provided by user — skip
        }
        // These are compile-time constants so they're `&'static str`; we can
        // add them without cloning via `add_template` (non-owned variant).
        if env.add_template(name, source).is_ok() {
            seen.insert(name.to_string());
        }
    }
}

/// Publish the template engine into the process-wide ambient handle.
///
/// `dirs` is the ordered list of directories to search — the first
/// entry is searched first (highest priority). Typically this is:
/// `[app_templates_dir, plugin_a_dir, plugin_b_dir, ...]`.
///
/// For each directory in order, every `.html` / `.htm` / `.txt` file is
/// registered under its path-relative-to-that-dir name. If a name was
/// already registered by an earlier directory, the later file is skipped
/// and a `tracing::warn!` is emitted so the collision is visible.
///
/// If none of the directories exist, init succeeds with an empty engine.
/// This is the right default for binaries that don't render HTML.
///
/// Returns the list of template names that collided (appeared in more
/// than one directory). The caller (`App::build`) logs these via tracing.
/// Tests can inspect the returned list to assert collision detection
/// without needing a tracing subscriber.
pub fn init(dirs: &[PathBuf]) -> Result<Vec<String>, TemplateError> {
    let (env, collisions) = build_env(dirs)?;

    for name in &collisions {
        tracing::warn!(
            template = %name,
            "umbral templates: template `{name}` is provided by multiple directories; \
             the first-registered copy wins"
        );
    }

    // Stash the dirs so the dev-mode render path can rebuild the env
    // on demand without re-running the (more expensive) init flow.
    let _ = WATCHED_DIRS.set(dirs.to_vec());

    ENGINE
        .set(env)
        .map_err(|_| TemplateError::AlreadyInitialised)?;
    Ok(collisions)
}

/// Like [`init`], but also installs plugin-contributed
/// [`TemplateRegistrar`]s (feature #67). The registrars are stashed in
/// the process-wide [`REGISTRARS`] handle *before* the engine is built so
/// [`build_env`] applies them — both here and on every dev-mode rebuild.
///
/// Called by `App::build` with the flattened registrars from every
/// plugin's `template_registrars()`, in topological order. The plain
/// [`init`] stays the no-plugin entry point used by template unit tests.
pub fn init_with(
    dirs: &[PathBuf],
    registrars: Vec<TemplateRegistrar>,
) -> Result<Vec<String>, TemplateError> {
    // Set even when empty so a second (errant) init can't smuggle in a
    // different registrar set behind the already-published engine.
    let _ = REGISTRARS.set(registrars);
    init(dirs)
}

/// Build a fresh `Environment` from the given dirs. Shared by the
/// init path and the dev-mode hot-reload path; both produce
/// bit-identical engines from the same input.
fn build_env(dirs: &[PathBuf]) -> Result<(Environment<'static>, Vec<String>), TemplateError> {
    let mut env = Environment::new();
    // Autoescape extensions MUST stay in sync with the loader
    // whitelist in `load_directory` (currently `html | htm | txt`).
    // If you add `.svg` or `.xml` to the loader, add them HERE too
    // — `.svg` carries inline-script XSS risk and `.xml` is generally
    // parsed by something downstream that wants attribute escaping.
    // `.txt` stays `None` because plaintext rendering shouldn't HTML-
    // escape (would replace `<` with `&lt;` in plain email bodies).
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") || name.ends_with(".htm") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });

    // gaps2 #21 — register the `img` filter for ergonomic, perf-
    // forward image markup. `{{ url | img(alt="...", width=400,
    // height=300) }}` expands to a fully-formed `<img>` with the
    // hat-trick that catches LCP regressions out of the box:
    // `loading="lazy"`, `decoding="async"`, explicit `width`/
    // `height` to reserve layout space (no CLS), and an `alt`
    // attribute that's empty rather than omitted (screen-reader-
    // friendly default for purely decorative images). Optional
    // `class="..."` flows through for Tailwind / scoped styling.
    register_img_filter(&mut env);

    // `{{ highlight_styles() }}` — the syntect token stylesheet for
    // server-highlighted code, emitted once into <head> by a base template.
    register_highlight_styles_function(&mut env);

    // Unified static pipeline — `{{ static("admin/admin.css") }}`
    // expands to `<static_url>admin/admin.css`. The `static_url` is read
    // from ambient settings (defaulting to `/static/` when settings
    // aren't initialised yet, e.g. in a bare template unit test) and
    // captured into the function closure. See `register_static_function`.
    let static_url = crate::settings::get_opt()
        .map(|s| s.static_url.clone())
        .unwrap_or_else(|| "/static/".to_string());
    register_static_function(&mut env, static_url);

    // `{{ media_url(plugin.logo) }}` resolves a stored file/image KEY
    // through the ambient Storage backend's `url()`, the media-side
    // companion to `static()`. ImageField/FileField serialize as the
    // bare key; this turns it into the public URL. See
    // `register_media_url_function`.
    register_media_url_function(&mut env);

    // features.md #4 — `{{ body | markdown }}` renders user-supplied
    // CommonMark/GFM to sanitized HTML. The reusable "safely show a
    // body/usage field" surface shared by the admin and end-user
    // templates; pairs with `#[umbral(widget = "markdown")]` on the
    // model field that captures the source.
    register_markdown_filter(&mut env);

    // features.md #4 — `{{ html | sanitize }}` cleans stored HTML (the
    // `rte` admin widget's output) to a safe allowlist. The HTML-side
    // companion to the markdown filter.
    register_sanitize_filter(&mut env);

    // gaps2 #19 follow-up — render `None` / `Undefined` as the
    // empty string instead of the literal "none" / "undefined" tokens
    // MiniJinja defaults to. Bug screenshot 2026-06-10 01-08-30: an
    // `Option<String>` model field with `value=None` rendered into
    // `<input value="{{ form.phone }}">` produced `value="none"` on a
    // fresh form, which the user then has to manually clear before
    // typing. Every form with optional fields hit this footgun.
    //
    // Defining a custom formatter is the framework-level fix — every
    // template (admin, shop, plugins) inherits the new behaviour
    // automatically. Non-null/non-undefined values pass through the
    // default formatter unchanged so HTML escaping, number / bool /
    // string rendering, and safe-string handling stay identical.
    env.set_formatter(|out, state, value| {
        if value.is_none() || value.is_undefined() {
            return Ok(());
        }
        minijinja::escape_formatter(out, state, value)
    });

    // features.md #67 — built-in example tags/filters. These ship as the
    // reference implementations for the custom-tag surface: `now()` for a
    // server-rendered timestamp, `currency` for money formatting. Plugins
    // add their own via `Plugin::template_registrars` (applied below).
    register_now_function(&mut env);
    register_currency_filter(&mut env);

    // features #65 — `{{ querystring_with(base_query, "page", item.n) }}`
    // rebuilds the current querystring replacing one key, so the bundled
    // `_pagination.html` nav carries `?sort=...` filters across every
    // `?page=N` link. See `register_querystring_with_function`.
    register_querystring_with_function(&mut env);

    // features.md #67 — plugin-contributed filters/functions. Applied
    // AFTER the built-ins so a plugin can deliberately override one by
    // re-registering the same name (minijinja's add_* overwrites). Runs
    // on every rebuild (dev hot-reload) because `Fn`, not `FnOnce`.
    if let Some(registrars) = REGISTRARS.get() {
        for registrar in registrars {
            registrar(&mut env);
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut collisions: Vec<String> = Vec::new();

    // Register the built-in default error templates before scanning disk
    // directories. Because disk directories are first-match-wins and are
    // scanned after this call, a user template with the same name (unlikely,
    // since the `__umbral__/` prefix is reserved) would silently replace the
    // built-in. Callers who want a clean opt-out should use
    // `App::builder().disable_default_error_pages()`.
    register_default_templates(&mut env, &mut seen);

    for dir in dirs {
        if dir.exists() {
            load_directory(&mut env, dir, dir, &mut seen, &mut collisions)?;
        }
    }

    Ok((env, collisions))
}

/// Render a template by name with a serde-serializable context value.
///
/// The name is the path relative to its templates directory, with
/// forward slashes regardless of host OS. `articles_list.html`,
/// `admin/base.html`, etc.
///
/// Returns `TemplateError::NotInitialised` if `App::build()` hasn't
/// run yet, `TemplateError::Missing` if the name doesn't match a
/// loaded template, and `TemplateError::Render` for any minijinja-
/// reported issue (syntax error, missing variable when strict undefined
/// is on, etc.).
pub fn render<C: Serialize>(name: &str, ctx: &C) -> Result<String, TemplateError> {
    // Dev-mode hot reload: when settings.environment == Dev, rebuild
    // the environment from disk on every render so template edits are
    // picked up without a server restart. This makes the dev loop —
    // edit `home.html`, hit reload, see the change — work without
    // `cargo run`-ing again. Production stays on the cached engine
    // for the fast path.
    //
    // Cost: one disk walk + minijinja parse per render in dev. For a
    // typical handler doing one render per request at ~10 RPS during
    // development, that's negligible. We chose this over per-file
    // stat checks because the per-render rebuild is dependency-free
    // and the staleness window is zero (a save followed instantly
    // by a reload always sees the new content).
    if dev_mode_active() {
        if let Some(dirs) = WATCHED_DIRS.get() {
            // Rebuild fresh; ignore collisions log here (init already
            // logged them once; we don't spam every render).
            match build_env(dirs) {
                Ok((env, _collisions)) => return render_with(&env, name, ctx),
                Err(e) => return Err(e),
            }
        }
    }

    let env = ENGINE.get().ok_or(TemplateError::NotInitialised)?;
    render_with(env, name, ctx)
}

/// Render an inline template source through the ambient-context path.
/// Test/bench helper only.
#[doc(hidden)]
pub fn render_str<C: Serialize>(src: &str, ctx: &C) -> Result<String, TemplateError> {
    let mut env = minijinja::Environment::new();
    env.add_template("__inline", src)
        .map_err(TemplateError::Render)?;
    render_with(&env, "__inline", ctx)
}

/// True when the ambient settings say we're in Dev. Returns false if
/// settings haven't been initialised (production-style binaries that
/// never went through `App::build()`).
fn dev_mode_active() -> bool {
    crate::settings::get_opt()
        .map(|s| matches!(s.environment, crate::settings::Environment::Dev))
        .unwrap_or(false)
}

/// Render a named template against the given env. Extracted so dev-mode
/// (fresh env per render) and prod (cached env) share one error mapping.
fn render_with<C: Serialize>(
    env: &Environment<'_>,
    name: &str,
    ctx: &C,
) -> Result<String, TemplateError> {
    let tmpl = env.get_template(name).map_err(|e| match e.kind() {
        minijinja::ErrorKind::TemplateNotFound => TemplateError::Missing(name.to_string()),
        _ => TemplateError::Render(e),
    })?;
    let merged = merge_ambient_context(ctx);
    tmpl.render(&merged).map_err(TemplateError::Render)
}

/// Merge the ambient task-locals into a serializable template context:
/// `user` (from `CURRENT_USER`) and the CSRF pair `csrf_token` /
/// `csrf_input` (from `CURRENT_CSRF`). The handler's own keys always
/// win — the ambient injection is the default, not an override.
///
/// `user` is injected unconditionally (anonymous fallback below);
/// the CSRF pair only when a middleware actually scoped a token —
/// there is no meaningful fallback token, and rendering an empty
/// hidden input would make a form post a guaranteed-403 silently.
///
/// Most code should use [`render`], which calls this automatically.
/// Plugins that own a private MiniJinja environment can call this before
/// `Template::render` to get the same `{{ user }}`, `{{ csrf_token }}`,
/// and `{{ csrf_input }}` semantics as the framework renderer.
pub fn merge_ambient_context<C: Serialize>(ctx: &C) -> minijinja::Value {
    let ctx_value = minijinja::Value::from_serialize(ctx);
    merge_ambient_value(ctx_value)
}

/// Same as [`merge_ambient_context`], but accepts an already-built
/// MiniJinja [`Value`](minijinja::Value). This is useful for private
/// plugin renderers that build context with `minijinja::context!`.
pub fn merge_ambient_value(ctx_value: minijinja::Value) -> minijinja::Value {
    let has = |key: &str| {
        ctx_value
            .get_attr(key)
            .map(|v| !v.is_undefined())
            .unwrap_or(false)
    };

    let need_user = !has("user");
    let csrf = current_csrf();
    let need_csrf = csrf.is_some() && !(has("csrf_token") && has("csrf_input"));

    if !need_user && !need_csrf {
        return ctx_value;
    }

    // Build a fresh object that contains every original key plus the
    // ambient ones. minijinja's `Value::from_iter` over (key, value)
    // pairs produces a Map value; we walk the original keys and add
    // ours last.
    let mut pairs: Vec<(String, minijinja::Value)> = Vec::new();
    if let Ok(keys) = ctx_value.try_iter() {
        for key in keys {
            let key_str = key.to_string();
            if let Ok(v) = ctx_value.get_item(&key) {
                pairs.push((key_str, v));
            }
        }
    }

    if need_user {
        // Resolve which `user` value should land in the rendered ctx:
        //   1. Task-local set by a middleware (AuthPlugin's
        //      `user_context_layer`) — the live request shape.
        //   2. Anonymous fallback `{ is_authenticated: false }` for
        //      callers WITHOUT a layer mounted AND for renders that
        //      happen outside the middleware's scope (notably the
        //      `render_500_middleware` recovery path — the
        //      user-context task-local has already dropped by the time
        //      the error layer renders, but the 500 template still
        //      needs `user.is_authenticated` to evaluate cleanly).
        //
        // The fallback is the same shape `serialize_anonymous` would
        // produce, kept in core so umbral-auth isn't a dependency of
        // the templates module.
        // Prefer the lazy channel (proxy defers resolution until attribute access),
        // then the eager task-local, then the anonymous fallback.
        let user_value = if let Ok(lazy) = CURRENT_USER_LAZY.try_with(|lazy| lazy.clone()) {
            lazy.into_proxy_value()
        } else if let Some(v) = CURRENT_USER.try_with(|u| u.clone()).ok().flatten() {
            v
        } else {
            anonymous_user_value()
        };
        pairs.push(("user".to_string(), user_value));
    }

    if let Some(token) = csrf {
        if !has("csrf_token") {
            pairs.push((
                "csrf_token".to_string(),
                minijinja::Value::from(token.clone()),
            ));
        }
        if !has("csrf_input") {
            // Today's tokens are hex (signed mode adds `.` + hex sig),
            // so the escape is belt-and-braces against a future
            // token-shape change — not a live attack surface.
            let escaped = token
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            pairs.push((
                "csrf_input".to_string(),
                minijinja::Value::from_safe_string(format!(
                    r#"<input type="hidden" name="csrf_token" value="{escaped}">"#
                )),
            ));
        }
    }

    minijinja::Value::from_iter(pairs)
}

/// Anonymous-user sentinel — the value `user` resolves to in
/// templates rendered outside an authenticated context (no auth
/// middleware, anonymous request, or the 500-rendering path
/// where the middleware's task-local has already dropped).
/// Carries only `{ is_authenticated: false }` — enough for
/// `{% if user.is_authenticated %}` / `{% if user.is_staff %}`
/// to evaluate to false without `umbral templates: undefined
/// value` errors that would otherwise mask the original failure.
fn anonymous_user_value() -> minijinja::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(false),
    );
    // is_staff / is_superuser default to false too so a template
    // gating on either doesn't accidentally render the privileged
    // branch when `user` is the anonymous fallback.
    map.insert("is_staff".to_string(), serde_json::Value::Bool(false));
    map.insert("is_superuser".to_string(), serde_json::Value::Bool(false));
    minijinja::Value::from_serialize(serde_json::Value::Object(map))
}

/// Walk a directory recursively and register every `.html` / `.htm` /
/// `.txt` file as a template under its path-relative-to-root name.
/// Subdirectories are reachable via forward-slash names: `admin/base.html`.
///
/// `seen` tracks which names have already been registered across all
/// directories. When a name collision is detected (a later directory
/// ships a template with the same relative name as an earlier one),
/// the duplicate is skipped and the name is appended to `collisions`.
/// First-match-wins.
fn load_directory(
    env: &mut Environment<'static>,
    root: &Path,
    dir: &Path,
    seen: &mut HashSet<String>,
    collisions: &mut Vec<String>,
) -> Result<(), TemplateError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            load_directory(env, root, &path, seen, collisions)?;
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "html" | "htm" | "txt") {
            continue;
        }
        let rel: PathBuf = path
            .strip_prefix(root)
            .expect("walked path is rooted at the templates dir")
            .to_path_buf();
        // minijinja template names are forward-slashed regardless of OS;
        // the path display would emit `\` on Windows, so build the name
        // explicitly.
        let name: String = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");

        if seen.contains(&name) {
            // Collision: a higher-priority directory already registered
            // this name. Record it and skip; init will log after all
            // dirs are processed.
            if !collisions.contains(&name) {
                collisions.push(name.clone());
            }
            continue;
        }

        let source = std::fs::read_to_string(&path)?;
        env.add_template_owned(name.clone(), source)
            .map_err(TemplateError::Render)?;
        seen.insert(name);
    }
    Ok(())
}

/// Errors the template engine can produce. Narrow at v1: load-time IO,
/// engine-not-ready, missing template, render-time minijinja error.
#[derive(Debug)]
pub enum TemplateError {
    /// `App::build()` hasn't run yet, so the ambient engine isn't set.
    NotInitialised,
    /// `init` was called twice — a programming error in the framework
    /// itself, not the user. Surfaced as a `BuildError` if it ever fires.
    AlreadyInitialised,
    /// IO error reading a template file at boot.
    Io(std::io::Error),
    /// The requested template name isn't loaded.
    Missing(String),
    /// Any other minijinja error (syntax, render-time, etc.). The
    /// inner `minijinja::Error` carries the diagnostic (line / col /
    /// undefined name) so the caller can pass it through `Display`.
    Render(minijinja::Error),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::NotInitialised => write!(
                f,
                "umbral templates: engine not initialised — call App::build() first"
            ),
            TemplateError::AlreadyInitialised => {
                write!(f, "umbral templates: init called more than once")
            }
            TemplateError::Io(e) => write!(f, "umbral templates: io: {e}"),
            TemplateError::Missing(name) => write!(
                f,
                "umbral templates: no template named `{name}`; check the templates directory"
            ),
            TemplateError::Render(e) => write!(f, "umbral templates: {e}"),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<std::io::Error> for TemplateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn img_url_scheme_safety() {
        // Relative + http(s) are allowed.
        assert!(url_scheme_is_safe("/media/cat.png"));
        assert!(url_scheme_is_safe("cat.png"));
        assert!(url_scheme_is_safe("../up/cat.png"));
        assert!(url_scheme_is_safe("http://example.com/cat.png"));
        assert!(url_scheme_is_safe("https://example.com/cat.png"));
        assert!(url_scheme_is_safe("HTTPS://EXAMPLE.com/cat.png"));
        assert!(url_scheme_is_safe("?query=only"));
        assert!(url_scheme_is_safe("#fragment"));
        // Dangerous / non-http schemes are rejected.
        assert!(!url_scheme_is_safe("javascript:alert(1)"));
        assert!(!url_scheme_is_safe("  javascript:alert(1)"));
        assert!(!url_scheme_is_safe("JaVaScRiPt:alert(1)"));
        assert!(!url_scheme_is_safe(
            "data:text/html,<script>alert(1)</script>"
        ));
        assert!(!url_scheme_is_safe("vbscript:msgbox(1)"));
        assert!(!url_scheme_is_safe("mailto:a@b.com"));
        // Malformed scheme (embedded control char) fails closed.
        assert!(!url_scheme_is_safe("java\u{0}script:alert(1)"));
    }

    #[test]
    fn img_filter_neutralises_javascript_url() {
        let mut env = minijinja::Environment::new();
        register_img_filter(&mut env);
        env.add_template("t", "{{ url | img }}").unwrap();
        let tmpl = env.get_template("t").unwrap();
        let out = tmpl
            .render(minijinja::context! { url => "javascript:alert(1)" })
            .unwrap();
        assert!(
            !out.contains("javascript:"),
            "javascript: URL must be neutralised; got {out}"
        );
        assert!(out.contains("src=\"\""), "expected empty src; got {out}");
    }

    #[test]
    fn nested_template_names_are_relative_to_templates_root() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let templates = tmp.path().join("templates");
        std::fs::create_dir_all(templates.join("base")).expect("create base template dir");
        std::fs::create_dir_all(templates.join("content")).expect("create content template dir");

        std::fs::write(
            templates.join("base").join("site.html"),
            "<main>{% block content %}{% endblock %}</main>",
        )
        .expect("write nested base template");
        std::fs::write(
            templates.join("content").join("contact.html"),
            r#"{% extends "base/site.html" %}{% block content %}<h1>{{ title }}</h1><p>Contact from nested content.</p>{% endblock %}"#,
        )
        .expect("write nested content template");

        let (env, collisions) = build_env(&[templates]).expect("build template env");
        assert!(collisions.is_empty());

        let rendered = render_with(
            &env,
            "content/contact.html",
            &json!({ "title": "Nested contact" }),
        )
        .expect("render nested template by relative name");

        assert!(rendered.contains("<main>"));
        assert!(rendered.contains("<h1>Nested contact</h1>"));
        assert!(rendered.contains("Contact from nested content."));
    }

    /// Render `{{ static(arg) }}` against an env whose `static()` was
    /// registered with the given `static_url`. Exercises the helper
    /// directly without needing the ambient `Settings` OnceLock (which
    /// can't be set under cargo's parallel test runner).
    fn render_static(static_url: &str, arg: &str) -> String {
        let mut env = Environment::new();
        register_static_function(&mut env, static_url.to_string());
        env.add_template("t.txt", "{{ static(arg) }}")
            .expect("add template");
        let tmpl = env.get_template("t.txt").expect("get template");
        tmpl.render(json!({ "arg": arg })).expect("render")
    }

    #[test]
    fn static_helper_prepends_root_relative_url() {
        assert_eq!(
            render_static("/static/", "admin/admin.css"),
            "/static/admin/admin.css"
        );
    }

    #[test]
    fn static_helper_prepends_cdn_origin() {
        assert_eq!(
            render_static("https://cdn.example.com/s/", "admin/admin.css"),
            "https://cdn.example.com/s/admin/admin.css"
        );
    }

    #[test]
    fn static_helper_does_not_double_slash_on_leading_slash_arg() {
        assert_eq!(render_static("/static/", "/admin/x"), "/static/admin/x");
    }

    #[test]
    fn highlight_css_contains_hl_rules() {
        let css = highlight_css();
        assert!(!css.is_empty(), "generated theme CSS should not be empty");
        assert!(
            css.contains(".hl-"),
            "theme CSS must target hl- classes: {css}"
        );
    }

    #[test]
    fn fenced_rust_block_gets_syntect_token_spans() {
        let html = render_markdown("```rust\nfn main() {}\n```\n");
        assert!(
            html.contains("language-rust"),
            "keeps the language class for the md-enhance label: {html}"
        );
        assert!(
            html.contains("class=\"hl-"),
            "emits syntect hl- token spans: {html}"
        );
    }

    #[test]
    fn script_in_code_fence_is_escaped_not_executed() {
        let html = render_markdown("```\n<script>alert(1)</script>\n```\n");
        assert!(!html.contains("<script>"), "no live script tag: {html}");
        assert!(
            html.contains("&lt;script&gt;"),
            "rendered as inert text: {html}"
        );
    }

    #[test]
    fn prose_script_is_still_stripped() {
        let html = render_markdown("hello <script>alert(1)</script> world");
        assert!(!html.contains("<script>"), "prose script stripped: {html}");
    }

    #[test]
    fn markdown_allows_class_but_not_style() {
        let html = render_markdown("<span class=\"x\" style=\"color:red\">hi</span>");
        assert!(html.contains("class=\"x\""), "class survives: {html}");
        assert!(!html.contains("style="), "style stripped: {html}");
    }

    #[test]
    fn unknown_and_plain_fences_do_not_panic() {
        let unknown = render_markdown("```notalanguage\nx := 1\n```\n");
        let plain = render_markdown("```\nplain text\n```\n");
        assert!(
            unknown.contains("<pre><code"),
            "unknown lang block: {unknown}"
        );
        assert!(plain.contains("<pre><code"), "plain block: {plain}");
        assert!(
            unknown.contains("language-notalanguage"),
            "unknown lang still labelled: {unknown}"
        );
    }

    /// Security: a hostile fence info token (e.g. `<script>alert(1)</script>`)
    /// must NOT appear as a live tag in the output. `wrap_code_block` HTML-escapes
    /// the lang token before inserting it into the class attribute value, and
    /// ammonia's builder only permits `class` on `<code>` — it does not allow
    /// arbitrary attributes or values. So a `<script>` info string is inert.
    ///
    /// Also asserts that the SAFE path — a plain `language-rust` class on
    /// the `<code>` element — still survives after the widened allowlist so
    /// the syntect token spans have a hook. This is the regression pin for
    /// gaps2 #36 sub-part (a).
    #[test]
    fn hostile_fence_info_string_is_escaped_and_language_class_survives() {
        // Hostile: info token that looks like a script injection.
        let hostile = render_markdown("```<script>alert(1)</script>\ncode\n```\n");
        assert!(
            !hostile.contains("<script>"),
            "live <script> from fence info must be stripped: {hostile}"
        );
        // The escaped form will appear inside a class value; ammonia lets
        // class through but the content is HTML-escaped so it is inert.
        assert!(
            hostile.contains("<pre><code"),
            "code block structure must survive: {hostile}"
        );

        // Hostile: info token with a class-injection attempt.
        let class_inject = render_markdown("```evil\" onmouseover=\"alert(1)\ncode\n```\n");
        assert!(
            !class_inject.contains("onmouseover"),
            "event handler injected via fence info must not survive: {class_inject}"
        );

        // Safe: the normal case — language-rust class must survive so
        // syntect hl- spans (server-side) and the md-enhance label both work.
        let safe = render_markdown("```rust\nfn ok() {}\n```\n");
        assert!(
            safe.contains("language-rust"),
            "language-rust class must survive sanitization (gaps2 #36a): {safe}"
        );
        assert!(
            safe.contains("class=\"hl-"),
            "syntect hl- token spans must survive sanitization: {safe}"
        );
    }

    #[test]
    fn highlight_styles_global_emits_a_style_block() {
        let mut env = Environment::new();
        register_highlight_styles_function(&mut env);
        env.add_template("t", "{{ highlight_styles() }}")
            .expect("add template");
        let out = env
            .get_template("t")
            .expect("get template")
            .render(())
            .expect("render");
        assert!(out.starts_with("<style>"), "wraps in a style block: {out}");
        assert!(out.contains(".hl-"), "carries the token CSS: {out}");
        assert!(
            out.trim_end().ends_with("</style>"),
            "closes the style block: {out}"
        );
    }
}
