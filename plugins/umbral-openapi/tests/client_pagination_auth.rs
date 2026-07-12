//! gaps3 #38 / Kikosi #1 — configurable pagination + adaptive auth in the
//! generated TypeScript client.
//!
//! Drives `client_gen::generate_with` directly with each `PaginationStyle`
//! (plus a custom paginator's declared `PaginationSchema`) and each kind of
//! OpenAPI security scheme, asserting the emitted client adapts:
//!
//! - the list envelope + query-builder methods match the paginator, and a
//!   custom paginator that declared its shape is emitted *typed* (not the
//!   opaque escape hatch);
//! - the `Authorization` prefix and the api-key header name come from the
//!   declared security scheme — a `Token`-prefixed scheme or an
//!   `x-umbral-api-key` header both come out right, nothing is hardcoded.
//!
//! `generate_with` is pure (it takes the config as parameters instead of
//! reading umbral-rest's process-global `OnceLock`s), so every case runs in one
//! binary with no `App::build`. `tests/client_pagination_auth_tsc.rs` proves the
//! output actually compiles and constrains a consumer.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use umbral::migrate::ModelMeta;
use umbral_rest::{PaginationField, PaginationScalar, PaginationSchema, PaginationStyle};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pa_post")]
pub struct PaPost {
    pub id: i64,
    pub title: String,
}

/// Generate the client for a single model with the given paginator + schemes.
fn gen_client(
    style: PaginationStyle,
    schema: Option<PaginationSchema>,
    schemes: &[(String, Value)],
) -> umbral_openapi::client_gen::GeneratedClient {
    umbral_openapi::client_gen::generate_with(
        &[ModelMeta::for_::<PaPost>()],
        "/api",
        style,
        schema,
        schemes,
    )
}

/// Types (envelope, builder signatures) live in the declaration file.
fn dts(style: PaginationStyle, schema: Option<PaginationSchema>) -> String {
    gen_client(style, schema, &[]).dts
}

/// Runtime (builder impls, auth header logic) lives in the JS module.
fn js_with(style: PaginationStyle, schemes: &[(String, Value)]) -> String {
    gen_client(style, None, schemes).js
}

#[track_caller]
fn assert_has(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}",
    );
}

#[track_caller]
fn assert_absent(haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "expected NOT to find:\n  {needle}\nin:\n{haystack}",
    );
}

// A cursor paginator's declared shape: a `next_cursor`/`prev_cursor` envelope
// (nullable), a `has_more` flag, and `?cursor=`/`?page_size=` params.
fn cursor_schema() -> PaginationSchema {
    PaginationSchema {
        envelope: vec![
            PaginationField::nullable("next_cursor", PaginationScalar::String),
            PaginationField::nullable("prev_cursor", PaginationScalar::String),
            PaginationField::new("has_more", PaginationScalar::Boolean),
        ],
        params: vec![
            PaginationField::new("cursor", PaginationScalar::String),
            PaginationField::new("page_size", PaginationScalar::Number),
        ],
    }
}

// ---- Pagination ---------------------------------------------------------

#[test]
fn page_number_envelope_and_builder_methods() {
    let d = dts(PaginationStyle::PageNumber, None);
    // Envelope carries the page-number metadata.
    for field in [
        "total_pages: number;",
        "current_page: number;",
        "page_size: number;",
        "next: number | null;",
    ] {
        assert_has(&d, field);
    }
    // Builder methods are declared...
    assert_has(&d, "page(v: number): this;");
    assert_has(&d, "pageSize(v: number): this;");
    // ...and implemented against the right wire params.
    let j = js_with(PaginationStyle::PageNumber, &[]);
    assert_has(
        &j,
        r#"page(v) { this.params.set("page", String(v)); return this; }"#,
    );
    assert_has(
        &j,
        r#"pageSize(v) { this.params.set("page_size", String(v)); return this; }"#,
    );
}

#[test]
fn limit_offset_envelope_and_builder_methods() {
    let d = dts(PaginationStyle::LimitOffset, None);
    assert_has(&d, "  limit: number;");
    assert_has(&d, "  offset: number;");
    assert_has(&d, "limit(v: number): this;");
    assert_has(&d, "offset(v: number): this;");

    let j = js_with(PaginationStyle::LimitOffset, &[]);
    assert_has(
        &j,
        r#"limit(v) { this.params.set("limit", String(v)); return this; }"#,
    );
    assert_has(
        &j,
        r#"offset(v) { this.params.set("offset", String(v)); return this; }"#,
    );
}

