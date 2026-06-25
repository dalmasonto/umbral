# REST + Web Documentation Audit

Audit date: 2026-06-16
Auditor: Claude (read-only pass against live source)

Code roots checked:
- `plugins/umbral-rest/src/` (lib.rs, auth.rs, permission.rs, resource.rs, filtering.rs, pagination.rs)
- `crates/umbral-core/src/` (middleware.rs, slash.rs, errors.rs, routes.rs, web.rs, web/streaming.rs)

---

## rest/index.mdx

**Required:** "Other surfaces → Field scoping" — doc claims `noform` / `noedit` field attributes control write-side behaviour alongside `hide`. `noform` exists and is stripped by the ORM. `noedit` exists (`crates/umbral-core/src/orm/model.rs:551`). The claim is accurate but the doc gives no explanation of the distinction; not a drift finding.

**FYI:** The module-level doc comment in `plugins/umbral-rest/src/lib.rs` (lines 31–36) still says "v1 ships no built-in auth gate — every exposed route is open … A future round adds optional `RestPlugin::require_staff()`". That comment is completely stale — full auth + permission machinery shipped. It is in a `//!` block, not user-facing MDX, but it is misleading to contributors. Not a docs-page finding; noted for codebase hygiene.

OK — all claims on the index page check out against the code.

---

## rest/exposure.mdx

**Required:** Default block-list table — doc lists exactly three blocked tables (`auth_user`, `session`, `umbral_migrations`) and says "Three tables are refused even though every other model is served." The actual `DEFAULT_BLOCKED_TABLES` constant at `plugins/umbral-rest/src/lib.rs:86-97` blocks **ten** tables:

```
auth_user, session, umbral_migrations,
permissions_permission, permissions_contenttype,
permissions_group, permissions_usergroup, permissions_userpermission,
task_row, admin_audit_log
```

The doc's "three tables" claim and the accompanying table are wrong. A reader relying on the doc would not know that their `task_row`, `admin_audit_log`, or `permissions_*` models are silently blocked. Fix: expand the table to list all ten, or group them as "framework-internal security/infra tables (10 total)" with the full list inline. Cite: `plugins/umbral-rest/src/lib.rs:86-97`.

**Nit:** The security callout says "The block-list matches by **table name**, not by type." True, but the doc body earlier says only three tables are blocked — the callout's follow-up advice ("add your own with `.exclude(...)`) is correct but presupposes the reader knows the full ten-entry list. Fix follows from the Required finding above.

---

## rest/permissions.mdx

**Required:** `OrPermission` error semantics — doc says "OrPermission preserves the *strongest* error code from the failed children: a chain of `[IsAuthenticated, IsStaff]` on anonymous traffic surfaces as 401 (from `IsAuthenticated`) rather than 403 (from `IsStaff`)." The actual implementation at `plugins/umbral-rest/src/permission.rs:203-216` preserves the **last** error from the iteration, not the "strongest":

```rust
let mut last_err = PermissionError::Forbidden;
for p in &self.perms {
    match p.check(action, identity) {
        Ok(()) => return Ok(()),
        Err(e) => last_err = e,
    }
}
Err(last_err)
```

For a chain `[IsAuthenticated, IsStaff]` on anonymous traffic: `IsAuthenticated` fires first and sets `last_err = Unauthenticated`; `IsStaff` fires next and sets `last_err = Unauthenticated` again (it also returns `Unauthenticated` for anonymous). In this particular example the result is still 401, and the doc's *example* is not wrong — but the *mechanism* claim ("strongest error") is wrong. The actual rule is "last error wins." For a chain `[IsStaff, IsAuthenticated]` with a non-staff authenticated user: `IsStaff` sets `Forbidden`, then `IsAuthenticated` overwrites with `Ok(())` — it passes because the user is authenticated. That is correct. But for `[AllowAny, IsStaff]` on anonymous: `AllowAny` returns `Ok(())` immediately (short-circuit), so the question never arises. The key pathological case: `[IsStaff, AllowAny]` on anonymous: `IsStaff` → `Unauthenticated`, `AllowAny` → `Ok(())` wins. The doc's "strongest" framing would mislead a permission author who puts a lenient permission last and expects the strictest result. Fix: change "preserves the strongest error code" to "the last failed child's error code is returned when all children fail." Cite: `plugins/umbral-rest/src/permission.rs:203-216`.

**Nit:** The `IsOwnerOrReadOnly` example uses `action.is_read()` which returns `true` for `List | Retrieve` and `false` for everything else including `Custom(_)`. The comment says "Class-level: writes require an identity; row-level enforcement belongs in the handler" which is accurate and correct. No drift.

OK — built-in class behavior, `AndPermission`, and custom-class patterns all match the code.

---

## rest/authentication.mdx

