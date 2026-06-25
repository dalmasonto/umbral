# Automatic CSRF Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Zero-ceremony CSRF - the SecurityPlugin middleware is the only token mint, templates receive `csrf_token` / `csrf_input` ambiently, view code contains zero CSRF lines.

**Architecture:** A `CURRENT_CSRF` tokio task-local in `umbral-core` (mirroring the existing `CURRENT_USER` seam) is scoped by `csrf_middleware` around every non-exempt request and merged into every `templates::render` context. The middleware mints pre-handler on safe methods (first visit + rotation of stale unsigned cookies), so handlers and the admin never mint. `signed_csrf` flips to default-on.

**Tech Stack:** Rust, axum 0.8, tokio task_local, minijinja, hmac/sha2/subtle (already deps). Spec: `docs/decisions/2026-06-10-automatic-csrf.md`.

**Ground rules for the executor:**
- All workspace commands run from `crates/` (`cd crates`). The shop builds from `examples/shop/` (standalone project).
- NEVER `cargo run` or restart the shop — the user runs it in dev mode (see memory `feedback_dev_server`).
- Before every commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` from `crates/`.

---

### Task 1: Core — `CURRENT_CSRF` task-local + render merge

**Files:**
- Modify: `crates/umbral-core/src/templates.rs` (task_local block ~line 58, `merge_ambient_user` ~line 398-449)
- Modify: `crates/umbral/src/lib.rs:331` (facade re-export)
- Test: `crates/umbral-core/tests/csrf_context.rs` (new)

- [ ] **Step 1: Write the failing tests**

Create `crates/umbral-core/tests/csrf_context.rs`. Follow the `template_discovery.rs` boot pattern (OnceLock-guarded `templates::init` over a TempDir):

```rust
//! The render merge for the ambient CSRF token: `{{ csrf_token }}`
//! (raw value) and `{{ csrf_input }}` (hidden input) appear in every
//! template rendered inside `with_current_csrf` scope; explicit ctx
//! keys win; outside the scope nothing is injected.

use std::fs;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbral_core::templates;

static DIR: OnceLock<TempDir> = OnceLock::new();

fn boot() {
    DIR.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("token.html"),
            "tok=[{{ csrf_token }}] input=[{{ csrf_input }}]",
        )
        .unwrap();
        let _ = templates::init(&[dir.path().to_path_buf()]);
        dir
    });
}

