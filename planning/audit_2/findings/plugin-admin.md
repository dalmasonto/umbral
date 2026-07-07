# Audit — `plugins/umbral-admin/` (auto-CRUD admin UI)

> **Verification stamp — code re-triaged 2026-07-06.** Checked against current code. **Fixed:** #1 (`escapejs` at every JS sink + test), #2 (palette perm-filter), #3 (filter-dialog perm), #4 (history perm), #7 (developer `action.confirm`/`facet.field` escaped). **Still open →** #6 (upload magic-byte sniff) tracked in `planning/gaps3.md #27`; #5 (in-handler CSRF verify) in `#28` (needs a boot-breaking dep or a multi-handler sweep; severity hinges on session-cookie `SameSite`). Treat the per-finding text below as historical.

Scope: authorization on every admin route, XSS in rendered output, IDOR, file/image upload, inline formsets/actions, CSRF on mutating POSTs. Rust handlers + Jinja templates only (minified `admin.css` ignored). Read: `lib.rs`, `auth.rs`, `permcheck.rs`, all of `handlers/*`, `util.rs`, `engine.rs`, `view.rs`, `files.rs`, and the templates.

---

## A. Executive summary

The admin's core CRUD authorization is solid and consistent: every full-page and sheet CRUD handler (`detail`, `new_form`/`create`, `edit_form`/`update`, `delete`/`htmx_delete`, `preview_sheet`, `edit_sheet_handler`, `confirm_delete_dialog`, `sheet_create`, `cell_edit_*`, `change_password`, `fk_options*`, bulk `run_action`/`dispatch_action`) gates on `require_staff` **and** the matching `permcheck::require(View/Add/Change/Delete)`, with superuser bypass and a graceful "permissions plugin absent → staff-only" fallback. The custom-view permission gate genuinely works at all three levels (page, sidebar, and the widget-data API), matching the docs.

Three problems dominate. **(1)** The Jinja layer's HTML autoescaping is context-blind: model data and reflected query params are dropped into inline `on*` event-handler attributes as JavaScript string literals in several places, where HTML-entity escaping does not prevent JS-string breakout. This yields a **reflected XSS** via the `?search=`/`?sort=`/`?order=` params on the filter-dialog fragment and **stored XSS** via uploaded filenames and editable cell values — in the admin origin, against higher-privileged staff. **(2)** Three read endpoints — `palette_search`, `filter_dialog_handler`, `history_handler` — enforce `require_staff` but **skip the per-model `view_<model>` check** that the rest of the admin applies, so a staff user restricted from a model can still read its rows/facet-values/audit-trail. The FK-picker and bulk-action handlers were explicitly patched for exactly this class ("WEB-7"); these three were missed. **(3)** Only the login POST self-verifies CSRF; every other mutating admin endpoint relies entirely on an externally-mounted `SecurityPlugin` middleware that is **not** a declared plugin dependency.

Could not assess from this crate alone: the session-cookie `SameSite`/`Secure`/`HttpOnly` flags (in `umbral-sessions`), the storage layer's filename sanitization / path-traversal handling and where `/media/` is served from (in core `umbral::storage`), and whether `SecurityPlugin`'s CSRF middleware is mounted in a given deployment. These bound the real-world severity of the CSRF and upload findings and are listed in Blind spots.

