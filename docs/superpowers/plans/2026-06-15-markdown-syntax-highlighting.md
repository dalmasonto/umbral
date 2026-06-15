# Markdown Syntax Highlighting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make fenced code blocks in the `| markdown` filter render with server-side, class-based syntect highlighting that is XSS-safe, so every markdown surface (blog, notes, usage docs) shows colored code. Also polish the already-server-rendered tables and images (website CSS/JS only): a rounded, horizontally-scrollable table frame and rounded, lightbox-able images.

**Architecture:** Rewrite `render_markdown`'s pulldown-cmark event stream so each fenced code block becomes a single pre-highlighted `Event::Html` produced by syntect's `ClassedHTMLGenerator` (inert `hl-` token spans, never inline styles). The final ammonia pass is widened by exactly one inert attribute — `class` on `pre`/`code`/`span` — so the spans and the `language-*` label survive while `style`/`on*`/`javascript:` stay stripped. Token colors come from a `base16-ocean.dark` stylesheet syntect generates, inlined into `base.html` via a `highlight_styles()` template global.

**Tech Stack:** Rust, `pulldown-cmark` 0.12 (already a dep), `ammonia` 4 (already a dep), `syntect` 5 (new, pure-Rust fancy-regex backend), minijinja.

**Spec:** `docs/superpowers/specs/2026-06-15-markdown-syntax-highlighting-design.md`

---

## File Structure

- **`crates/umbra-core/Cargo.toml`** — add the `syntect` dependency.
- **`crates/umbra-core/src/templates.rs`** — all framework logic: the lazy `SyntaxSet`, `highlight_code_block`/`wrap_code_block` helpers, the rewritten `render_markdown`, `highlight_css()`, and the `highlight_styles()` global registration. (One file: highlighting is one cohesive concern living next to the filter it extends.)
- **`crates/umbra/src/lib.rs`** — re-export `highlight_css` from the `umbra::templates` facade module.
- **`umbra_website/templates/base.html`** — emit `{{ highlight_styles() }}` once in `<head>`.
- **`umbra_website/static/js/md-enhance.js`** — add `enhanceTables(root)` (wrap `<table>` in a `.md-table` frame), called in the existing `[data-md]` root loop.
- **`umbra_website/static/css/md-enhance.css`** — add the `.md-table` styles and a `border-radius` on `.md-img`.
- **`documentation/docs/v0.0.1/web/markdown-syntax-highlighting.mdx`** — the user-facing doc page.

All commands in this plan run from the framework workspace root `crates/` unless a path says otherwise. The website is a separate cargo project and is verified last.

---

### Task 1: Add syntect, lazy syntax/theme assets, and `highlight_css()`