#[tokio::test]
async fn csrf_merged_inside_scope() {
    boot();
    let out = templates::with_current_csrf(Some("abc123".to_string()), async {
        templates::render("token.html", &serde_json::json!({})).unwrap()
    })
    .await;
    assert!(out.contains("tok=[abc123]"), "raw token missing: {out}");
    assert!(
        out.contains(r#"<input type="hidden" name="csrf_token" value="abc123">"#),
        "csrf_input missing or escaped: {out}"
    );
}

#[tokio::test]
async fn explicit_ctx_key_wins_over_merge() {
    boot();
    let out = templates::with_current_csrf(Some("ambient".to_string()), async {
        templates::render("token.html", &serde_json::json!({"csrf_token": "explicit"})).unwrap()
    })
    .await;
    assert!(out.contains("tok=[explicit]"), "explicit ctx lost: {out}");
}

#[tokio::test]
async fn nothing_injected_outside_scope() {
    boot();
    let out = templates::render("token.html", &serde_json::json!({})).unwrap();
    assert!(out.contains("tok=[]"), "token leaked outside scope: {out}");
    assert!(out.contains("input=[]"), "input leaked outside scope: {out}");
}

#[tokio::test]
async fn current_csrf_reads_the_scoped_value() {
    let got = templates::with_current_csrf(Some("xyz".to_string()), async {
        templates::current_csrf()
    })
    .await;
    assert_eq!(got.as_deref(), Some("xyz"));
    assert_eq!(templates::current_csrf(), None);
}
```

NOTE for the executor: check `templates::init`'s exact signature in `templates.rs` (the discovery test calls it with an ordered dir list) and adjust the `init` call to match. If `umbral-core` lacks a `serde_json` dev-dependency, use `minijinja::context!{}` / a small `#[derive(Serialize)]` struct instead.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates && cargo test -p umbral-core --test csrf_context`
Expected: compile error — `with_current_csrf` / `current_csrf` not found.

- [ ] **Step 3: Implement the task-local and the merge**

In `crates/umbral-core/src/templates.rs`, extend the existing `tokio::task_local!` block (line ~58):

```rust
tokio::task_local! {
    pub static CURRENT_USER: Option<minijinja::Value>;

    /// Per-request CSRF token, set by `umbral-security`'s middleware and
    /// read by [`render`] to inject `csrf_token` / `csrf_input` into every
    /// template, so a form template need only drop in `{{ csrf_input }}`.
    /// Outside the middleware's scope nothing is injected.
    pub static CURRENT_CSRF: Option<String>;
}
```

Below `with_current_user`, add:

```rust
/// Run `fut` with the ambient CSRF token scoped for its duration.
/// Intended for the CSRF middleware in `umbral-security`.
pub async fn with_current_csrf<F: std::future::Future>(token: Option<String>, fut: F) -> F::Output {
    CURRENT_CSRF.scope(token, fut).await
}

/// Read the ambient CSRF token, if a middleware has scoped one for this
/// request. Non-template consumers (e.g. the admin's login form) use this
/// to embed the same token the middleware minted.
pub fn current_csrf() -> Option<String> {
    CURRENT_CSRF.try_with(|t| t.clone()).ok().flatten()
}
```

Rename `merge_ambient_user` → `merge_ambient` (update the single call site at ~line 398) and restructure so the early-return doesn't skip the CSRF merge:

```rust
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
            // Token is hex (signed adds `.` + hex), so escaping is
            // belt-and-braces against a future token-shape change.
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
```

Keep the existing doc-comment's intent, extending it to mention both `user` and the CSRF pair. Preserve the comment about the 500-recovery path.

In `crates/umbral/src/lib.rs` line 331, widen the facade re-export:

```rust
pub use umbral_core::templates::{
    CURRENT_CSRF, CURRENT_USER, TemplateError, current_csrf, render, with_current_csrf,
    with_current_user,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd crates && cargo test -p umbral-core --test csrf_context`
Expected: 4 passed. Also run `cargo test -p umbral-core` — the existing template tests (user merge, discovery, img filter) must stay green.

- [ ] **Step 5: Workspace verify + commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
git add crates/umbral-core/src/templates.rs crates/umbral/src/lib.rs crates/umbral-core/tests/csrf_context.rs
git commit -m "feat(templates): ambient CSRF token in every render via CURRENT_CSRF task-local

csrf_token (raw) and csrf_input (a ready-to-drop-in hidden <input>)
merge into every template context when a middleware scopes the token.
Explicit ctx keys win, same precedence as the user merge.

Part 1/4 of docs/decisions/2026-06-10-automatic-csrf.md.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Security — middleware-only mint, rotation, append, signed default

**Files:**
- Modify: `plugins/umbral-security/src/lib.rs` (crate docs 34-63, `SecurityConfig::default` ~198, `CsrfState` ~365, `csrf_middleware` ~494-577, delete `ensure_csrf_cookie` ~630 + `response_sets_csrf_cookie` ~640, `tokens_match` → pub ~653)
- Test: `plugins/umbral-security/tests/csrf_flow.rs` (new; dev-deps tokio/tower/http-body-util already present)

- [ ] **Step 1: Write the failing integration tests**

Create `plugins/umbral-security/tests/csrf_flow.rs`:

```rust
//! End-to-end CSRF middleware flow against a real axum Router:
//! first-visit mint is visible to the handler (pre-handler mint),
//! Set-Cookie is appended (session cookie survives), the POST
//! re-render path has the token in scope, rotation replaces a
//! token that can't pass signed-mode validation.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Router, middleware};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral_security::test_support::csrf_layer_for_tests;

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn app(signed: bool, secret: Option<&str>) -> Router {
    let routes = Router::new()
        .route(
            "/form",
            get(|| async { umbral::templates::current_csrf().unwrap_or_default() }),
        )
        .route(
            "/form",
            post(|| async { umbral::templates::current_csrf().unwrap_or_default() }),
        )
        .route(
            "/with-session-cookie",
            get(|| async {
                ([(header::SET_COOKIE, "umbral_session=abc; Path=/")], "ok")
            }),
        );
    routes.layer(csrf_layer_for_tests(signed, secret.map(str::to_string)))
}

fn cookie_token(resp: &axum::response::Response) -> Option<String> {
    resp.headers().get_all(header::SET_COOKIE).iter().find_map(|v| {
        let s = v.to_str().ok()?;
        let rest = s.strip_prefix("umbral_csrf_token=")?;
        Some(rest.split(';').next().unwrap_or("").to_string())
    })
}

#[tokio::test]
async fn first_visit_handler_sees_the_minted_token() {
    let app = app(false, None);
    let resp = app
        .oneshot(Request::get("/form").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let minted = cookie_token(&resp).expect("first visit must set the csrf cookie");
    let seen = body_string(resp).await;
    assert_eq!(seen, minted, "handler-visible token must equal the cookie");
    assert!(!seen.is_empty());
}

#[tokio::test]
async fn set_cookie_is_appended_not_replaced() {
    let app = app(false, None);
    let resp = app
        .oneshot(Request::get("/with-session-cookie").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookies: Vec<String> = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(cookies.iter().any(|c| c.starts_with("umbral_session=")), "{cookies:?}");
    assert!(cookies.iter().any(|c| c.starts_with("umbral_csrf_token=")), "{cookies:?}");
}

#[tokio::test]
async fn valid_post_passes_and_has_token_in_scope_for_rerenders() {
    let app = app(false, None);
    let tok = "a".repeat(64);
    let resp = app
        .oneshot(
            Request::post("/form")
                .header(header::COOKIE, format!("umbral_csrf_token={tok}"))
                .header("x-csrf-token", tok.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, tok, "POST re-render must see the token");
}

#[tokio::test]
async fn mismatched_post_is_403() {
    let app = app(false, None);
    let resp = app
        .oneshot(
            Request::post("/form")
                .header(header::COOKIE, format!("umbral_csrf_token={}", "a".repeat(64)))
                .header("x-csrf-token", "b".repeat(64))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn stale_unsigned_cookie_rotates_under_signed_mode() {
    let app = app(true, Some("app-secret"));
    let resp = app
        .oneshot(
            Request::get("/form")
                .header(header::COOKIE, format!("umbral_csrf_token={}", "a".repeat(64)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let rotated = cookie_token(&resp).expect("unsigned cookie must be re-minted");
    assert!(rotated.contains('.'), "rotated token must be signed: {rotated}");
    let seen = body_string(resp).await;
    assert_eq!(seen, rotated, "handler must see the rotated token, not the stale one");
}

#[tokio::test]
async fn valid_signed_cookie_is_not_rotated() {
    let app = app(true, Some("app-secret"));
    // Mint via a first request, then replay the minted cookie.
    let first = app
        .clone()
        .oneshot(Request::get("/form").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let minted = cookie_token(&first).unwrap();
    let second = app
        .oneshot(
            Request::get("/form")
                .header(header::COOKIE, format!("umbral_csrf_token={minted}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(cookie_token(&second).is_none(), "valid signed cookie must not re-mint");
    assert_eq!(body_string(second).await, minted);
}
```

This needs a tiny test-only constructor since `CsrfState` is private. Add to `lib.rs` (bottom, before `#[cfg(test)]`):

```rust
/// Test-only constructors. Hidden from docs; NOT a stable API.
#[doc(hidden)]
pub mod test_support {
    use super::*;

    /// A CSRF middleware layer with an explicit state, bypassing
    /// settings resolution — integration tests have no `App::build()`.
    pub fn csrf_layer_for_tests(
        signed: bool,
        secret: Option<String>,
    ) -> axum::middleware::FromFnLayer<
        fn(
            State<CsrfState>,
            Request,
            Next,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Response, Infallible>> + Send>,
        >,
        CsrfState,
        (State<CsrfState>, Request),
    > {
        unimplemented!("executor: see note below")
    }
}
```

NOTE for the executor: the exact `FromFnLayer` type is unwieldy — the simpler shape is to return the layered router instead: `pub fn wrap_with_csrf(router: axum::Router, signed: bool, secret: Option<String>) -> axum::Router` that builds `CsrfState { secure: false, signed, secret, session_cookie: None, exempt_paths: vec![] }` and applies `middleware::from_fn_with_state(state, csrf_middleware)`. Adjust the test's `app()` helper to call `wrap_with_csrf(routes, signed, secret)`. Use whichever compiles cleanly; the test assertions are the contract.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates && cargo test -p umbral-security --test csrf_flow`
Expected: FAIL — `test_support` unimplemented / handler sees empty token (current middleware mints post-handler and never scopes).

- [ ] **Step 3: Rewrite the middleware**

In `plugins/umbral-security/src/lib.rs`:

(a) Add to `CsrfState`:

```rust
/// True when `token` may keep serving as this browser's CSRF cookie.
/// Plain mode accepts any non-empty token. Signed mode (with a
/// resolvable secret) requires a structurally valid `<raw>.<sig>` —
/// anything else (typically a cookie minted before `signed_csrf`
/// was enabled) triggers a rotation re-mint by the caller.
fn token_acceptable(&self, token: &str, session_value: Option<&str>) -> bool {
    if token.is_empty() {
        return false;
    }
    if !self.signed {
        return true;
    }
    let Some(secret) = self.secret.as_deref() else {
        return true; // signing requested but no secret resolved: degrade
    };
    let Some((raw, sig)) = token.rsplit_once('.') else {
        return false;
    };
    tokens_match(sig, &sign(secret, raw, self.session_bind(session_value)))
}
```

(b) Replace the body of `csrf_middleware` from the `if is_safe_method(&method)` block onward:

```rust
    if is_safe_method(&method) {
        // The middleware is the only mint (see docs/decisions/
        // 2026-06-10-automatic-csrf.md): mint BEFORE the handler runs so
        // first-visit renders already have a token, and rotate a token
        // that can't pass signed-mode validation so flipping
        // `signed_csrf` on doesn't 403 browsers holding old cookies.
        let (token, minted) = match cookie_token {
            Some(t) if state.token_acceptable(&t, session_value.as_deref()) => (t, false),
            _ => (mint_token(&state, session_value.as_deref()), true),
        };
        let mut response =
            umbral::templates::with_current_csrf(Some(token.clone()), next.run(req)).await;
        if minted {
            let mut cookie = format!("{CSRF_COOKIE}={token}; Path=/; SameSite=Lax");
            if state.secure {
                cookie.push_str("; Secure");
            }
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                // `append`, not `insert` — `insert` would wipe any cookie
                // the handler set on this response (e.g. the session).
                response.headers_mut().append(SET_COOKIE, v);
            }
        }
        return Ok(response);
    }

    // Write methods: cookie and (header OR form field) must validate.
    // On success the token is scoped around the handler so a
    // validation-error re-render still carries it into the form.
    let header_token = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);

    if let Some(c) = cookie_token.as_ref() {
        if let Some(h) = header_token.as_ref() {
            if csrf_valid(&state, c, h, session_value.as_deref()) {
                let token = c.clone();
                return Ok(
                    umbral::templates::with_current_csrf(Some(token), next.run(req)).await
                );
            }
        }
        let content_type = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type.starts_with("application/x-www-form-urlencoded") {
            let cookie_owned = c.clone();
            let (parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, MAX_FORM_BODY).await {
                Ok(b) => b,
                Err(_) => return Ok(forbidden()),
            };
            if let Some(s) = form_field_token(&bytes) {
                if csrf_valid(&state, &cookie_owned, &s, session_value.as_deref()) {
                    let req = Request::from_parts(parts, Body::from(bytes));
                    return Ok(umbral::templates::with_current_csrf(
                        Some(cookie_owned),
                        next.run(req),
                    )
                    .await);
                }
            }
        }
    }

    Ok(forbidden())
```

(c) Delete `ensure_csrf_cookie` (lines ~621-637) and `response_sets_csrf_cookie` (lines ~639-648) entirely. Keep `current_csrf_token` (read-only, used by admin fallback + JS docs) and `generate_token`.

(d) Make the comparison helper public:

```rust
/// Constant-time string equality. ... (keep the existing doc-comment)
pub fn tokens_match(a: &str, b: &str) -> bool {
```

(e) Flip the default: in `SecurityConfig::default()`, `signed_csrf: true,`. Update the field doc-comment and the crate-level docs (lines 49-63): signed CSRF is now the default; the middleware is the only mint when the plugin is mounted; stale unsigned cookies rotate automatically on the next safe request; set `signed_csrf: false` to opt back into plain double-submit. Delete the "Default off because the admin mints raw tokens" paragraph — it's no longer true after Task 3.

(f) Add the test-support module per Step 1's executor note.

- [ ] **Step 4: Run tests**

Run: `cd crates && cargo test -p umbral-security`
Expected: all unit tests + 6 integration tests pass. The existing `unsigned_mode_is_plain_double_submit` etc. are unaffected (they build `CsrfState` directly).

- [ ] **Step 5: Workspace verify + commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
git add plugins/umbral-security/
git commit -m "feat(security): middleware-only CSRF mint, ambient token, signed by default

csrf_middleware now mints BEFORE the handler on safe methods (first
visit covered), scopes the token via templates::with_current_csrf for
all non-exempt requests (POST error re-renders included), rotates
cookies that fail signed-mode validation, and appends its Set-Cookie
instead of clobbering handler cookies. ensure_csrf_cookie and the
handler-mint deference are deleted — no handler ever needs to mint.
signed_csrf defaults on; rotation makes the flip deploy-safe.

Part 2/4 of docs/decisions/2026-06-10-automatic-csrf.md. Closes gaps2 #26.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Admin — ambient-first token, constant-time compare, htmx header

**Files:**
- Modify: `plugins/umbral-admin/src/auth.rs` (`ensure_csrf_token` ~196, `login_post` csrf check ~142, doc-comments ~61-69 and ~123-128)
- Modify: `plugins/umbral-admin/templates/wrapper.html:493` (body tag)
- Modify: `plugins/umbral-admin/src/assets/admin.js` (fetch calls ~58, ~91)
- Test: `plugins/umbral-admin/tests/phase4_dashboard.rs` (append) + `#[cfg(test)]` in `auth.rs`

- [ ] **Step 1: Write the failing tests**

Append to `plugins/umbral-admin/tests/phase4_dashboard.rs` (match the file's existing read-the-template-source style, e.g. the `admin_js_served_as_external_asset_not_inline` test):

```rust
/// docs/decisions/2026-06-10-automatic-csrf.md: every htmx request the
/// admin makes must carry the ambient CSRF token. hx-headers on <body>
/// is inherited by all descendant hx-* requests.
#[test]
fn wrapper_body_carries_csrf_hx_headers() {
    let wrapper = include_str!("../templates/wrapper.html");
    let body_line = wrapper
        .lines()
        .find(|l| l.trim_start().starts_with("<body"))
        .expect("wrapper.html must have a <body> tag");
    assert!(body_line.contains("hx-headers"), "missing hx-headers: {body_line}");
    assert!(body_line.contains("X-CSRF-Token"), "missing X-CSRF-Token: {body_line}");
    assert!(body_line.contains("{{ csrf_token }}"), "must use the ambient token: {body_line}");
}

/// Raw fetch() calls in admin.js (the prefs writes) must send the token too.
#[test]
fn admin_js_fetches_send_csrf_header() {
    let js = include_str!("../src/assets/admin.js");
    assert!(js.contains("csrfHeaders"), "admin.js needs the csrfHeaders helper");
    let posts = js.matches("method: 'POST'").count() + js.matches("method: \"POST\"").count();
    let wired = js.matches("csrfHeaders()").count();
    assert!(wired > posts, "every POST fetch must spread csrfHeaders(): {wired} uses for {posts} POSTs");
}
```

(Executor: check how the prefs fetches declare their method — adjust the `posts` needle to the actual source shape; the intent is "each POSTing fetch spreads the helper, plus the one definition site".)

In `plugins/umbral-admin/src/auth.rs`, add at the bottom:

```rust
#[cfg(test)]
mod csrf_tests {
    use super::*;

    #[tokio::test]
    async fn ensure_csrf_token_prefers_the_ambient_token() {
        let headers = HeaderMap::new();
        let (tok, cookie) = umbral::templates::with_current_csrf(
            Some("ambient-token".to_string()),
            async { ensure_csrf_token(&headers) },
        )
        .await;
        assert_eq!(tok, "ambient-token");
        assert!(cookie.is_none(), "middleware owns the cookie; admin must not set one");
    }

    #[tokio::test]
    async fn ensure_csrf_token_self_mints_without_middleware() {
        let headers = HeaderMap::new();
        let (tok, cookie) = ensure_csrf_token(&headers);
        assert!(!tok.is_empty());
        assert!(cookie.is_some(), "no middleware, no cookie: admin must self-mint");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd crates && cargo test -p umbral-admin wrapper_body_carries_csrf_hx_headers admin_js_fetches_send_csrf_header ensure_csrf_token`
Expected: the two template/asset tests FAIL (attribute/helper absent); the ambient test FAILS (`ensure_csrf_token` reads only the cookie).

- [ ] **Step 3: Implement**

(a) `auth.rs` — `ensure_csrf_token` becomes ambient-first:

```rust
/// Resolve the CSRF token for an admin-rendered form.
///
/// 1. Ambient (`umbral::templates::current_csrf()`): `SecurityPlugin` is
///    mounted — its middleware minted the token and owns the cookie; the
///    admin sets nothing.
/// 2. Cookie fallback, then self-mint: no SecurityPlugin. The admin
///    stays self-protecting (login_post's own comparison is the
///    validator in this mode).
fn ensure_csrf_token(headers: &HeaderMap) -> (String, Option<String>) {
    if let Some(tok) = umbral::templates::current_csrf() {
        return (tok, None);
    }
    if let Some(tok) = umbral_security::current_csrf_token(headers) {
        return (tok, None);
    }
    let tok = umbral_security::generate_token();
    let cookie = format!("umbral_csrf_token={tok}; Path=/; SameSite=Lax");
    (tok, Some(cookie))
}
```

(b) `auth.rs` line ~142 — constant-time compare:

```rust
    let csrf_ok = !submitted_csrf.is_empty()
        && !cookie_csrf.is_empty()
        && umbral_security::tokens_match(submitted_csrf, &cookie_csrf);
```

(c) Update the `login_get` doc-comment (~61-69): the stale "the middleware mints one on the *next* GET" story is gone — with SecurityPlugin the token is ambient on the FIRST get; without it the admin self-mints immediately.

(d) `wrapper.html:493`:

```html
<body hx-headers='{"X-CSRF-Token": "{{ csrf_token }}"}' {% block body_attrs %}class="bg-background text-on-surface antialiased"{% endblock %}>
```

(e) `admin.js` — add near the top (after the `umbralAdminBase` usage area):

```js
  // Ambient CSRF: htmx requests inherit the header from <body hx-headers>;
  // raw fetch() calls read the (deliberately non-HttpOnly) cookie here.
  function csrfHeaders() {
    var m = document.cookie.match(/(?:^|;\s*)umbral_csrf_token=([^;]*)/);
    return m ? { 'X-CSRF-Token': decodeURIComponent(m[1]) } : {};
  }
```

and spread it into both `/api/prefs` fetches, e.g.:

```js
      fetch(umbralAdminBase + '/api/prefs', {
        method: 'POST',
        headers: Object.assign({ 'Content-Type': 'application/json' }, csrfHeaders()),
        body: JSON.stringify(payload),
      });
```

(Executor: read the two call sites at ~lines 58 and 91 and merge the existing options; keep their current body/credentials fields intact.)

- [ ] **Step 4: Run tests**

Run: `cd crates && cargo test -p umbral-admin`
Expected: new tests pass; the full admin suite (phase2/phase4, login flow) stays green.

- [ ] **Step 5: Workspace verify + commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
git add plugins/umbral-admin/
git commit -m "fix(admin): carry the ambient CSRF token on every htmx and fetch write

With SecurityPlugin mounted the admin's CRUD writes (sheet create/edit,
inline edit, delete, prefs fetches) carried NO token and 403'd —
only login.html had one (AUTH-2). <body hx-headers> now injects the
ambient {{ csrf_token }} into every htmx request and csrfHeaders()
covers the raw fetches. ensure_csrf_token prefers the middleware's
ambient token (single mint point; unblocked the signed_csrf default),
self-minting only when no SecurityPlugin is mounted. Login comparison
switches to constant-time tokens_match.

Part 3/4 of docs/decisions/2026-06-10-automatic-csrf.md.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Shop — delete every CSRF line from the views

**Files:**
- Modify: `examples/shop/src/views/public.rs` (`contact` ~165, `submit_contact` ~190, `render_contact_page` ~233, `render_contact_page_raw` ~252)
- Modify: `examples/shop/templates/contact.html:29`

- [ ] **Step 1: Strip the plumbing**

`contact` loses `headers`, the mint, and the Set-Cookie attach:

```rust
pub async fn contact(Query(query): Query<ContactQuery>) -> Result<Response, (StatusCode, String)> {
    let sent = query.sent.as_deref() == Some("1");
    // CSRF is fully ambient: SecurityPlugin's middleware minted the token
    // and templates::render injects {{ csrf_input }} into contact.html.
    render_contact_page(sent, &ContactMessage::default(), serde_json::Map::new(), StatusCode::OK)
}
```

`submit_contact` drops the `headers: HeaderMap` parameter and the `let csrf_token = ...` line; the error branch becomes:

```rust
        Err(errs) => {
            return render_contact_page_raw(
                false,
                errs.raw_as_json(),
                ctx_with_form_summary(&errs),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
```

Both render helpers drop the `csrf_token: &str` parameter and their `context!` calls become `context!(sent, form, errors)`. Remove the now-unused `HeaderMap` import (and `umbral::web::header::*` if nothing else uses it).

`contact.html:29`:

```html
        {{ csrf_input }}
```

- [ ] **Step 2: Build the shop**

Run: `cd examples/shop && cargo build`
Expected: clean build, no unused-import warnings. Do NOT run the server — the user runs it in dev mode.

- [ ] **Step 3: Commit**

```bash
git add examples/shop/src/views/public.rs examples/shop/templates/contact.html
git commit -m "refactor(shop): drop all manual CSRF plumbing from the contact views

The middleware mints + scopes the token and render injects
{{ csrf_input }}; the views' HeaderMap params, ensure_csrf_cookie
call, Set-Cookie attach, and csrf_token threading all disappear.
This is the zero-ceremony payoff the decision doc promised: zero
CSRF lines in view code, one token in the template.

Part 4/4 of docs/decisions/2026-06-10-automatic-csrf.md.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Docs page (none exists for SecurityPlugin)

**Files:**
- Create: `documentation/docs/v0.0.1/plugins/security.mdx`

- [ ] **Step 1: Write the page**

Minimal per CLAUDE.md (purpose, one example, spec link). Check `documentation/docs/v0.0.1/plugins/` sibling frontmatter for the `sidebar_position` to use (one past the highest). Content shape (don't hard-wrap prose; Specra components are global, no imports):

```mdx
---
title: Security
description: CSRF protection and hardening headers, on by default once mounted.
sidebar_position: <next free>
tags: [security, csrf, middleware]
---

# Security

`SecurityPlugin` gives every non-safe request automatic CSRF validation (signed double-submit) and a modern security-header bundle. Mount it and you're done - the middleware mints the token, your templates receive it ambiently, and a missing or forged token on any POST/PUT/PATCH/DELETE returns 403.

```rust
App::builder()
    .plugin(AuthPlugin::new())
    .plugin(SecurityPlugin::new())
    .build()
    .await?;
```

In an HTML form, emit the hidden input with `{{ csrf_input }}`:

```html
<form method="post" action="/contact">
  {{ csrf_input }}
  ...
</form>
```

For JavaScript / htmx writes, send the raw token as a header:

```html
<body hx-headers='{"X-CSRF-Token": "{{ csrf_token }}"}'>
```

<Callout type="info">
Token-authenticated APIs carry no session cookie — exempt them with `csrf_exempt_paths: vec!["/api".into()]` on `SecurityConfig`.
</Callout>

Design rationale: see `docs/decisions/2026-06-10-automatic-csrf.md` in the repo.
```

- [ ] **Step 2: Commit**

```bash
git add documentation/docs/v0.0.1/plugins/security.mdx
git commit -m "docs(plugins): SecurityPlugin page — automatic CSRF + header bundle

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Close gaps2 #26 (archive convention) + re-index

- [ ] **Step 1: Tracker update**

Per the archive convention (memory `feedback_gaps_archive_convention`): append the full shipped write-up for #26 to `bugs/archive/gaps2-done.md` under its number (commit hashes from Tasks 2-3, the rotation mechanism, test names), and replace the `bugs/gaps2.md` entry with:

```markdown
26. [x] Signed/session-bound CSRF (`SecurityConfig::signed_csrf`) is now the default — archived
```

- [ ] **Step 2: Final workspace verify + commit + re-index**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
cd ../examples/shop && cargo build
git add bugs/
git commit -m "docs(bugs): close gaps2 #26 — signed CSRF is the default

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
npx gitnexus analyze   # refresh the knowledge graph over the real code changes
# commit the AGENTS.md/CLAUDE.md count refresh if the analyzer dirtied them
```

---

## Self-review notes

- Spec coverage: decision items 1→Task 1, 2→Task 2, 3→Task 2(e), 4→Task 3(a,b), 5→Task 3(d,e), 6→Task 4; tests section mapped per task; doc page Task 5; tracker Task 6. No gaps.
- Type consistency: `with_current_csrf(Option<String>, F)` / `current_csrf() -> Option<String>` used identically in Tasks 1, 2, 3; `tokens_match(&str, &str) -> bool` made `pub` in Task 2 before the Task 3 use.
- Known executor-judgment points are explicitly marked (templates::init signature, test_support layer shape, admin.js call-site merge, sidebar_position) — each names the file to read and the contract to satisfy.
