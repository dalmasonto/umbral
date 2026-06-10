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
//! boot so the collision is visible in the log. This matches Django's
//! `APP_DIRS` loader semantics. Silently-overridden templates are a
//! well-known footgun, so the warning is non-optional.
//!
//! Rendering goes through one ambient accessor, [`render`], which reads
//! the engine the App builder published into an `OnceLock` during build.
//!
//! ```ignore
//! let html = umbra::templates::render("articles_list.html", &context!(articles))?;
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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use minijinja::{AutoEscape, Environment};

tokio::task_local! {
    /// Per-request ambient user value, set by a session-aware layer
    /// (typically `umbra_sessions::UserContextLayer<U>`) and read by
    /// [`render`] to mirror Django's `request.user` ergonomic in
    /// templates. `None` means an anonymous request.
    ///
    /// Outside the layer's scope, `try_with` returns `Err(AccessError)`
    /// and `render` skips the merge — explicit ctx behaviour is
    /// preserved when no layer is installed.
    pub static CURRENT_USER: Option<minijinja::Value>;

    /// Per-request CSRF token, set by `umbra-security`'s middleware and
    /// read by [`render`] to inject `csrf_token` / `csrf_input` into
    /// every template — Django's `{% csrf_token %}` ergonomic. Outside
    /// the middleware's scope nothing is injected (a template that
    /// references `{{ csrf_token }}` then renders it empty under the
    /// engine's lenient-undefined behaviour).
    pub static CURRENT_CSRF: Option<String>;
}

/// Run `fut` with the ambient template user value scoped to `user`
/// for its duration. Intended for the session-aware layer in
/// `umbra-sessions`; downstream handler code reads the value
/// transparently through [`render`].
pub async fn with_current_user<F: std::future::Future>(
    user: Option<minijinja::Value>,
    fut: F,
) -> F::Output {
    CURRENT_USER.scope(user, fut).await
}

/// Run `fut` with the ambient CSRF token scoped for its duration.
/// Intended for the CSRF middleware in `umbra-security`; downstream
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

/// Register the built-in default 404/500 templates into an environment.
///
/// Called from `init` before any disk directories are scanned. The names
/// use the `__umbra__/` prefix so they can never collide with a user's
/// `templates/` directory (slashes aren't meaningful to the engine's name
/// lookup — `__umbra__/default_404.html` is just a unique string key).
///
/// Because the user's disk directories are added after this call and
/// first-match-wins is enforced by the `seen` set, a user who places a
/// file named `__umbra__/default_404.html` in their own templates dir will
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
fn register_markdown_filter(env: &mut Environment<'static>) {
    env.add_filter("markdown", |input: String| -> minijinja::Value {
        minijinja::Value::from_safe_string(render_markdown(&input))
    });
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

/// Render CommonMark + GFM `input` to sanitized HTML. Pulled out of the
/// filter closure so it's unit-testable and reusable by any future
/// Rust-side caller (e.g. a REST endpoint that returns pre-rendered
/// HTML).
pub fn render_markdown(input: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(input, options);
    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);

    ammonia::clean(&rendered)
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
            "umbra templates: template `{name}` is provided by multiple directories; \
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

    // features.md #4 — `{{ body | markdown }}` renders user-supplied
    // CommonMark/GFM to sanitized HTML. The reusable "safely show a
    // body/usage field" surface shared by the admin and end-user
    // templates; pairs with `#[umbra(widget = "markdown")]` on the
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

    let mut seen: HashSet<String> = HashSet::new();
    let mut collisions: Vec<String> = Vec::new();

    // Register the built-in default error templates before scanning disk
    // directories. Because disk directories are first-match-wins and are
    // scanned after this call, a user template with the same name (unlikely,
    // since the `__umbra__/` prefix is reserved) would silently replace the
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
    let merged = merge_ambient(ctx);
    tmpl.render(&merged).map_err(TemplateError::Render)
}

/// Merge the ambient task-locals into the caller's ctx: `user` (from
/// `CURRENT_USER`) and the CSRF pair `csrf_token` / `csrf_input` (from
/// `CURRENT_CSRF`). The handler's own keys always win — the ambient
/// injection is the default, not an override.
///
/// `user` is injected unconditionally (anonymous fallback below);
/// the CSRF pair only when a middleware actually scoped a token —
/// there is no meaningful fallback token, and rendering an empty
/// hidden input would make a form post a guaranteed-403 silently.
fn merge_ambient<C: Serialize>(ctx: &C) -> minijinja::Value {
    let ctx_value = minijinja::Value::from_serialize(ctx);
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
        // produce, kept in core so umbra-auth isn't a dependency of
        // the templates module.
        let layer_user = CURRENT_USER.try_with(|u| u.clone()).ok().flatten();
        pairs.push((
            "user".to_string(),
            layer_user.unwrap_or_else(anonymous_user_value),
        ));
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
/// to evaluate to false without `umbra templates: undefined
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
                "umbra templates: engine not initialised — call App::build() first"
            ),
            TemplateError::AlreadyInitialised => {
                write!(f, "umbra templates: init called more than once")
            }
            TemplateError::Io(e) => write!(f, "umbra templates: io: {e}"),
            TemplateError::Missing(name) => write!(
                f,
                "umbra templates: no template named `{name}`; check the templates directory"
            ),
            TemplateError::Render(e) => write!(f, "umbra templates: {e}"),
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
}
