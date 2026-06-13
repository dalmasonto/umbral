# OAuth SPA login (token return) + endpoint discovery

Date: 2026-06-13
Status: approved, implementing

## Problem

`umbra-oauth` today is a browser/session flow: the callback ends in `login_user_id()` (a session cookie) + a 302. That works for a server-rendered app, and — because `umbra-rest`'s auth chain includes `SessionAuthentication` — it already works for a *same-origin* SPA that sends the cookie. Two gaps remain:

1. **A separate-origin SPA (Vite/Next) can't obtain a bearer token.** It wants `Authorization: Bearer <token>`, and the flow never hands one back.
2. **The OAuth URLs aren't discoverable.** A client has to hardcode `/oauth/google/login`; there's no machine-readable list of what's configured, and no API-root index that advertises plugin endpoints.

## Scope

In scope: (A) token return on login completion for SPAs, (B) an OAuth discovery endpoint, (C) a generic `Plugin` seam + a REST API root that aggregates plugin endpoints. Out of scope: SPA-driven PKCE (`POST /exchange`), refresh-token rotation — both can come later on the same seam.

## A. Token return (SPA login)

Pattern: **redirect-with-token**, reusing `umbra_auth::AuthToken::create_for`.

Flow:
1. SPA navigates the browser to `GET /oauth/{provider}/login?next=<spa_url>`.
2. The login handler validates `next` against an **allowlist** and stores it in the in-session `FlowState` (`return_to`).
3. Provider → `…/callback` → exchange + `resolve_user` (unchanged).
4. **Login flow with `return_to`:** mint `AuthToken::create_for(&user, "oauth")`, redirect to `{return_to}#token=<plaintext>&token_type=Bearer`. The token rides in the URL **fragment** — never sent to a server, never logged.
5. SPA reads `window.location.hash`, stores the token, clears the hash, and authenticates subsequent REST calls with `Authorization: Bearer`.

Security:
- **Open-redirect / token-theft defense:** `next` MUST prefix-match one of `OAuthPlugin.allowed_returns` (configured via `.allow_return(prefix)`). A non-matching `next` is rejected with `400` — no silent fallback. With no allowlist configured, `?next=` is ignored entirely and the flow keeps its existing session behavior (safe by default).
- Token mode is **login-only.** A connect flow (`connect_user = Some`) with `return_to` redirects back *without* minting a token (the user already holds one).
- The session cookie is still set on the API origin for the login flow; it's harmless and unused by a cross-origin SPA.

`FlowState` gains `return_to: Option<String>` (`#[serde(default)]` for in-flight flows). The callback loads the `AuthUser` by id (`resolve_user` returns the id) to satisfy `create_for(&AuthUser, …)`.

## B. Discovery endpoint (umbra-oauth, zero coupling)

`GET /oauth/providers` (public, GET, no CSRF) → JSON auto-built from the registered providers + `redirect_base`:

```json
{ "providers": [
  { "key": "google", "label": "Google",
    "login":    { "path": "/oauth/google/login",    "url": "https://api.example.com/oauth/google/login" },
    "connect":  { "path": "/oauth/google/connect",  "url": "https://api.example.com/oauth/google/connect" },
    "callback": { "path": "/oauth/google/callback", "url": "https://api.example.com/oauth/google/callback" } } ] }
```

`path` relative; `url` absolute, joined from `redirect_base` (the authoritative public origin the callbacks already use). One **shared descriptor builder** feeds both this endpoint and `api_endpoints()` (C) so they can't drift.

## C. Generic Plugin seam + REST API root

- **Core (`umbra-core`):** new `ApiEndpoint { group, name, method, path, label }` (origin-agnostic — relative `path` only), re-exported from the facade. New default trait method `Plugin::api_endpoints(&self) -> Vec<ApiEndpoint> { Vec::new() }`.
- **App build:** `App::build()` already walks `sorted_plugins`; collect each `plugin.api_endpoints()` into a core `OnceLock<Vec<ApiEndpoint>>`, exposed via `pub fn registered_api_endpoints()`. (`registered_plugins()` returns only names, so a live-object iteration isn't available at request time — this global is the seam, mirroring the existing model/alias registries.)
- **umbra-oauth:** implements `api_endpoints()` from the shared descriptor builder.
- **umbra-rest:** a root handler at `GET {base}/` returns `{ resources, endpoints }` — `resources` = its own registered models, `endpoints` = `registered_api_endpoints()`, each annotated with an absolute `url` joined from the incoming request origin.

This is the dependency-inversion contract: OAuth *declares* endpoints, REST *discovers* them through the trait/global — neither names the other (Cargo's no-circular-dep rule still holds).

## Testing (behavioral, real requests)

- oauth: `GET /oauth/providers` lists a registered fake provider with correct `path` + `url`.
- oauth: login `?next=<allowed>` → callback → 302 `Location` carries `#token=`; the token resolves via `AuthToken::lookup`.
- oauth: login `?next=<not-allowed>` → `400`.
- rest: `GET /api/` lists `resources` and the aggregated `oauth` `endpoints`; a plugin with no `api_endpoints()` contributes nothing.

## Docs

`oauth.mdx`: a "Discovery" section (`/oauth/providers`) and an "SPA login (token)" section (the `next` + fragment flow, the `.allow_return` allowlist, the security note). Short note on the REST page about `GET /api/`.