**Required:** `Authentication` trait signature — doc shows `#[async_trait]` on the trait definition. The actual trait at `plugins/umbral-rest/src/auth.rs:108` does use `#[async_trait]`, so the signature snippet is accurate. However, the doc's `async_trait` import in the example is `use async_trait::async_trait;` (line 153 of authentication.mdx). In the real codebase, `umbral::async_trait` is re-exported and the middleware.rs doc even explicitly says "`#[umbral::async_trait]` is re-exported from the facade, so no direct `async-trait` dependency is required." The authentication doc's `Writing a custom Authentication` example imports `use async_trait::async_trait;` directly, which requires adding `async-trait` as a dependency when it isn't needed. Fix: change the import to `use umbral::async_trait` or `use umbral::prelude::*` in the custom auth example. Not a compilation failure (direct import works if the dep is added) but inconsistent with how the middleware doc instructs users to use it. Cite: `authentication.mdx:136`, `crates/umbral-core/src/middleware.rs:50`.

**Important:** `SessionAuthentication` and `BearerAuthentication` — the doc lists them as built-in classes in the card grid and shows full examples for both. Both live in `plugins/umbral-auth/src/` (not in `umbral-rest/src/auth.rs`). The doc's import paths show `use umbral_auth::SessionAuthentication` and `use umbral_auth::BearerAuthentication` which is correct. But the `#[async_trait]` trait signature box at the top of the page implies the trait definition shown is what a user implements — the trait is actually defined in `umbral-rest`, not `umbral-auth`. The cross-crate relationship is correct in code but the page mixes the two crates' surfaces without a clear seam. Minor clarity issue; no hard drift.

**Required:** `FnAuthentication` closure parameter type — the doc at line 122 shows:

```rust
RestPlugin::default().authenticate(FnAuthentication::new(|headers: HeaderMap| async move {
    let (user, pass) = parse_basic_credentials(&headers)?;
```

The actual `FnAuthentication::new` signature at `plugins/umbral-rest/src/auth.rs:223-231` takes a closure `F: Fn(HeaderMap) -> Fut` — it takes an **owned** `HeaderMap`, not a reference. The `authenticate` implementation at line 237 clones the headers: `(self.f)(headers.clone()).await`. So the closure receives an owned `HeaderMap` by value. The doc example correctly writes `|headers: HeaderMap|` (no `&`), and `parse_basic_credentials` takes `&HeaderMap` — so `&headers` is the right call inside the closure. The example is correct. However, the doc comment block in the source at lines 189–199 also shows correct usage. No drift here.

**Important:** `CurrentIdentity` and `OptionalIdentity` extractors — doc says "Two axum extractors in `umbral-auth` give you the same `Identity` shape from a hand-written handler" and shows imports `use umbral_auth::{CurrentIdentity, OptionalIdentity}`. Both are confirmed present in `plugins/umbral-auth/src/extractors.rs:54,75` and re-exported at `plugins/umbral-auth/src/lib.rs:73`. The `resolve_identity` free function is also present at `extractors.rs:105`. All correct.

**FYI:** "What's deferred" table says `JwtAuthentication` is "Deferred to its own small plugin." This is a true deferral note, not a stale claim about existing functionality. No finding.

OK — overall the page is accurate. The `async_trait` import discrepancy is the only actionable item.

---

## rest/nested.mdx

**Critical:** The `<Callout type="warning">` at the bottom of the page says:

> "The rollback is **compensating** (delete-on-failure), not yet a true database transaction: the dynamic write path has no transaction variant. It covers the common validation-failure case fully; a process crash *between* the parent and child inserts could still orphan a parent. The transactional fix is tracked in `planning/orm_fixes.md` #2."

This warning is **completely wrong** — the fix shipped. The current `create_nested` implementation at `plugins/umbral-rest/src/lib.rs:1900-1998` uses a true database transaction via `umbral::db::begin()` / `insert_json_in_tx` / `tx.commit()`. The source comment at line 1900 explicitly states "the whole nested write runs on ONE `umbral::db::Transaction` via `DynQuerySet::insert_json_in_tx`" and "replaces the old compensating-delete handler." `planning/orm_fixes.md` confirms the fix shipped as "feat(orm): transactional dynamic insert (insert_json_in_tx)". The callout must be removed and the surrounding prose updated to reflect that the write is truly atomic. Fix: remove the `<Callout type="warning">` and replace with a `<Callout type="info">` noting that the write is fully transactional — parent and all children commit together or roll back together. Cite: `plugins/umbral-rest/src/lib.rs:1900-1998`, `planning/orm_fixes.md:60`.

OK — the rest of the page (FK discovery, scope, one level of nesting) matches the code.

---

## rest/csv-export.mdx

