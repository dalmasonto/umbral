//! gaps2 #19 — `Form<T>` axum extractor + structured `FormErrors`.
//!
//! Three contracts pinned:
//! 1. Happy path: a valid form body extracts to `Ok(T)` carrying
//!    the parsed struct.
//! 2. Validation failure path: an invalid body extracts to
//!    `Err(FormErrors)` carrying the per-field errors. The HTTP
//!    layer never rejects — handlers ALWAYS see a `Form<T>` and
//!    branch.
//! 3. `#[form(normalize_strings)]` auto-trims every String field
//!    before validation runs.

#![allow(dead_code)]

use axum::body::Body;
use axum::extract::FromRequest;
use axum::http::{Request, header};
use serde::{Deserialize, Serialize};
use umbral::forms::Form;

#[derive(Debug, Serialize, Deserialize, Default, umbral::forms::Form)]
#[form(normalize_strings)]
struct ContactSpec {
    #[form(required, min_length = 1, max_length = 100)]
    name: String,

    #[form(required, email)]
    email: String,

    #[form(max_length = 30, optional)]
    phone: String,

    #[form(required, length(min = 10, max = 5000))]
    message: String,
}

async fn extract(body: &str) -> Form<ContactSpec> {
    let req = Request::builder()
        .method("POST")
        .uri("/")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_owned()))
        .unwrap();
    <Form<ContactSpec> as FromRequest<()>>::from_request(req, &())
        .await
        .expect("extractor never rejects — always returns Form<T>")
}

#[tokio::test]
async fn happy_path_returns_validated_struct() {
    let body = "name=Alice&email=alice%40example.com&phone=&message=Hello+there+I+have+a+question";
    let form = extract(body).await;
    let valid = form.into_result().expect("body is valid — Ok(T) expected");
    assert_eq!(valid.name, "Alice");
    assert_eq!(valid.email, "alice@example.com");
    assert_eq!(valid.phone, ""); // optional + empty = empty string
    assert!(valid.message.starts_with("Hello"));
}

#[tokio::test]
async fn missing_required_fields_surface_per_field_errors() {
    // Empty body: every required field missing.
    let form = extract("").await;
    let errs = form
        .into_result()
        .expect_err("validation should fail on empty body");

    let field_errors = errs.field_errors();
    // `name`, `email`, `message` are all required. `phone` is
    // optional so its absence does NOT produce an entry.
    assert!(field_errors.contains_key("name"));
    assert!(field_errors.contains_key("email"));
    assert!(field_errors.contains_key("message"));
    assert!(
        !field_errors.contains_key("phone"),
        "optional empty field must not produce a per-field error"
    );

    // Each entry must carry at least one human-readable message.
    for (field, msgs) in &field_errors {
        assert!(
            !msgs.is_empty(),
            "field `{field}` has an empty messages vec"
        );
        assert!(
            msgs[0].len() > 3,
            "field `{field}`'s first message is suspiciously short"
        );
    }
}

#[tokio::test]
async fn invalid_email_surfaces_under_email_key() {
    let body = "name=Alice&email=not-an-email&message=Hello+there+I+have+a+question";
    let form = extract(body).await;
    let errs = form.into_result().expect_err("invalid email should fail");
    let field_errors = errs.field_errors();
    assert!(
        field_errors.contains_key("email"),
        "email-format error must land under the `email` key"
    );
}

#[tokio::test]
async fn normalize_strings_trims_whitespace_before_validation() {
    // `name` arrives with leading + trailing spaces. Without
    // normalize_strings the validator would accept the padded
    // form and the struct would carry the spaces verbatim. With
    // normalize_strings on (set at the container level), the
    // raw value is trimmed before validation AND ends up trimmed
    // in the parsed struct.
    let body =
        "name=++++Alice++&email=alice%40example.com&message=Hello+there+yes+I+have+a+question";
    let form = extract(body).await;
    let valid = form
        .into_result()
        .expect("after trim, name = `Alice` is non-empty and length-valid");
    assert_eq!(
        valid.name, "Alice",
        "normalize_strings must strip leading/trailing whitespace"
    );
}

#[tokio::test]
async fn template_ctx_renders_first_error_per_field_under_flat_key() {
    let form = extract("").await;
    let errs = form.into_result().expect_err("validation should fail");

    // `as_template_ctx` is the flat shape templates expect:
    // `errors.name`, `errors.email`, etc. — each maps to the
    // first error string for that field.
    let ctx = errs.as_template_ctx();
    assert!(ctx.get("name").and_then(|v| v.as_str()).is_some());
    assert!(ctx.get("email").and_then(|v| v.as_str()).is_some());
    assert!(ctx.get("message").and_then(|v| v.as_str()).is_some());
    // `phone` was optional + absent so it shouldn't surface.
    assert!(
        ctx.get("phone").is_none(),
        "optional field with no input must not appear in the flat ctx"
    );
}
