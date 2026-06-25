# Automatic CSRF: the middleware is the only mint, templates get the token for free

**Status:** accepted 2026-06-10. **Closes:** gaps2 #26 (signed CSRF default), the manual-CSRF ceremony in `examples/shop/src/views/public.rs`, and the admin's unprotected htmx writes.

## Problem

The validation half of CSRF is already automatic (`umbral-security`'s `csrf_middleware` rejects unsafe methods without a matching token). The token-delivery half is manual: every form view calls `ensure_csrf_cookie(&headers)`, threads `csrf_token` through its render helpers, and hand-attaches the `Set-Cookie`. The target experience is that a developer writes one token tag (`{% csrf_token %}`) in the template and nothing else; today umbral's developer writes ~20 lines of plumbing per form view.

Three secondary problems share the same root cause (no single owner for token minting):

- The admin plugin self-mints (`ensure_csrf_token` in `plugins/umbral-admin/src/auth.rs`), forcing the middleware to carry deference logic (`response_sets_csrf_cookie`) and blocking the `signed_csrf` default flip (gaps2 #26).
- The admin's htmx CRUD requests (sheet create/edit, inline edit, delete, actions) carry **no token at all** — only `login.html` does. With SecurityPlugin mounted over the admin (as the shop now does), every admin write 403s.
- The middleware attaches its cookie with `headers_mut().insert(SET_COOKIE, …)`, which replaces any cookie the handler set (e.g. a session cookie) instead of appending.

## Decision

Adopt a two-part split (validation middleware + a template token tag), reusing the existing `CURRENT_USER` task-local seam in `umbral-core`:

1. **Core (`crates/umbral-core/src/templates.rs`).** New `CURRENT_CSRF: Option<String>` task-local + `with_current_csrf(token, fut)` scope fn + `current_csrf() -> Option<String>` read accessor. `render` merges `csrf_token` (raw value, for `X-CSRF-Token` headers / htmx) and `csrf_input` (pre-built safe-string hidden `<input name="csrf_token">`, the value the `{% csrf_token %}` tag emits) into every template context. Explicit ctx keys win, same precedence as the `user` merge. Facade re-export under `umbral::templates`, not in the prelude.

2. **Middleware is the only mint (`plugins/umbral-security/src/lib.rs`).** `csrf_middleware` resolves the cookie token for *all* methods (POST error re-renders need it in scope too), mints **before** `next.run` on safe methods when the cookie is missing **or fails signed-mode validation** (rotation — so flipping `signed_csrf` on doesn't 403 browsers holding old unsigned cookies), scopes `with_current_csrf` around the handler, and `append`s (not `insert`s) the `Set-Cookie` when it minted. `ensure_csrf_cookie` and `response_sets_csrf_cookie` are **deleted** — with the task-local in place no handler ever needs to mint.

3. **`signed_csrf` defaults to `true`.** Tokens become `<random>.<HMAC-SHA256(secret_key, random)>`. Safe now because: (a) the middleware is the only mint when SecurityPlugin is mounted, (b) rotation re-mints stale unsigned cookies on the next safe request, (c) with no resolvable `secret_key` the mint degrades to plain double-submit instead of locking writes out.

4. **Admin prefers the ambient token (`plugins/umbral-admin/src/auth.rs`).** `ensure_csrf_token` tries `umbral::templates::current_csrf()` first (SecurityPlugin mounted → middleware minted, admin sets no cookie); falls back to the existing cookie-read + self-mint only when nothing is in scope (SecurityPlugin absent → admin stays self-protecting). Its token comparison switches to constant-time equality via a shared public helper in `umbral-security` (the private `tokens_match` becomes `pub`).

5. **Admin htmx carries the token.** `wrapper.html`'s `<body>` gains `hx-headers='{"X-CSRF-Token": "{{ csrf_token }}"}'` (htmx inherits headers to all descendant requests); raw `fetch()` calls in `admin.js` read the (deliberately non-HttpOnly) cookie.

6. **Shop drops every CSRF line.** `contact` loses the `HeaderMap` param, mint call, and `Set-Cookie` plumbing; `submit_contact` loses the manual token read; the render helpers lose the `csrf_token` parameter; `contact.html` uses `{{ csrf_input }}`.

## Alternatives rejected

- **`CsrfToken` axum extractor** — still per-handler boilerplate; doesn't reach "developer writes nothing in the view".
- **Post-render HTML rewriting** (inject hidden inputs into every `<form method="post">` via lol_html) — streaming-parse CPU on every response; same trade-off already rejected in gaps2 #21 Option B.
- **Keeping `ensure_csrf_cookie` as a deprecated escape hatch** — keeping it requires keeping the middleware deference check, which is the complexity this design exists to delete.

## Consequences

- A form view is CSRF-complete with zero view code and one template token. REST/API paths keep using `csrf_exempt_paths`.
- Exactly one mint site when SecurityPlugin is mounted; the admin's fallback mint runs only when it isn't.
- Old unsigned cookies rotate transparently; in-flight forms rendered before a deploy lose one POST (403, refresh re-renders with a valid token), an acceptable trade-off for a cookie-secret rotation.
- Apps that template `csrf_token` explicitly keep working (explicit ctx wins over the merge).

## Tests

Core: render-merge in scope / out of scope / explicit-ctx-wins / `csrf_input` escapes correctly. Security: first-visit GET renders a token the subsequent POST validates end-to-end; POST error re-render has the token in scope; unsigned→signed rotation; session `Set-Cookie` survives the append; exempt paths bypass. Admin: ambient-preferred over self-mint; htmx header present in wrapper. Shop: workspace build + existing form tests.
