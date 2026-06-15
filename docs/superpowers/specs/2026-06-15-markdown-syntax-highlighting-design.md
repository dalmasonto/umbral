# Markdown syntax highlighting: server-side, class-based, safe

Date: 2026-06-15
Area: `umbra-core` (the `| markdown` filter) + `umbra_website` (stylesheet wiring)
Status: approved (design)

## Problem

The framework's `| markdown` filter (`crates/umbra-core/src/templates.rs`, `render_markdown`) parses CommonMark + GFM with pulldown-cmark, renders to HTML, then sanitizes with `ammonia::clean`. Fenced code blocks come out as plain `<pre><code>…</code></pre>` — and ammonia's default allowlist strips the `language-*` class, so even the language hint is gone. There is no syntax highlighting. The code comment at `templates.rs:249` records this as deliberately deferred: "syntax highlighting on fenced code blocks (ammonia strips the `language-*` class today)".

Every markdown surface wants colored code: the blog (`site_content/blog_post.html`), plugin bodies and usage docs, and — imminently — the community-notes thread, which becomes a chat surface where people paste Rust/TOML/SQL. Because notes publish the instant they post, the highlighting has to be **safe**: it must not become an XSS vector.

Today's only enhancement is client-side: `static/js/md-enhance.js` wraps each `<pre>` in a dark `.md-code` frame with a language label and Copy button, and turns images into a lightbox. It does **not** color tokens. This spec adds real, server-rendered token coloring underneath that frame.

## Decisions

- **Server-side, in the framework filter.** Highlighting lands in `render_markdown` so every `| markdown` consumer (blog, notes, usage docs) gets it for free, with no JS and no flash of unhighlighted code. This implements the slice `templates.rs:249` deferred.
- **Class-based output, never inline styles.** syntect's `ClassedHTMLGenerator` (with `ClassStyle::SpacedPrefixed { prefix: "hl-" }`) emits `<span class="hl-…">` token spans. Colors live in a separate stylesheet. The rejected alternative — `highlighted_html_for_string`, which emits `<span style="color:#…">` — would force ammonia to permit the `style` attribute, its single most dangerous one. Classes are inert (a class name can't execute), so the security boundary stays intact.
- **Theme: `base16-ocean.dark`.** A syntect built-in (no extra theme file). Its muted blue-grey palette reads well on the existing dark `.md-code` frame and doesn't fight the site's warm paper/accent UI. The theme is just the generated CSS — swapping it later is a one-line change plus a regenerate.
- **CSS delivered inline in `base.html`.** A new template global `highlight_styles()` emits the generated stylesheet inside a `<style>` block in `<head>`, once per page. No extra HTTP request, no static-file build step, always in sync with the framework version.
- **Ammonia widened minimally and only for inert attributes.** The markdown path uses a shared `ammonia::Builder` (lazy static) that additionally allows the `class` attribute on `pre`, `code`, and `span` — nothing else. `style`, `on*` handlers, and `javascript:` URLs stay stripped. This is the whole of the "safely" surface.
- **Unknown / absent language is a graceful fallback, never an error.** A fenced block with no language, or a language syntect doesn't know, renders as a plain escaped `<pre><code>` — still carrying `class="language-x"` when a language token was given (so the `md-enhance.js` label keeps working), just without the `hl-` token spans. Highlighting failures never panic and never drop the user's code.
- **Pure-Rust regex.** Pull syntect with `default-features = false` + the fancy-regex backend so it brings no C/onig dependency, matching the framework's pure-Rust posture.
- **No render caching (deferred).** `SyntaxSet`/`ThemeSet` load once into a `OnceLock` (the loads are the expensive part). Per-render highlighting is acceptable for blog/notes volumes. Memoizing rendered markdown is a separate, later optimization.

## Framework changes (`crates/umbra-core`)

1. **Cargo.toml**: add `syntect` (default-features off, fancy-regex backend).
2. **`render_markdown`**: keep the pulldown options, but iterate the event stream instead of piping straight to `push_html`. On `Start(CodeBlock(Fenced(lang)))…Text…End(CodeBlock)`, buffer the code text, call the new helper, and splice the result in as a single `Event::Html`. All other events pass through unchanged.
3. **`highlight_code_block(lang: &str, src: &str) -> String`** (new, private, unit-testable): resolve the syntax by token (fallback to plain text when unknown), run `ClassedHTMLGenerator` over `LinesWithEndings`, and wrap the spans in `<pre><code class="language-{lang}">…</code></pre>`. Returns escaped-plain HTML when there's no language or no syntax match.
4. **Shared markdown `ammonia::Builder`** (lazy static): the default allowlist plus `class` allowed on `pre`/`code`/`span`. `render_markdown` cleans through it. `sanitize_html` (the RTE path) is unchanged — it doesn't need highlight classes.
5. **`pub fn highlight_css() -> &'static str`**: the `base16-ocean.dark` stylesheet, generated once via `css_for_theme_with_class_style` with the `hl-` prefix and cached in a `OnceLock`. Re-exported from the `umbra` facade (`umbra::templates::highlight_css`).
6. **Template global `highlight_styles()`**: returns `Value::from_safe_string("<style>…</style>")` wrapping `highlight_css()`, registered on the core environment so any template can emit it.

## Website changes (`umbra_website`)

- **`templates/base.html`**: add `{{ highlight_styles() }}` once in `<head>`. The token colors render on top of the existing dark `.md-code` frame; `md-enhance.js` is untouched (it still reads the preserved `language-*` class for the label and wraps the `<pre>`).

## Interaction with `md-enhance.js`

Unchanged. The enhancer keys off the `<code class="language-…">` class (now preserved through ammonia) and wraps the `<pre>`; the inner `hl-*` token spans are inert content it never inspects. Server highlight + client frame/copy/lightbox compose cleanly.

## Tests (`crates/umbra-core`)

- A fenced `rust` block renders `hl-` token spans and keeps `class="language-rust"`.
- XSS guard: `<script>alert(1)</script>` **inside** a fenced block is escaped as text (visible, inert), and a literal `<script>` in prose is still stripped — the security boundary holds with the widened allowlist.
- `style="…"` on a span is still removed (prove only `class` was allowed).
- An unknown language and a no-language fence both render a plain block without panicking.
- `highlight_css()` is non-empty and contains `.hl-` rules.

## Documentation

- `documentation/docs/v0.0.1/web/markdown-syntax-highlighting.mdx`: purpose (one paragraph), one fenced-code example showing the rendered result, and a link to this spec. Per "ship a feature, ship its doc page."

## Out of scope

- The notes → chat surface (one-level replies, inline composer, compact layout). That is the next spec; this one only makes code render colored wherever `| markdown` already runs.
- Render-result caching / memoization.
- A light-theme variant or per-consumer configurable theme. One baked theme now; configurability is a later slice if asked for.
