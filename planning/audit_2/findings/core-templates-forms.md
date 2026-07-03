# Audit: core templates + forms

Scope: `crates/umbral-core/src/templates.rs`, `crates/umbral-core/src/templates/` (incl. `defaults/default_404.html`, `defaults/default_500.html`), `crates/umbral-core/src/forms.rs`. Focus: template autoescaping / XSS, server-side template injection, form validation completeness, mass-assignment, file-upload handling, error-message leakage, CSRF field integration.

## A. Executive summary

Autoescaping is correctly wired as the default: `build_env` installs an autoescape callback that turns on HTML escaping for every `.html`/`.htm` template and leaves `.txt` verbatim (templates.rs:921-927), and the custom formatter (templates.rs:986-991) delegates to minijinja's `escape_formatter` for all non-null values, so it never opens an escape hole. The `img`, `markdown`, and `sanitize` filters all sanitize/escape before wrapping in `from_safe_string`, and `render()` does a registry-key lookup rather than compiling arbitrary source, so user-controlled template *names* cannot inject template code (no SSTI on the `render` path).

The most urgent problem is that autoescape is only an *HTML-context* guarantee, and the framework's own shipped 404 page violates it: the request path (fully attacker-controlled) is interpolated into an inline `onclick="...writeText('{{ path }}')..."` JavaScript-string context in `default_404.html:146`. HTML escaping does not neutralize a JS-string breakout, so any umbral app running the default error pages (the default) has a reflected XSS on every 404. The same pattern exists in `default_500.html:140` but is dev-mode-only because `request_path` is blanked in production (errors.rs:294). Second, `render_str` — a no-autoescape helper that compiles arbitrary template source — is re-exported through the public facade (`umbral::templates::render_str`), giving a foot-gun for both XSS and SSTI if a consumer feeds it user input. Third, several smaller hygiene issues: form field `name`/label are emitted unescaped (safe only while names stay static), the form re-render error path leaks the template name + minijinja error to the client, and disabling the form-body cap maps to unbounded buffering.

