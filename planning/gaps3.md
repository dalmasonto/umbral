# Seen/Known gaps - Continued from @gaps2.md

1. [x] REST `views([...])` means read-only everywhere (routes, OPTIONS Allow, OpenAPI spec, 405 vs 404) — archived
2. [ ] Push notifications implementations
3. [ ] Can one stream a video
4. [x] Flash messages no-op without a pre-existing session — resolved (works with SessionsPlugin; was a test-harness misconfig + doc error) — archived

The original framing was wrong. `session_layer` (mounted by `SessionsPlugin::wrap_router`, default-on) injects a candidate `SessionToken` into every request extension including cookieless ones (the `fresh = true` path). `Messages::from_request_parts` prefers this extension over the raw cookie, so on a brand-new anonymous visitor's first submit: `session_layer` provides the token → `Messages::add` materialises the session row (lazy, side-channel write) → `session_layer` emits `Set-Cookie` on the response. Flash feedback for anonymous first-visit failures works end-to-end **when `SessionsPlugin` is mounted**.

The only configuration where it breaks is `AuthPlugin` booted ALONE without `SessionsPlugin` — a degenerate test harness config, not a real app config. The `form_surface.rs` test used exactly that broken boot; the fix (done in feat/auth-full-surface) is to mount `SessionsPlugin` in the test and assert the session cookie is set on a failed login. Discovered in the Task 14 review; closed here.