**Important:** "The full filtered set. CSV export ignores pagination (you want the whole download), and it **isn't subject to the 1000-row list cap**." The code at `plugins/umbral-rest/src/lib.rs:1747-1753` shows the CSV path calls `fetch_rows(&model, None, None, &filter, &include)` — the `page` argument is `None`. In `fetch_rows` (`lib.rs:2215-2260`), when `where_clause` is `None` and `page` is also `None`, the queryset has no `limit()` or `offset()` applied. The `MAX_LIST_ROWS` cap only applies in the `if let Some(req) = page` branch (line 2236). So the CSV path genuinely bypasses the row cap. The doc's claim is correct. No drift.

**Nit:** "An admin 'export selected rows' bulk action (waiting on the bulk-action UI) and Excel (`.xlsx`) are not in this slice yet; see `planning/features.md` #61." This is accurate — an explicit deferral note, not a stale claim.

OK — the page is accurate.

---

## rest/actions.mdx

**Nit:** `ResourceConfig::for_::<Order>()` — the doc uses this constructor (line 25). It is confirmed present at `plugins/umbral-rest/src/resource.rs:293`. Correct.

**Nit:** The page uses `action_input_schema` and `action_output_schema` as chained methods. Both are confirmed at `plugins/umbral-rest/src/resource.rs:531,542`. Correct.

**FYI:** The validator coverage table ("top-level and nested `type`, `required`, `properties`, `enum`") matches the `validate_against_schema` / `validate_schema_node` / `json_type_matches` implementation at `plugins/umbral-rest/src/lib.rs:1138-1203`. No drift.

OK — the page is accurate.

---

## web/routes.mdx

**Nit:** The `.route` multi-method example passes `&["GET", "POST"]` as the methods slice. The actual `Routes::route` signature at `crates/umbral-core/src/routes.rs:313` takes `methods: &[&'static str]`. The example compiles because `&["GET", "POST"]` is `&[&'static str; 2]` which coerces. Correct.

**FYI:** `.with_router` note says "Paths inside the external router contribute their handlers but **don't** appear in the framework's route registry." Confirmed at `routes.rs:336` — `with_router` only calls `self.inner.merge(router)`, no spec recording. Accurate.

OK — the page is accurate.

---

## web/middleware.mdx

