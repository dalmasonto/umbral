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

/// Generate `client.ts` for a single model with the given paginator + schemes.
fn gen_client(
    style: PaginationStyle,
    schema: Option<PaginationSchema>,
    schemes: &[(String, Value)],
) -> String {
    let (_models, client) = umbral_openapi::client_gen::generate_with(
        &[ModelMeta::for_::<PaPost>()],
        "/api",
        style,
        schema,
        schemes,
    );
    client
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
    let c = gen_client(PaginationStyle::PageNumber, None, &[]);
    // Envelope carries the page-number metadata.
    for field in [
        "total_pages: number;",
        "current_page: number;",
        "page_size: number;",
        "next: number | null;",
    ] {
        assert_has(&c, field);
    }
    // Query builder gains page / pageSize.
    assert_has(&c, "page(n: number): this");
    assert_has(&c, "pageSize(n: number): this");
}

#[test]
fn limit_offset_envelope_and_builder_methods() {
    let c = gen_client(PaginationStyle::LimitOffset, None, &[]);
    assert_has(&c, "limit: number;");
    assert_has(&c, "offset: number;");
    assert_has(&c, "limit(n: number): this");
    assert_has(&c, "offset(n: number): this");
}

/// A custom paginator that DECLARED its shape gets a typed envelope + typed
/// builder methods — the headline of "typed via metadata".
#[test]
fn custom_with_schema_is_fully_typed() {
    let c = gen_client(PaginationStyle::Custom, Some(cursor_schema()), &[]);
    // Typed envelope, nullable honored.
    assert_has(&c, "  results: T[];");
    assert_has(&c, "  next_cursor: string | null;");
    assert_has(&c, "  prev_cursor: string | null;");
    assert_has(&c, "  has_more: boolean;");
    // The permissive index signature must NOT be there — the shape is known.
    assert_absent(&c, "[key: string]: unknown;");
    // One typed builder method per declared param (snake_case → camelCase).
    assert_has(&c, "cursor(v: string): this");
    assert_has(&c, "pageSize(v: number): this");
}

/// A custom paginator that did NOT declare a shape gets an honest permissive
/// envelope + the generic `.param(...)` escape hatch — no invented fields.
#[test]
fn custom_without_schema_is_permissive() {
    let c = gen_client(PaginationStyle::Custom, None, &[]);
    assert_has(&c, "  results?: T[];");
    assert_has(&c, "  [key: string]: unknown;");
    // No page-number/limit-offset fields leaked in.
    assert_absent(&c, "total_pages");
    assert_absent(&c, "current_page");
}

/// The generic escape hatch is present for every style, custom or not.
#[test]
fn generic_param_escape_hatch_is_always_present() {
    for style in [
        PaginationStyle::None,
        PaginationStyle::PageNumber,
        PaginationStyle::Custom,
    ] {
        let c = gen_client(style, None, &[]);
        assert_has(
            &c,
            "param(key: string, value: string | number | boolean): this",
        );
    }
}

// ---- Auth (derived from the declared security scheme) -------------------

fn scheme(name: &str, value: Value) -> Vec<(String, Value)> {
    vec![(name.to_string(), value)]
}

/// A standard `http`/`bearer` scheme → `Authorization: Bearer <token>`.
#[test]
fn bearer_scheme_yields_bearer_prefix() {
    let c = gen_client(
        PaginationStyle::None,
        None,
        &scheme("bearerAuth", json!({ "type": "http", "scheme": "bearer" })),
    );
    assert_has(&c, r#"this.opts.tokenPrefix ?? "Bearer""#);
}

/// The prefix is NOT hardcoded: an `http` scheme whose `scheme` is `token`
/// yields `Authorization: Token <token>`. This is the case the user called out.
#[test]
fn token_scheme_yields_token_prefix_not_bearer() {
    let c = gen_client(
        PaginationStyle::None,
        None,
        &scheme("tokenAuth", json!({ "type": "http", "scheme": "token" })),
    );
    assert_has(&c, r#"this.opts.tokenPrefix ?? "Token""#);
    assert_absent(&c, r#"this.opts.tokenPrefix ?? "Bearer""#);
}

/// The api-key header is NOT hardcoded: the declared `name` is baked as the
/// default header — `x-umbral-api-key`, whatever the API chose.
#[test]
fn api_key_header_comes_from_the_scheme() {
    let c = gen_client(
        PaginationStyle::None,
        None,
        &scheme(
            "apiKeyAuth",
            json!({ "type": "apiKey", "in": "header", "name": "X-Umbral-Api-Key" }),
        ),
    );
    assert_has(&c, r#"this.opts.apiKeyHeader ?? "X-Umbral-Api-Key""#);
    assert_absent(&c, r#"this.opts.apiKeyHeader ?? "X-API-Key""#);
}

/// A cookie (session) apiKey scheme flips fetch to send credentials by default.
#[test]
fn cookie_scheme_sends_credentials() {
    let c = gen_client(
        PaginationStyle::None,
        None,
        &scheme(
            "sessionAuth",
            json!({ "type": "apiKey", "in": "cookie", "name": "sessionid" }),
        ),
    );
    assert_has(&c, r#"this.opts.credentials ?? "include""#);
}

/// With no scheme declared (the common case — auth undocumented), the client
/// still offers a sensible generic surface: Bearer token, X-API-Key, and the
/// dynamic hook. It sends no credentials by default.
#[test]
fn no_scheme_falls_back_to_generic_defaults() {
    let c = gen_client(PaginationStyle::None, None, &[]);
    assert_has(&c, r#"this.opts.tokenPrefix ?? "Bearer""#);
    assert_has(&c, r#"this.opts.apiKeyHeader ?? "X-API-Key""#);
    assert_has(&c, "this.opts.credentials ?? undefined");
}

/// The dynamic hook is always available — for a rotating JWT / refresh flow /
/// request signing — and is merged last so it overrides the static options.
#[test]
fn dynamic_get_auth_headers_is_always_present() {
    let c = gen_client(PaginationStyle::None, None, &[]);
    assert_has(
        &c,
        "getAuthHeaders?: () => Record<string, string> | Promise<Record<string, string>>;",
    );
    assert_has(
        &c,
        "Object.assign(headers, await this.opts.getAuthHeaders());",
    );
}