**Files:**
- Modify: `crates/umbra-core/Cargo.toml`
- Modify: `crates/umbra-core/src/templates.rs` (add imports + helpers near `render_markdown`, ~line 423; add test in `mod tests`, ~line 952)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/umbra-core/src/templates.rs`:

```rust
#[test]
fn highlight_css_contains_hl_rules() {
    let css = highlight_css();
    assert!(!css.is_empty(), "generated theme CSS should not be empty");
    assert!(css.contains(".hl-"), "theme CSS must target hl- classes: {css}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbra-core highlight_css_contains_hl_rules`
Expected: FAIL — compile error, `cannot find function highlight_css in this scope`.

- [ ] **Step 3: Add the syntect dependency**

In `crates/umbra-core/Cargo.toml`, under `[dependencies]`, next to the existing `pulldown-cmark` / `ammonia` lines:

```toml
# syntect: server-side syntax highlighting for fenced code blocks in the
# `markdown` filter. default-features off + `default-fancy` selects the
# pure-Rust fancy-regex backend (no C/onig dep) with the bundled default
# syntaxes + themes. See docs/superpowers/specs/2026-06-15-markdown-syntax-highlighting-design.md
syntect = { version = "5", default-features = false, features = ["default-fancy"] }
```

- [ ] **Step 4: Add imports + lazy assets + `highlight_css()`**

At the top of `crates/umbra-core/src/templates.rs`, add to the existing `use` block (or a new one):

```rust
use std::sync::OnceLock;

use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
```

Then, immediately above `pub fn render_markdown` (~line 423), add:

```rust
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p umbra-core highlight_css_contains_hl_rules`
Expected: PASS. (First run recompiles with syntect — may take a minute.)

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbra/crates
cargo fmt
git add umbra-core/Cargo.toml umbra-core/Cargo.lock umbra-core/src/templates.rs
git commit -m "feat(templates): generate base16-ocean.dark highlight CSS via syntect"
```

(If `Cargo.lock` lives at `crates/Cargo.lock`, add that path instead — `git add crates/Cargo.lock`.)

---

### Task 2: Highlight fenced code blocks in `render_markdown`

**Files:**
- Modify: `crates/umbra-core/src/templates.rs` (add `highlight_code_block` + `wrap_code_block` helpers; rewrite `render_markdown` body, ~line 423–437; add tests in `mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `crates/umbra-core/src/templates.rs`:

```rust
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
    // The widened allowlist adds exactly `class` (inert) and nothing else:
    // a raw styled span keeps its class but loses the style attribute.
    let html = render_markdown("<span class=\"x\" style=\"color:red\">hi</span>");
    assert!(html.contains("class=\"x\""), "class survives: {html}");
    assert!(!html.contains("style="), "style stripped: {html}");
}

#[test]
fn unknown_and_plain_fences_do_not_panic() {
    let unknown = render_markdown("```notalanguage\nx := 1\n```\n");
    let plain = render_markdown("```\nplain text\n```\n");
    assert!(unknown.contains("<pre><code"), "unknown lang block: {unknown}");
    assert!(plain.contains("<pre><code"), "plain block: {plain}");
    assert!(
        unknown.contains("language-notalanguage"),
        "unknown lang still labelled: {unknown}"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p umbra-core fenced_rust_block_gets_syntect_token_spans`
Expected: FAIL — the current `render_markdown` emits a plain class-stripped `<pre><code>`, so `language-rust` / `hl-` are absent.

- [ ] **Step 3: Add the highlighting helpers**

Below `highlight_css` (still above `render_markdown`) in `crates/umbra-core/src/templates.rs`:

```rust
/// Render one fenced code block to safe HTML. `lang` is the fence info
/// token (`Some("rust")`) or `None` for an unlabelled / indented block.
/// With a known language the body is syntect-highlighted into `hl-` token
/// spans; otherwise — or on any highlighter error — it falls back to a
/// plain escaped block that still carries `class="language-…"` so the
/// `md-enhance.js` label keeps working. Never panics, never drops the
/// user's code.
fn highlight_code_block(lang: Option<&str>, src: &str) -> String {
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
```

- [ ] **Step 4: Rewrite `render_markdown`**

Replace the entire body of `pub fn render_markdown` (currently `crates/umbra-core/src/templates.rs:423-437`) with:

```rust
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
```

- [ ] **Step 5: Run all the markdown tests to verify they pass**

Run: `cargo test -p umbra-core markdown` then `cargo test -p umbra-core fenced_ -- --nocapture` and the rest:

```bash
cargo test -p umbra-core fenced_rust_block_gets_syntect_token_spans
cargo test -p umbra-core script_in_code_fence_is_escaped_not_executed
cargo test -p umbra-core prose_script_is_still_stripped
cargo test -p umbra-core markdown_allows_class_but_not_style
cargo test -p umbra-core unknown_and_plain_fences_do_not_panic
```

Expected: all PASS.

- [ ] **Step 6: Verify the rest of umbra-core didn't regress**

Run: `cargo test -p umbra-core`
Expected: PASS (no other markdown/template test broke).

- [ ] **Step 7: Commit**

```bash
cd /home/dalmas/E/projects/umbra/crates
cargo fmt
cargo clippy -p umbra-core --all-targets
git add umbra-core/src/templates.rs
git commit -m "feat(templates): highlight fenced code blocks server-side, safely

Rewrite render_markdown to replace each code block with syntect-rendered
hl- token spans, and widen the ammonia allowlist by the single inert
attribute class (on pre/code/span). style/on*/javascript: stay stripped.
Implements the slice deferred at templates.rs:249."
```

---

### Task 3: Expose `highlight_styles()` template global + facade re-export

**Files:**
- Modify: `crates/umbra-core/src/templates.rs` (add `register_highlight_styles_function`; call it in `build_env` ~line 568; add test in `mod tests`)
- Modify: `crates/umbra/src/lib.rs:438` (facade re-export)

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `crates/umbra-core/src/templates.rs`:

```rust
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
    assert!(out.trim_end().ends_with("</style>"), "closes the style block: {out}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbra-core highlight_styles_global_emits_a_style_block`
Expected: FAIL — `cannot find function register_highlight_styles_function`.

- [ ] **Step 3: Add the registration function**

In `crates/umbra-core/src/templates.rs`, next to `register_markdown_filter` (~line 331), add:

```rust
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
```

- [ ] **Step 4: Wire it into `build_env`**

In `crates/umbra-core/src/templates.rs`, in `build_env` (alongside the other `register_*` calls, ~line 568, right after `register_img_filter(&mut env);`), add:

```rust
    // `{{ highlight_styles() }}` — the syntect token stylesheet for
    // server-highlighted code, emitted once into <head> by a base template.
    register_highlight_styles_function(&mut env);
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p umbra-core highlight_styles_global_emits_a_style_block`
Expected: PASS.

- [ ] **Step 6: Re-export `highlight_css` from the facade**

In `crates/umbra/src/lib.rs`, edit the `pub use umbra_core::templates::{ … }` list (line 438) to add `highlight_css`:

```rust
    pub use umbra_core::templates::{
        CURRENT_CSRF, CURRENT_USER, TemplateError, TemplateRegistrar, current_csrf, highlight_css,
        merge_ambient_context, merge_ambient_value, render, resolve_static_url, with_current_csrf,
        with_current_user,
    };
```

- [ ] **Step 7: Verify the whole workspace builds + tests**

Run from `crates/`:
```bash
cargo build
cargo test -p umbra-core
```
Expected: PASS (facade compiles with the new re-export; all umbra-core tests green).

- [ ] **Step 8: Commit**

```bash
cd /home/dalmas/E/projects/umbra/crates
cargo fmt
git add umbra-core/src/templates.rs umbra/src/lib.rs
git commit -m "feat(templates): add highlight_styles() global + facade highlight_css"
```

---

### Task 4: Inline the stylesheet in the website's base template

**Files:**
- Modify: `umbra_website/templates/base.html` (add `{{ highlight_styles() }}` in `<head>`, after the `md-enhance.css` link at line 18)

- [ ] **Step 1: Add the global to `<head>`**

In `umbra_website/templates/base.html`, immediately after line 18 (`<link rel="stylesheet" href="/static/css/md-enhance.css">`), add:

```html
  {# Syntect token colors for server-highlighted fenced code. Emitted once
     here so every `[data-md]` surface (blog, plugin notes, usage docs)
     gets colored code; the dark .md-code frame from md-enhance.js provides
     the background. #}
  {{ highlight_styles() }}
```

- [ ] **Step 2: Let the dev server rebuild, then verify the stylesheet is present**

The running `cargo-watch` rebuilds the website because umbra-core (a path dep) changed. Wait for the rebuild to finish (watch the dev terminal), then:

Run:
```bash
curl -s http://localhost:8100/plugins/umbra-admin | grep -c '<style>'
curl -s http://localhost:8100/plugins/umbra-admin | grep -oE '\.hl-[a-z]+' | head -3
```
Expected: at least one `<style>` block, and `.hl-…` rules present (the inlined theme CSS).

- [ ] **Step 3: Verify a real fenced block renders token spans**

Pick a page whose markdown contains a fenced code block. The blog is the surest:
```bash
# list blog posts, then fetch one and look for hl- spans
curl -s http://localhost:8100/blog 2>/dev/null | grep -oE '/blog/[a-z0-9-]+' | head -1
# then, substituting the slug found above:
curl -s http://localhost:8100/blog/<slug> | grep -oE 'class="hl-[a-z ]+"' | head -5
```
Expected: one or more `class="hl-…"` token spans inside the rendered post. If no blog post has a code fence, instead post a note containing a ```rust fenced block on a plugin page and confirm the rendered note row contains `hl-` spans.

- [ ] **Step 4: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add umbra_website/templates/base.html
git commit -m "feat(website): inline syntect highlight stylesheet in base.html"
```

---

### Task 5: User-facing documentation page

**Files:**
- Create: `documentation/docs/v0.0.1/web/markdown-syntax-highlighting.mdx`

- [ ] **Step 1: Write the doc page**

Create `documentation/docs/v0.0.1/web/markdown-syntax-highlighting.mdx` (check the `web/` folder exists and has a `_category_.json`; if the folder is new, create the category file too — see a sibling area like `orm/_category_.json` for the shape):

```mdx
---
title: Syntax highlighting
description: Fenced code blocks in the markdown filter are highlighted server-side, safely.
sidebar_position: 5
tags: [markdown, templates]
---

# Syntax highlighting

The `| markdown` filter highlights fenced code blocks on the server. Add a language to the fence and the rendered HTML carries syntect token classes (`hl-…`), colored by a built-in `base16-ocean.dark` theme. Highlighting is class-based — never inline styles — so the sanitizer keeps its guard up: only the inert `class` attribute is allowed through, while `<script>`, event handlers, and `javascript:` URLs are still stripped. An unknown or missing language falls back to a plain (but still labelled) block.

## Example

In a template:

```html
<div class="prose" data-md>{{ post.body | markdown }}</div>
```

Where `post.body` contains:

````markdown
```rust
fn main() {
    println!("hello, umbra");
}
```
````

To get colors, include the generated stylesheet once in your base template's `<head>`:

```html
{{ highlight_styles() }}
```

<Callout type="info">
  The colors are generated from a fixed theme. To change palettes you regenerate the stylesheet from a different syntect theme — the token classes stay the same, so no template changes are needed.
</Callout>

See the design rationale in `docs/superpowers/specs/2026-06-15-markdown-syntax-highlighting-design.md`.
```

- [ ] **Step 2: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add documentation/docs/v0.0.1/web/markdown-syntax-highlighting.mdx
# include documentation/docs/v0.0.1/web/_category_.json if you created it
git commit -m "docs(web): syntax highlighting page for the markdown filter"
```

---

### Task 6: Table frame + image radius (website CSS/JS)

These are static assets — no Rust rebuild; the dev server serves the updated files directly. No JS unit-test harness exists, so verification is "served correctly" + a visual check, mirroring Task 4.

**Files:**
- Modify: `umbra_website/static/js/md-enhance.js` (add `enhanceTables`, call it in the `[data-md]` loop)
- Modify: `umbra_website/static/css/md-enhance.css` (add `.md-table` block; add radius to `.md-img`)

- [ ] **Step 1: Add `enhanceTables` and call it**

In `umbra_website/static/js/md-enhance.js`, update the root loop inside `ready(...)` (currently calls `enhanceCodeBlocks` + `collectImages`) to also enhance tables:

```js
    roots.forEach(function (root) {
      enhanceCodeBlocks(root);
      enhanceTables(root);
      collectImages(root, gallery);
    });
```

Then add the function next to `enhanceCodeBlocks`:

```js
  /* ---- Tables: rounded, horizontally-scrollable frame ---------------- */
  function enhanceTables(root) {
    Array.prototype.slice.call(root.querySelectorAll("table")).forEach(function (table) {
      // Already framed (e.g. re-run after a live DOM insert) — skip.
      if (table.parentNode && table.parentNode.classList.contains("md-table")) return;
      var wrap = document.createElement("div");
      // `not-prose` keeps Tailwind `prose` from re-styling the framed table.
      wrap.className = "md-table not-prose";
      table.parentNode.insertBefore(wrap, table);
      wrap.appendChild(table);
    });
  }
```

- [ ] **Step 2: Add the table styles + image radius**

Append to `umbra_website/static/css/md-enhance.css`:

```css
/* ===== Tables ===== */
.md-table {
  overflow-x: auto;            /* wide tables scroll instead of breaking layout */
  margin: 1em 0;
  border: 1px solid var(--hairline);
  border-radius: 12px;
}
.md-table table {
  width: 100%;
  border-collapse: collapse;
  font-size: 14px;
}
.md-table th,
.md-table td {
  padding: 9px 14px;
  text-align: left;
  border-bottom: 1px solid var(--hairline);
}
.md-table th {
  background: var(--surface-2);
  font-weight: 700;
  color: var(--ink);
}
.md-table tr:last-child td { border-bottom: 0; }   /* clean bottom corners */
.md-table tbody tr:nth-child(even) td {
  background: color-mix(in srgb, var(--surface-2) 45%, transparent);
}
```

And update the existing `.md-img` rule (currently `cursor: zoom-in;` + `transition`) to add the radius:

```css
.md-img {
  cursor: zoom-in;
  border-radius: 12px;
  transition: filter 0.15s ease;
}
```

- [ ] **Step 3: Verify the updated assets are served**

Run:
```bash
curl -s http://localhost:8100/static/js/md-enhance.js | grep -c enhanceTables
curl -s http://localhost:8100/static/css/md-enhance.css | grep -c 'md-table'
```
Expected: the JS count is `2` (definition + call), the CSS count is `>= 1`.

- [ ] **Step 4: Visual check in the browser**

Open a page whose markdown has a table and an image (a blog post is surest). Confirm: the table sits in a rounded, bordered frame with a shaded header and scrolls horizontally when narrow; images are rounded and open the lightbox on click. (The `.md-table` wrapper is added by JS, so it only appears in the live DOM, not in `curl` output.)

- [ ] **Step 5: Commit**

```bash
cd /home/dalmas/E/projects/umbra
git add umbra_website/static/js/md-enhance.js umbra_website/static/css/md-enhance.css
git commit -m "feat(website): rounded table frame + image radius for markdown"
```

---

## Final verification

- [ ] From `crates/`: `cargo fmt --check && cargo clippy -p umbra-core --all-targets && cargo build && cargo test -p umbra-core` — all green.
- [ ] The website rebuilt and `/plugins/umbra-admin` serves the inlined `<style>` with `.hl-` rules; a fenced code block renders `hl-` token spans.
- [ ] `md-enhance.js`/`.css` are served with the table frame + image radius; a blog table renders a rounded scrollable frame and images are rounded + lightbox-able.
- [ ] Notes still post via AJAX without a reload (the earlier fix is untouched).

## Self-review notes

- **Spec coverage:** server-side filter highlighting (Task 2), class-based not inline (Task 2 helper uses `ClassedHTMLGenerator`), ammonia `class`-only widening (Task 2 + the `markdown_allows_class_but_not_style` test), `base16-ocean.dark` (Task 1), inline `<style>` delivery (Tasks 3–4), pure-Rust fancy-regex backend (Task 1 Cargo feature), graceful unknown-lang fallback (Task 2 test), `OnceLock` caching with no render caching (Task 1), tests (Tasks 1–3), facade re-export (Task 3), doc page (Task 5), folded-in table frame + image radius (Task 6). Video is explicitly out of scope (own spec). All spec sections map to a task.
- **Type consistency:** `hl_class_style()`, `HL_PREFIX`, `syntax_set()`, `highlight_css()`, `highlight_code_block(Option<&str>, &str)`, `wrap_code_block(Option<&str>, &str)`, `register_highlight_styles_function(&mut Environment)` are defined once and referenced consistently.
- **Placeholder scan:** none — every code step shows complete code; the only runtime substitution is the blog `<slug>` in Task 4 Step 3, which is discovered by the preceding command.