/// A custom paginator that DECLARED its shape gets a typed envelope + typed
/// builder methods — the headline of "typed via metadata".
#[test]
fn custom_with_schema_is_fully_typed() {
    let d = dts(PaginationStyle::Custom, Some(cursor_schema()));
    // Typed envelope, nullable honored.
    assert_has(&d, "  results: T[];");
    assert_has(&d, "  next_cursor: string | null;");
    assert_has(&d, "  prev_cursor: string | null;");
    assert_has(&d, "  has_more: boolean;");
    // The permissive index signature must NOT be there — the shape is known.
    assert_absent(&d, "[key: string]: unknown;");
    // One typed builder method per declared param (snake_case → camelCase).
    assert_has(&d, "cursor(v: string): this;");
    assert_has(&d, "pageSize(v: number): this;");

    // And the runtime sets the declared wire params.
    let c = gen_client(PaginationStyle::Custom, Some(cursor_schema()), &[]);
    assert_has(
        &c.js,
        r#"cursor(v) { this.params.set("cursor", String(v)); return this; }"#,
    );
    assert_has(
        &c.js,
        r#"pageSize(v) { this.params.set("page_size", String(v)); return this; }"#,
    );
}

/// A custom paginator that did NOT declare a shape gets an honest permissive
/// envelope + the generic `.param(...)` escape hatch — no invented fields.
#[test]
fn custom_without_schema_is_permissive() {
    let d = dts(PaginationStyle::Custom, None);
    assert_has(&d, "  results?: T[];");
    assert_has(&d, "  [key: string]: unknown;");
    // No page-number/limit-offset fields leaked in.
    assert_absent(&d, "total_pages");
    assert_absent(&d, "current_page");
}

/// The generic escape hatch is present for every style, declared and implemented.
#[test]
fn generic_param_escape_hatch_is_always_present() {
    for style in [
        PaginationStyle::None,
        PaginationStyle::PageNumber,
        PaginationStyle::Custom,
    ] {
        let c = gen_client(style, None, &[]);
        assert_has(
            &c.dts,
            "param(key: string, value: string | number | boolean): this;",
        );
        assert_has(&c.js, "param(key, value) {");
    }
}

// ---- Auth (derived from the declared security scheme) -------------------
//
// The header logic lives in the runtime, so these assert against `client.js`.

fn scheme(name: &str, value: Value) -> Vec<(String, Value)> {
    vec![(name.to_string(), value)]
}

/// A standard `http`/`bearer` scheme → `Authorization: Bearer <token>`.
#[test]
fn bearer_scheme_yields_bearer_prefix() {
    let j = js_with(
        PaginationStyle::None,
        &scheme("bearerAuth", json!({ "type": "http", "scheme": "bearer" })),
    );
    assert_has(&j, r#"this.opts.tokenPrefix ?? "Bearer""#);
}

/// The prefix is NOT hardcoded: an `http` scheme whose `scheme` is `token`
/// yields `Authorization: Token <token>`. This is the case the user called out.
#[test]
fn token_scheme_yields_token_prefix_not_bearer() {
    let j = js_with(
        PaginationStyle::None,
        &scheme("tokenAuth", json!({ "type": "http", "scheme": "token" })),
    );
    assert_has(&j, r#"this.opts.tokenPrefix ?? "Token""#);
    assert_absent(&j, r#"this.opts.tokenPrefix ?? "Bearer""#);
}

/// The api-key header is NOT hardcoded: the declared `name` is baked as the
/// default header — `x-umbral-api-key`, whatever the API chose.
#[test]
fn api_key_header_comes_from_the_scheme() {
    let j = js_with(
        PaginationStyle::None,
        &scheme(
            "apiKeyAuth",
            json!({ "type": "apiKey", "in": "header", "name": "X-Umbral-Api-Key" }),
        ),
    );
    assert_has(&j, r#"this.opts.apiKeyHeader ?? "X-Umbral-Api-Key""#);
    assert_absent(&j, r#"this.opts.apiKeyHeader ?? "X-API-Key""#);
}

/// A cookie (session) apiKey scheme flips fetch to send credentials by default.
#[test]
fn cookie_scheme_sends_credentials() {
    let j = js_with(
        PaginationStyle::None,
        &scheme(
            "sessionAuth",
            json!({ "type": "apiKey", "in": "cookie", "name": "sessionid" }),
        ),
    );
    assert_has(&j, r#"this.opts.credentials ?? "include""#);
}

/// With no scheme declared (the common case — auth undocumented), the client
/// still offers a sensible generic surface: Bearer token, X-API-Key, and the
/// dynamic hook. It sends no credentials by default.
#[test]
fn no_scheme_falls_back_to_generic_defaults() {
    let j = js_with(PaginationStyle::None, &[]);
    assert_has(&j, r#"this.opts.tokenPrefix ?? "Bearer""#);
    assert_has(&j, r#"this.opts.apiKeyHeader ?? "X-API-Key""#);
    assert_has(&j, "this.opts.credentials ?? undefined");
}

/// The dynamic hook is always available — for a rotating JWT / refresh flow /
/// request signing — and is merged last so it overrides the static options.
#[test]
fn dynamic_get_auth_headers_is_always_present() {
    let c = gen_client(PaginationStyle::None, None, &[]);
    assert_has(
        &c.dts,
        "getAuthHeaders?: () => Record<string, string> | Promise<Record<string, string>>;",
    );
    assert_has(
        &c.js,
        "Object.assign(headers, await this.opts.getAuthHeaders());",
    );
}