Could not assess: actual multipart file-upload size/type/content validation (lives in umbral-admin, out of scope — forms.rs explicitly does "no file uploads"), CSRF *enforcement* middleware (umbral-security, out of scope), and whether the live axum/hyper router accepts the sub-delim characters the 404 exploit relies on before reaching the fallback.

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | XSS (JS-context) | crates/umbral-core/src/templates/defaults/default_404.html:146 (data via errors.rs:188-192,235) | Attacker-controlled request path `{{ path }}` interpolated into an inline `onclick` JS string. HTML autoescape does not stop JS-string breakout. Block is not dev-gated; default 404 page is on by default. | Reflected XSS on every umbral app's 404 → arbitrary JS in victim origin, session/cookie theft, account takeover. At 10M-user scale this is a broad phishing-to-XSS surface. | Remove the `{{ path }}` from the JS handler; drive the copy button from the already-safe `data-`/text node instead of inlining into JS. See C. | ✅ done |
| 2 | LOW | XSS (JS-context) | crates/umbral-core/src/templates/defaults/default_500.html:140 | Same JS-string injection with `{{ request_path }}`, but `request_path` is `""` in production (errors.rs:290-295) and only populated in dev (errors.rs:282-288). | Reflected XSS only when a dev/staging server with `environment=Dev` is reachable and returns a 500 on a crafted path. | Same fix as #1; also fix regardless since dev servers are frequently exposed. | ✅ done |
| 3 | MEDIUM | SSTI / XSS | crates/umbral-core/src/templates.rs:1079-1084; re-exported at crates/umbral/src/lib.rs:463 | `render_str` builds a fresh `Environment::new()` and registers source under name `__inline` (no `.html` suffix) → `AutoEscape::None`, and it compiles the passed `src` as a template. It is `#[doc(hidden)]` "test/bench only" but is `pub` and re-exported through the public facade. | A consumer who reaches for `umbral::templates::render_str(user_input, ..)` gets both unescaped output (XSS) and template-source compilation (SSTI: `{{ ... }}`, `{% ... %}` from user data execute). | Do not re-export from the facade (drop `pub use ... render_str`), or gate behind `#[cfg(test)]` / a `test-util` feature. If it must stay public, apply the same autoescape callback as `build_env`. | deferred: fix lives in the `umbral` facade crate (`crates/umbral/src/lib.rs`), out of this area's scope |
| 4 | LOW | XSS (attribute) | crates/umbral-core/src/forms.rs:687,697,703,723,727,768; 102-103 | Field `name` (and the trait-default `render_html` label text/`for=`) are written into HTML without `html_escape`. Only `value`, option `val`/`label` are escaped. | Safe while field names are static struct identifiers (the derive-macro norm). An app that constructs `Field::text(user_supplied_name)` gets attribute-injection/XSS. | Run `name` through `html_escape` in every `format!` that emits it. | ✅ done |
| 5 | LOW | Info leak | crates/umbral-core/src/forms.rs:1022-1027 | On a form-template render failure, `FormErrors::render_with` returns `500` with body `format!("form re-render failed for `{template}`: {e}")` — template name + raw minijinja error (can carry file path/line) sent to the client, not dev-gated. | Minor internal-detail disclosure on a misconfiguration path. | Return a generic 500 body to the client; `tracing::error!` the detail server-side (mirror errors.rs render_500). | ✅ done |
| 6 | LOW | DoS | crates/umbral-core/src/forms.rs:1172-1178 | `max_form_body_bytes = Some(0)` (documented "disable") maps to `usize::MAX`, so `to_bytes` will buffer an unbounded urlencoded body into memory. | An operator who sets the cap to 0 (or the config doc's "large forms" advice) exposes a memory-exhaustion DoS. | Treat `0` as "reject body"/keep a hard ceiling, or document that `0` is dev-only and never for internet-facing deploys. | deferred: `0`=disable is a documented, intentional config contract + operator opt-in (not attacker-controlled); changing the semantics breaks the documented behaviour and needs a settings-docs redesign |

## C. Detailed findings (CRITICAL / HIGH)

### #1 — Reflected XSS in the default 404 page (JS-string context)

`render_not_found` renders `default_404.html` (the default when `default_pages_enabled()`), passing the raw request path unconditionally:

```rust
// errors.rs:235 (fallback handler)
let path = req.uri().path().to_owned();
render_not_found(template.as_deref(), &path)
// errors.rs:188-192
let ctx = context! {
    path => path,           // populated for EVERY request, not just dev
    dev_mode => dev_mode,
    routes_by_plugin => routes_ctx,
};
```

The template inlines `path` into an event-handler attribute:

```html
<!-- default_404.html:130,146 — NOT inside any {% if dev_mode %} -->
{% if path %}
  ...
  <button
    onclick="navigator.clipboard?.writeText('{{ path }}'); this.textContent='Copied'; ...">
    Copy
  </button>
{% endif %}
```

`{{ path }}` renders with `AutoEscape::Html`, which escapes `< > & " '` — the correct rule for HTML *text* and quoted-attribute contexts (the sibling `title="{{ path }}"` and `>{{ path }}</code>` at lines 141-142 are safe). It is the *wrong* rule for a JavaScript string literal. The browser HTML-decodes the attribute value (`&#x27;` → `'`) before handing it to the JS parser, so an escaped quote still closes the JS string.

**Attack.** `req.uri().path()` is not percent-decoded, and hyper accepts the RFC 3986 sub-delims `'`, `(`, `)`, `;` unencoded in a path — none of which the HTML escaper touches usefully here. Attacker sends a victim a link:

```
https://app.example.com/x');alert(document.domain);('
```

The path lands in the handler as `/x');alert(document.domain);('`. After HTML escaping the attribute reads `writeText('/x&#x27;);alert(document.domain);(&#x27;')`. When the browser parses the `onclick`, it decodes the entities and the JS engine sees:

```js
navigator.clipboard?.writeText('/x');alert(document.domain);('');  ...
```

`alert(document.domain)` executes in the app's origin — replaceable with cookie/token exfiltration. This fires on the default 404 of a default-configured production app; no auth needed, victim only clicks a link.

**Fix.** Never interpolate a variable into an inline JS string. Drive the copy button from the safe DOM text that's already on the page:

```html
<code id="req-path" class="mono ..." title="{{ path }}">{{ path }}</code>
<button
  type="button"
  onclick="navigator.clipboard?.writeText(document.getElementById('req-path').textContent); this.textContent='Copied'; setTimeout(()=>this.textContent='Copy',1200);">
  Copy
</button>
```

`textContent` reads the already-HTML-escaped, inert text node — no value crosses into JS source. Apply the identical change to `default_500.html:140` (#2).

## D. Blind spots

- **Multipart file uploads.** forms.rs handles only urlencoded bodies and treats a file field's value as an opaque storage key with a `Required` check (forms.rs:514-532). Actual upload size/content-type/magic-byte validation lives in umbral-admin's multipart handler — out of scope; not assessed here.
- **CSRF enforcement.** forms.rs only surfaces `csrf_input`/`csrf_token` for templates (via the ambient merge in templates.rs). Whether the CSRF *middleware* validates tokens, its cookie flags, and rotation are in umbral-security — out of scope. Note: the form-primitive `FormValidate::render_html` emits `<div class="field">` inputs with **no** `<form>` wrapper and **no** CSRF field, so relying on it alone yields an unprotected form; the CSRF field must come from the surrounding template's `{{ csrf_input }}`.
- **Router path acceptance.** The #1 exploit assumes axum/hyper delivers `'`, `(`, `)`, `;` in `uri().path()` without rejecting or normalizing (standard hyper behavior). I read the raw `req.uri().path()` call (errors.rs:235) but did not exercise the live router.
- **minijinja escape set.** I did not pin the exact character set minijinja escapes; the #1 XSS holds whether or not single-quote is escaped (escaped → decoded back by the browser; unescaped → breaks the JS string directly).
- **Mass assignment.** At the form layer there is no mass-assignment risk: `FormValidate::validate` reads only declared fields, so extra POST keys are ignored (the form struct is the whitelist). The ORM `create` path that consumes validated structs is out of scope.

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Fix #1 and #2: rewrite the copy-button `onclick` in `default_404.html` and `default_500.html` to read `textContent` instead of interpolating the path into JS. (HIGH)
2. Fix #3: remove `pub use umbral_core::templates::render_str;` from the facade, or feature-gate it. (MEDIUM)
3. Fix #5: replace the client-visible template-name+error 500 body in `FormErrors::render_with` with a generic message + server-side log. (LOW)

**Short term (< 2 weeks)**
4. Fix #4: HTML-escape field `name`/label in `forms.rs` render paths so user-derived field names can't inject attributes.
5. Fix #6: give `max_form_body_bytes = 0` a hard ceiling or restrict "disable" to dev.
6. Add a regression test that renders `default_404.html` with a path of `x');alert(1)//` and asserts no un-decoded quote reaches the JS context (assert the JS handler no longer contains `{{ path }}`-derived data).

**Structural (needs design work)**
7. Consider a JS/URL-context-aware escaping helper (or lint) so framework templates can't reintroduce JS-string interpolation. Autoescape is HTML-context-only by design; the framework needs a documented, enforced pattern for the JS/`href="javascript:"`/`style` contexts.

## Docs updated

- `documentation/docs/v0.0.1/templates/rendering-html.mdx` — the "Autoescape is on by default" section claimed the `<script> → &lt;script&gt;` XSS guarantee without qualifying that it is an **HTML-context** guarantee only. Added a `Callout` stating autoescape does not cover JavaScript-string, `href="javascript:"`, or CSS contexts, and that variables must not be interpolated into inline `onclick`/`<script>` (grounded in the real behavior that produced finding #1). No import added (Specra globals).