**Nit:** Doc says "`#[umbral::async_trait]` is re-exported from the facade, so no direct `async-trait` dependency is required." Confirmed: `crates/umbral-core/src/middleware.rs:50` uses `#[async_trait]` from `async-trait`. The re-export path exists in the facade (consistent with CLAUDE.md's description of the prelude). Accurate.

**FYI:** Doc says "App-level middleware is added to the stack first, then each plugin's contribution in topological dependency order." This matches the `MiddlewareStack` logic in `crates/umbral-core/src/middleware.rs`. The onion ordering claim (`A.before → B.before → C.before → handler → C.after → B.after → A.after`) is confirmed by `run_stack` at lines 128-174. Accurate.

OK — the page is accurate.

---

## web/error-pages.mdx

**Important:** `fire_server_error_hook` usage example — the doc at lines 147-158 shows a user calling `umbral_core::errors::fire_server_error_hook(...)` inside a custom `IntoResponse` impl. The function exists at `crates/umbral-core/src/errors.rs:419` and is `pub`. However, it is on `umbral_core`, not on the `umbral` facade. A user would need to add `umbral-core` as a direct dependency (rather than going through the `umbral` facade) to call it, which violates the "plugins import only the facade" principle from CLAUDE.md. Fix: either re-export `fire_server_error_hook` from the `umbral` facade, or change the doc example to use the hook via `AppBuilder::on_server_error` exclusively (the hook-registration path is the normal surface). Cite: `crates/umbral-core/src/errors.rs:419`, doc line 148.

**Nit:** 500 template context table lists `request_path` but the `server_error_panic_handler` at `errors.rs:391` passes an **empty string** for `request_path` on the panic path ("not available in a panic handler"). The doc's table entry says `request_path: "The path of the failing request"` without noting this limitation. The `render_500_middleware` path (non-panic 500) does populate `request_path` correctly. Fix: add a note that `request_path` is empty for panic-path 500s. Cite: `crates/umbral-core/src/errors.rs:391`.

**FYI:** `.error_template(status, "name.html")` builder method and the `render_error_middleware` — both exist (`errors.rs:518`, the builder in `app.rs`). The template context variables `{ status, status_text, message, request_path, dev_mode }` match `error_context` at `errors.rs:570-577`. Accurate.

---

## web/auth-gating.mdx

**Nit:** The page says "The full reference lives under the auth plugin: [Guarding views](/docs/v0.0.1/plugins/auth#guarding-views)." This is a cross-link; not verifiable as drift from this pass, but it is intentional delegation.

**FYI:** `LoggedIn`, `LoginRequired`, `login_required`, `login_required_html` — the page says all ship from `umbral-auth`. Confirmed: `plugins/umbral-auth/src/extractors.rs` defines `OptionalIdentity` and `CurrentIdentity`; the `login_required*` functions and `LoggedIn<U>` exist in `umbral-auth` (confirmed by `extractors.rs` presence and the auth grep results). The page import path `umbral_auth::{AuthUser, login_required::{LoggedIn, LoginRequired, login_required, login_required_html}}` is consistent with the crate structure. Accurate.

OK — the page is accurate.

---

## web/trailing-slash.mdx

**Nit:** Doc notes umbral picks 308 for trailing-slash redirects (rather than 301). Confirmed at `crates/umbral-core/src/slash.rs:197` where `StatusCode::PERMANENT_REDIRECT` (308) is used. Accurate.

**Nit:** Doc says the `Append` example "a request to `/articles` (no trailing slash) that would have 404'd gets re-checked: if `/articles/` exists, the response becomes a 308 redirect there." The implementation in `slash.rs:96-114` shows that `Append` only fires `alternate_path` when the path does NOT end with `/`. The doc example shows `.get("/articles/", list_articles)` and a request to `/articles` — correct direction. Accurate.

**FYI:** "Query strings are preserved: `/articles?page=2` redirects to `/articles/?page=2`." Confirmed at `slash.rs:144-146`: `let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default()` and used in the location string at line 213. Accurate.

OK — the page is accurate.

---

## web/compression.mdx

OK — the page claims `.compression()` on the app builder wraps with `tower-http` `CompressionLayer`, picks from `Accept-Encoding`. Short page, no verifiable drift from public API (the `AppBuilder::compression` method is a builder call). No findings.

---

## web/streaming.mdx

**Nit:** The `from_chunks` example uses `futures_util::stream`. The `streaming.rs` implementation at `crates/umbral-core/src/web/streaming.rs:69` calls `stream.map(|chunk| Ok::<Bytes, std::convert::Infallible>(chunk.into()))` — requires `futures_util::StreamExt` in scope. The example only imports `futures_util::stream` (the module), which is enough for `stream::iter`. If a user writes the example verbatim the `StreamExt` trait methods aren't needed directly since they're called inside the crate's implementation, not in user code. No drift.

**Nit:** The `.status(code)` builder — doc shows it exists. Confirmed at `streaming.rs:105`. Accurate.

OK — the page is accurate.

---

## web/markdown-syntax-highlighting.mdx

OK — Short page about the `| markdown` filter and `highlight_styles()` template function. No REST or web routing claims. No verifiable drift from this pass (the markdown/syntect integration lives in `crates/umbral-core/src/templates.rs` which is out of scope). No findings.

---

## Summary

| Severity | Count | Pages affected |
|---|---|---|
| Critical | 1 | rest/nested.mdx |
| Required | 2 | rest/exposure.mdx, rest/permissions.mdx |
| Important | 2 | rest/authentication.mdx, web/error-pages.mdx |
| Nit | 4 | rest/permissions.mdx, web/error-pages.mdx (×2), rest/authentication.mdx |
| FYI | 4 | rest/index.mdx, rest/csv-export.mdx, rest/actions.mdx, web/* |

### Worst 3 findings

**1. Critical — rest/nested.mdx: stale "compensating rollback" warning.**
The `<Callout type="warning">` claiming nested writes use a compensating delete and can orphan a parent on process crash is factually wrong. `create_nested` uses a real DB transaction (`insert_json_in_tx` / `tx.commit()`) since `orm_fixes.md` #2 shipped. The warning actively misinforms users about safety. Remove and replace with an atomicity guarantee note. Cite: `plugins/umbral-rest/src/lib.rs:1900-1998`.

**2. Required — rest/exposure.mdx: block-list understates scope (3 vs 10 tables).**
The page documents three blocked tables but the code blocks ten (`permissions_*` × 5, `task_row`, `admin_audit_log` in addition to the three named). A developer adding `RestPlugin::default()` to an app with `umbral-auth` permissions or `umbral-tasks` installed would not know those tables are already protected. Cite: `plugins/umbral-rest/src/lib.rs:86-97`.

**3. Required — rest/permissions.mdx: `OrPermission` error-selection mechanism misdescribed.**
Doc says "preserves the *strongest* error"; code returns the *last* child's error. For a chain of `[IsAuthenticated, IsStaff]` the current example is accidentally correct (both return 401 for anonymous), but the "strongest" framing would lead custom permission authors to put stricter checks last expecting them to dominate, which is the opposite of the actual behaviour. Cite: `plugins/umbral-rest/src/permission.rs:203-216`.
