//! Kikosi #1/#2 — the generated session client (`client.auth`).
//!
//! The client's auth surface is discovered from the paths every plugin publishes
//! via `Plugin::openapi_paths()`, keyed on `operationId` (`auth_login`,
//! `auth_logout`, `auth_me`, `auth_register`) rather than on path spelling. This
//! drives the discovery with **umbral-auth's real contribution**, so the emitted
//! types track the actual published request/response schemas — not a copy of them
//! that can drift.
//!
//! Two properties matter most:
//!
//! - an app WITHOUT an auth plugin generates no session client at all (no dead
//!   code, no methods that would 404);
//! - an auth plugin mounted at a custom prefix still generates a working client,
//!   because the path comes from the published document.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use umbral::migrate::ModelMeta;
use umbral::plugin::Plugin;
use umbral_openapi::client_gen::GeneratedClient;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ca_post")]
pub struct CaPost {
    pub id: i64,
    pub title: String,
}

/// The auth paths umbral-auth really publishes.
fn auth_paths() -> Vec<(String, Value)> {
    umbral_auth::AuthPlugin::<umbral_auth::AuthUser>::default()
        .with_default_routes()
        .openapi_paths()
}

fn gen_client(paths: &[(String, Value)]) -> GeneratedClient {
    umbral_openapi::client_gen::generate_with(
        &[ModelMeta::for_::<CaPost>()],
        "/api",
        umbral_rest::PaginationStyle::None,
        None,
        &[],
        paths,
    )
}

#[track_caller]
fn assert_has(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}",
    );
}

/// An app with no auth plugin gets NO session client — not an `auth` object
/// whose methods would 404 against endpoints that don't exist.
#[test]
fn no_auth_plugin_emits_no_session_client() {
    let c = gen_client(&[]);
    for absent in ["AuthClient", "readonly auth:", "LoginCredentials"] {
        assert!(
            !c.js.contains(absent) && !c.dts.contains(absent),
            "a REST-only app must not carry a session client (`{absent}`); got:\n{}\n{}",
            c.js,
            c.dts,
        );
    }
}

/// With umbral-auth mounted, the session client appears — typed from the schemas
/// the plugin actually publishes.
#[test]
fn auth_plugin_generates_a_typed_session_client() {
    let c = gen_client(&auth_paths());

    // Types come from the published schemas. `required` on the user response
    // means these are NOT optional — `user.username` is `string`, not
    // `string | undefined`.
    assert_has(&c.dts, "export type AuthUser = {");
    assert_has(&c.dts, "username: string;");
    assert_has(&c.dts, "is_staff: boolean;");
    assert_has(&c.dts, "id: number;");
    assert!(
        !c.dts.contains("username?: string;"),
        "the user response declares `required`, so its fields must not be optional; got:\n{}",
        c.dts,
    );

    // Credentials come from the login request-body schema.
    assert_has(&c.dts, "export type LoginCredentials = {");
    assert_has(&c.dts, "password: string;");

    assert_has(&c.dts, "export interface LoginResult {");
    assert_has(&c.dts, "export declare class AuthClient {");
    assert_has(
        &c.dts,
        "login(credentials: LoginCredentials): Promise<LoginResult>;",
    );
    assert_has(&c.dts, "me(): Promise<AuthUser | null>;");
    assert_has(&c.dts, "logout(): Promise<void>;");
    // Reachable from the client.
    assert_has(&c.dts, "readonly auth: AuthClient;");

    // And the runtime implements it against the REAL published paths.
    assert_has(&c.js, "export class AuthClient {");
    assert_has(&c.js, "this.auth = new AuthClient(this);");
    assert_has(
        &c.js,
        r#"this.client._request("POST", "/api/auth/login", credentials)"#,
    );
    assert_has(&c.js, r#"this.client._request("GET", "/api/auth/me")"#);
}

/// Signing in must be enough: `login` stores the token and every later request
/// picks it up, so the app never threads a token through call sites by hand.
#[test]
fn login_stores_the_token_and_requests_pick_it_up() {
    let c = gen_client(&auth_paths());
    // login adopts the token...
    assert_has(
        &c.js,
        "this.client._setToken(out && out.token ? out.token : null);",
    );
    // ...the request path reads that live token (not the constructor option)...
    assert_has(&c.js, "if (this._token) {");
    assert_has(&c.js, "${this._token}");
    // ...and logout drops it even if the server call fails.
    assert_has(&c.js, "finally { this.client._setToken(null); }");
}

/// A 401 from `/me` is the ANSWER to "am I logged in?", not a failure — it must
/// resolve `null`, not throw. Anything else still throws.
#[test]
fn me_returns_null_when_signed_out_rather_than_throwing() {
    let c = gen_client(&auth_paths());
    assert_has(
        &c.js,
        "if (err instanceof UmbralError && err.status === 401) return null;",
    );
    assert_has(&c.js, "throw err;");
}

/// The token is never persisted to localStorage by the generated client (XSS
/// reads it); the app is handed `onToken` and makes that call itself.
#[test]
fn the_client_never_writes_the_token_to_local_storage() {
    let c = gen_client(&auth_paths());
    // The guarantee is that it never WRITES web storage. (The comment explaining
    // why is allowed to name it — assert on the call, not the word.)
    assert!(
        !c.js.contains("localStorage.setItem") && !c.js.contains("sessionStorage.setItem"),
        "the client must not persist the token to web storage itself; got:\n{}",
        c.js,
    );
    assert_has(&c.js, "if (this.opts.onToken) this.opts.onToken(token);");
    assert_has(&c.dts, "onToken?: (token: string | null) => void;");
}

/// Discovery is by `operationId`, not path spelling — so an auth plugin remounted
/// at a custom prefix still yields a working client pointed at the real URLs.
#[test]
fn a_remounted_auth_prefix_is_honoured() {
    let paths = vec![
        (
            "/accounts/sign-in".to_string(),
            json!({ "post": {
                "operationId": "auth_login",
                "requestBody": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["email", "password"],
                    "properties": {"email": {"type": "string"}, "password": {"type": "string"}}
                }}}},
                "responses": {"200": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["user", "token"],
                    "properties": {
                        "user": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}},
                        "token": {"type": "string"}
                    }
                }}}}}
            }}),
        ),
        (
            "/accounts/whoami".to_string(),
            json!({ "get": { "operationId": "auth_me" } }),
        ),
    ];
    let c = gen_client(&paths);
    assert_has(
        &c.js,
        r#"this.client._request("POST", "/accounts/sign-in", credentials)"#,
    );
    assert_has(&c.js, r#"this.client._request("GET", "/accounts/whoami")"#);
    // And the credential type follows THAT app's schema (email, not username).
    assert_has(
        &c.dts,
        "export type LoginCredentials = { email: string; password: string; };",
    );
    // No register endpoint was published → no register method invented.
    assert!(
        !c.dts.contains("register("),
        "must not invent a register endpoint the app doesn't serve; got:\n{}",
        c.dts,
    );
}