Most urgent: the context-blind XSS (reflected variant needs no privileges), then the palette cross-model data disclosure, then the CSRF dependency gap.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | XSS | `templates/_macros/filter_dialog.html:186,198`; `templates/_macros/previews/image.html:22`; `src/handlers/inline_edit.rs:90`; `templates/_macros/sheet.html:264` | User/model data interpolated into inline `on*` (`onclick`/`onkeydown`) attributes as JS string literals. HTML autoescape (and `util::html_escape`) escape `'`→`&#x27;`, which the HTML parser decodes back to `'` before the JS runs → JS-string breakout. | Reflected XSS via `?search=`/`?sort=`/`?order=` on the filter-dialog fragment (no privileges needed); stored XSS via uploaded filenames and via any editable cell value (fires on thumbnail click / Escape while editing). Runs in the admin origin → session/account takeover of higher-priv staff. | Do not put untrusted data inside inline event-handler JS strings. Move to `data-*` attributes read by JS, or JS-encode (`\xHH`/`\uHHHH`) in a dedicated `escapejs` filter/helper. See C.1. | ✅ done |
| 2 | HIGH | AuthZ / IDOR | `src/handlers/palette.rs:71-160` | `palette_search` gates on `require_staff` only; it iterates `discover_models()` and returns matching rows (model name + label column + PK) for **every** registered model. No `permcheck::require(View)` per model. | Any staff user can search/read row labels + PKs (e.g. `auth_user` usernames/emails) across models they have no `view_<model>` right to — the ⌘K palette bypasses per-model permissions entirely. | Filter each model in the loop by `permcheck::check(&user, plugin, table, View)` (load the user's codename set once); skip models the user can't view. See C.2. | ✅ done |
| 3 | MEDIUM | AuthZ | `src/handlers/list.rs:833-886` (`filter_dialog_handler`) | Gates on `require_staff` but has **no** `permcheck::require(View)`. Builds facets via `fetch_distinct_values`, returning distinct column values (e.g. statuses, categories, distinct emails) for any table. | A staff user without `view_<model>` reads distinct facet values for a model they're barred from. Same class as #2, narrower payload. | Add `let (plugin, model) = find_model(...)`; `permcheck::require(&user, &plugin, &table, View)?` before building facets. | ✅ done |
| 4 | MEDIUM | AuthZ | `src/handlers/history.rs:17-43` | `history_handler` gates on `require_staff` only; renders the 50 most recent audit entries (change descriptions like "updated Product #5", "changed password on …") for any `(table,id)`. No per-model `View` check. | Staff without `view_<model>` reads the object's audit trail across the permission boundary. | Add `permcheck::require(&user, &plugin, &table, View)` after `find_model`. | ✅ done |
| 5 | MEDIUM | CSRF | `src/lib.rs:600-604` (deps); `src/handlers/crud.rs`, `sheet.rs`, `actions.rs`, `inline_edit.rs`, `upload.rs`, `prefs.rs`, `dashboard.rs` (all POST/PUT/DELETE handlers) | Only `login_post` self-verifies CSRF (`auth.rs:136-151`). Every other mutating endpoint has no in-handler CSRF check and relies on `SecurityPlugin`'s middleware — but the admin declares deps `["auth","sessions"]` only, not `security`. | If `SecurityPlugin` isn't mounted (or the session cookie isn't `SameSite=Lax/Strict`), all state-changing admin actions (create/update/delete/bulk-action/inline-edit/upload/prefs) are CSRF-forgeable. Login is protected; the actions that matter are not. | Either add `security` to `Plugin::dependencies()` and fail boot without it, or self-verify CSRF in the mutating handlers the way `login_post` does. Document the session-cookie `SameSite` requirement. | **PARTIALLY ADDRESSED 2026-07-07.** The primary defense is confirmed: the session cookie defaults to **`SameSite=Lax`** (umbral-sessions, tested in `same_site_cookie.rs`), which blocks the forged cross-site POST/PUT/DELETE that would carry the cookie — so the *default* admin posture is not CSRF-forgeable. The residual risk is the explicit `SameSite=None` config (cross-origin SPA). For that, the admin's `on_ready` now **warns loudly** (reads `umbral_sessions::configured_same_site()`; reliable because `on_ready` runs in topological order and admin depends on `sessions`) that mutations rely on a CSRF middleware when `SameSite=None`. The hard-dependency change and the per-handler CSRF self-verify remain deferred (Group B) exactly as noted here. |
| 6 | LOW | Upload validation | `src/handlers/upload.rs:130-144` | Image allow-list checks only the **client-declared** part `Content-Type`; no magic-byte sniff. Mitigated by the storage layer renaming `.svg`/`.html`→`.txt` (per comment). | A polyglot/mislabeled file can be stored under an image MIME; served-content risk is bounded by the storage active-content guard (out of this crate). | Sniff the leading bytes (e.g. `infer`/magic) and reject on mismatch; don't trust the declared type. | deferred: LOW, outside the prioritized scope (XSS / palette perms / CSRF). Fix requires adding a new magic-byte crate dependency (`infer`) to the plugin; already mitigated by the storage layer's `.svg`/`.html`→`.txt` active-content rename. |
| 7 | LOW | XSS (self-inflicted) | `templates/_macros/data_table.html:530,550`; `rows_fragment.html:193,212` (`action.confirm`); `_macros/filter_dialog.html:138` (`facet.field`) | Developer-supplied `Action::confirm` text and configured field names are placed in `onclick`/`oninput` JS strings unescaped for the JS context. Not end-user input, so it's a robustness/defense-in-depth issue, but the same context-blind pattern as #1. | A developer confirm string containing `'` breaks the handler; broader risk if a config value is ever sourced from data. | Fold into the #1 fix (JS-encode all interpolation into `on*` handlers). | ✅ done |

No issues found in: the CRUD/sheet/action/fk-picker authorization paths (all correctly double-gated), the `tojson` filter (correctly escapes `</script>` breakout — see `engine.rs:42-59` + its test), `prefs` IDOR (scoped to the session `user.id`), `sanitise_next` open-redirect (rejects `//` and `://`; residual only under the unusual `.at("/")` empty-base config — noted in Blind spots).

---

## C. Detailed findings (HIGH)

### C.1 — Context-blind autoescaping → XSS in inline event handlers (Finding 1)

**Reflected variant (no privileges).** `filter_dialog_handler` reflects the raw `?search=` param and renders the fragment:

```rust
// src/handlers/list.rs:888
let search_val = params.get("search").cloned().unwrap_or_default();
// ... rendered into filter_dialog_fragment.html → filter_dialog macro
```

```jinja
{# templates/_macros/filter_dialog.html:198 #}
onclick="umbral._filterApply('{{ model.table }}', '{{ search_val }}', '{{ sort_col }}', '{{ sort_order }}')"
```

`{{ search_val }}` is autoescaped for **HTML** (`'` → `&#x27;`). The browser's HTML parser decodes the attribute value *before* the JS engine sees it, so `&#x27;` becomes a literal `'` inside the `onclick` JS source.

Attack: a staff victim opens
`GET /admin/product/filter-dialog?search=');alert(document.cookie);('`
(a fragment endpoint that returns `text/html`, so it renders standalone, and the filter dialog is also HTMX-loaded with the live query string). The rendered handler becomes:

```js
umbral._filterApply('product', '');alert(document.cookie);('', '', '')
```

`alert(document.cookie)` runs in the admin origin. `sort`/`order` are identical sinks.

**Stored variant.** `image_thumb` puts the uploaded filename into an `onclick` JS string:

```jinja
{# templates/_macros/previews/image.html:22 #}
onclick="umbral._openImageLightbox(this, '{{ descriptor.url }}', '{{ descriptor.filename }}')"
```

An attacker who can upload/name a file `x');alert(1);//.png` stores XSS that fires when any staff user clicks the thumbnail. The inline-edit editor has the same shape hand-built in Rust:

```rust
// src/handlers/inline_edit.rs:90 (inside cell_edit_get)
onkeydown="if(event.key==='Escape'){{ ... innerHTML = '<span ...>{escaped_value}</span>')}}"
// escaped_value = html_escape(&value)  → escapes '→&#x27;, decoded back to ' in the attr
```

A cell value containing `'` breaks out of the JS string and executes on Escape — stored XSS that a low-privileged editor can plant in a field a higher-privileged admin later edits. (`sheet.html:264` `instance_id` is the same sink for string-PK models.)

**Root cause:** HTML-context escaping is the wrong encoding for a JavaScript-string context. `util::html_escape` (`util.rs:22-28`) and minijinja's HTML autoescape both only make output safe between tags / in ordinary attributes — not inside `on*` handler JS.

**Fix — stop nesting untrusted data in inline JS; pass via data attributes:**

```jinja
{# filter_dialog.html — no data in the handler; JS reads inputs/dataset #}
<button type="button"
        class="filter-apply"
        data-table="{{ model.table }}"
        data-search="{{ search_val }}"
        data-sort="{{ sort_col }}"
        data-order="{{ sort_order }}">Apply</button>
{# admin.js: el.addEventListener('click', e => umbral._filterApply(e.currentTarget.dataset)); #}
```

```jinja
{# image.html #}
<button type="button" class="img-thumb"
        data-url="{{ descriptor.url }}" data-filename="{{ descriptor.filename }}"> … </button>
```

For the Rust-built `cell_edit_get`, drop the `onkeydown` inline entirely and bind Escape in `admin.js`; if inline JS is unavoidable, JS-encode instead of HTML-encode:

```rust
/// Encode for a single/double-quoted JS string literal: escape the
/// quote chars, backslash, and the HTML/line-terminator chars.
fn escape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' | '"' | '\\' | '`' => { out.push('\\'); out.push(c); }
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}
// then: data-* attribute (HTML-escaped) is still preferable to inline JS.
```

### C.2 — `palette_search` ignores per-model view permission (Finding 2)

```rust
// src/handlers/palette.rs:71-130 (abridged)
pub(crate) async fn palette_search(State(state), headers, Query(params)) -> Response {
    if let Err(r) = require_staff(&headers, "/admin/api/palette/search").await { return r; }
    // ...
    for (_, model) in discover_models() {              // EVERY model, no perm filter
        let rows = DynQuerySet::for_meta(&model)
            .select_cols(&select_cols)
            .search(&search_fields, query_term)
            .limit(remaining)
            .fetch_as_strings().await ...;
        // returns model.name + label + pk for each match
    }
}
```

The FK-picker handlers were fixed for this exact issue ("WEB-7", `fk_picker.rs:56`, `243`); the palette search was not. `palette_fragment` (the jump-target list) *is* filtered via `sidebar_apps`, but the search endpoint bypasses it.

**Scenario:** a support agent given only `orders.view_order` types `@example.com` into ⌘K and reads matching `auth_user` rows (usernames, and the first text column — often email), plus PKs for deep-linking, across every table in the database.

**Fix — resolve the viewer's codenames once and gate each model:**

```rust
let user = match require_staff(&headers, "/admin/api/palette/search").await {
    Ok(u) => u, Err(r) => return r,
};
// ...
for (plugin_name, model) in discover_models() {
    if total_found >= MAX_RESULTS { break; }
    // Skip models the viewer cannot view (no-op when permissions absent / superuser).
    if !crate::permcheck::check(&user, &plugin_name, &model.table,
                                crate::permcheck::Action::View).await {
        continue;
    }
    // ... existing search ...
}
```

(For efficiency, load `user_perms` once and check membership in-memory as `AdminPerms::load` does, rather than one `check` await per model.)

---

## D. Blind spots (could not verify from this crate)

- **Session cookie flags** (`SameSite`/`Secure`/`HttpOnly`) live in `umbral-sessions` — they determine the real exploitability of Finding 5 (CSRF). If the session cookie is `SameSite=Lax/Strict`, cross-site CSRF is largely neutralized regardless of the missing in-handler checks.
- **Whether `SecurityPlugin` is mounted** in a given deployment — Finding 5 is fully mitigated when it is, and its middleware also enforces CSRF for the non-login POSTs. Not observable from the admin crate.
- **Storage layer** (`umbral::storage`): filename→key sanitization, path-traversal / symlink escape, the SVG/HTML→`.txt` active-content rename, and where `/media/<key>` is served from and with what `Content-Type`/`Content-Disposition`. These bound Finding 6 and the stored-XSS-via-served-file surface. `files.rs`/`upload.rs` only build descriptors and hand bytes to `storage.store`.
- **`descriptor.filename` provenance** for Finding 1's stored variant — the exact path by which a file descriptor's `filename` is populated (raw upload name vs. sanitized) is set outside this crate; the template sink is vulnerable regardless of source.
- **`sanitise_next` under `.at("")`/`.at("/")`** (empty base): `!trimmed.starts_with("")` is always false, so the "must start with base" guard is void; combined with the `://`-only scheme check, a value like `https:evil.com` (no `//`) could slip through `Redirect::to`. Only reachable under the unusual root-mount config; not verified end-to-end.
- **Rate limiting** on `login_post` and the search/COUNT-heavy endpoints — not present in this crate; brute-force protection would live in `umbral-auth`/`SecurityPlugin`.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Add the missing `permcheck::require(View)` to `filter_dialog_handler` (#3) and `history_handler` (#4), and the per-model `check(View)` loop-filter to `palette_search` (#2). Mirror the existing WEB-7 pattern.
2. Remove the `onkeydown` inline handler in `cell_edit_get` (bind Escape in `admin.js`) — closes the hand-built Rust XSS sink (#1, stored).

**Short term (< 2 weeks)**
3. Sweep every `on*="…{{ }}…"` / hand-built `on*` in templates + handlers; move interpolated values to `data-*` attributes consumed by `admin.js`, or add and use an `escapejs` filter/helper. Covers the reflected `search`/`sort`/`order` sink, `image_thumb`, `sheet.html` `instance_id`, and the developer-config sinks (#1, #7).
4. Resolve the CSRF dependency gap (#5): add `security` to `AdminPlugin::dependencies()` (fail boot if absent) or self-verify CSRF in the mutating handlers; document the session-cookie `SameSite` requirement.
5. Add magic-byte sniffing to `upload_image` (#6).

**Structural (needs design work)**
6. Introduce a context-aware escaping convention across the admin (HTML text vs. HTML attribute vs. JS-string vs. URL), and a lint/grep gate in CI for `on*=` attributes containing `{{`. The current single autoescape mode invites the whole Finding-1 class to recur.
7. Consider a single authorization middleware/extractor that resolves `(staff + per-model perms)` once per request, so a new endpoint cannot ship without the check — the three misses (#2/#3/#4) exist because the gate is copy-pasted per handler.

## Docs updated

None. The pages I own — `documentation/docs/v0.0.1/admin/{custom-views,inlines,widgets}.mdx` — do not contradict the code. In particular, `custom-views.mdx:64`'s claim that the custom-view permission gate applies at "page, sidebar, and data API" levels is **accurate**: `custom_view` checks `require_codename` (`handlers/custom_view.rs:35`), `view_groups` filters the sidebar (`view.rs:120`), and `dashboard_widget_data` enforces `widget_gates` before serving data (`handlers/dashboard.rs:255`). No fabricated edits made. (Note: the broader `documentation/docs/v0.0.1/plugins/admin` page, if it claims all model endpoints are permission-gated, would contradict Findings 2-4 — but that page is outside this audit's owned folder.)
