//! Integration coverage for `#[derive(Form)]`. The macro emits an
//! `impl Form` against the primitives in `umbra::forms` (Field,
//! ValidationErrors, validators). Each test pins a different
//! lowering shape: per-attr validators, Option<T> -> optional,
//! type dispatch over String / i64 / f64 / bool, the email and
//! password attribute hooks, and the default render_html walk.
//!
//! Pure compile-time + sync — no DB, no async runtime needed.

#![allow(dead_code)]

use std::collections::HashMap;

// `Form` is now the axum extractor (gaps2 #19). The `validate()`
// method comes from the `FormValidate` trait the derive emits an
// impl of. Imports keep the test surface short.
use umbra::forms::{FormValidate, ValidationErrors};

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// --------------------------------------------------------------------- //
// 1. Minimum-viable form: String field, no attributes. The derive       //
// picks Field::text and Required-by-default, so an empty input fails.   //
// --------------------------------------------------------------------- //

#[derive(Debug, umbra::forms::Form)]
struct MinimalForm {
    title: String,
}

#[tokio::test]
async fn minimal_string_form_round_trips_a_valid_input() {
    let form = MinimalForm::validate(&data(&[("title", "hello")]))
        .await
        .expect("should validate");
    assert_eq!(form.title, "hello");
}

#[tokio::test]
async fn minimal_string_form_rejects_empty_input() {
    let err = MinimalForm::validate(&data(&[("title", "")]))
        .await
        .expect_err("empty fails");
    assert!(err.fields.contains_key("title"));
    assert!(err.fields["title"][0].contains("required"));
}

// --------------------------------------------------------------------- //
// 2. The full attr set: min_length, max_length, email, password,        //
// optional. The macro lowers each to the matching Field builder method. //
// --------------------------------------------------------------------- //

#[derive(Debug, umbra::forms::Form)]
struct SignupForm {
    #[form(min_length = 3, max_length = 150)]
    username: String,

    #[form(email)]
    email: String,

    #[form(password, min_length = 8)]
    password: String,

    #[form(optional, max_length = 280)]
    bio: Option<String>,

    is_admin: bool,
}

#[tokio::test]
async fn signup_form_happy_path_returns_the_typed_struct() {
    let form = SignupForm::validate(&data(&[
        ("username", "alice"),
        ("email", "alice@example.com"),
        ("password", "hunter2-stronger"),
        ("bio", "loves rust"),
        ("is_admin", "true"),
    ]))
    .await
    .expect("happy path");
    assert_eq!(form.username, "alice");
    assert_eq!(form.email, "alice@example.com");
    assert_eq!(form.password, "hunter2-stronger");
    assert_eq!(form.bio.as_deref(), Some("loves rust"));
    assert!(form.is_admin);
}

#[tokio::test]
async fn signup_form_collects_every_field_error_at_once() {
    let err = SignupForm::validate(&data(&[
        ("username", "ab"),
        ("email", "not-an-email"),
        ("password", "short"),
        ("bio", ""),
        ("is_admin", ""),
    ]))
    .await
    .expect_err("multi-field failure");
    assert!(err.fields.contains_key("username"), "username missing");
    assert!(err.fields.contains_key("email"), "email missing");
    assert!(err.fields.contains_key("password"), "password missing");
    assert!(
        !err.fields.contains_key("bio"),
        "optional empty bio shouldn't error"
    );
    assert!(
        !err.fields.contains_key("is_admin"),
        "boolean missing-key is valid (form omits unchecked boxes)"
    );

    assert!(err.fields["username"][0].contains("at least 3"));
    assert!(
        err.fields["email"][0].contains("@") || err.fields["email"][0].contains("`@`"),
        "email diagnostic should mention @ symbol: {:?}",
        err.fields["email"]
    );
    assert!(err.fields["password"][0].contains("at least 8"));
}

#[tokio::test]
async fn signup_form_optional_bio_handles_both_some_and_none() {
    let with_bio = SignupForm::validate(&data(&[
        ("username", "alice"),
        ("email", "alice@example.com"),
        ("password", "hunter2-stronger"),
        ("bio", "wrote a book"),
        ("is_admin", "false"),
    ]))
    .await
    .expect("with bio");
    assert_eq!(with_bio.bio.as_deref(), Some("wrote a book"));

    let without_bio = SignupForm::validate(&data(&[
        ("username", "bob"),
        ("email", "bob@example.com"),
        ("password", "hunter2-stronger"),
    ]))
    .await
    .expect("without bio");
    assert_eq!(without_bio.bio, None);
}

#[tokio::test]
async fn signup_form_checkbox_is_false_when_key_is_absent() {
    let form = SignupForm::validate(&data(&[
        ("username", "alice"),
        ("email", "alice@example.com"),
        ("password", "hunter2-stronger"),
    ]))
    .await
    .expect("happy path with no is_admin");
    assert!(
        !form.is_admin,
        "an unchecked HTML checkbox sends no key; the form should default to false"
    );
}

// --------------------------------------------------------------------- //
// 3. Numeric types. The derive dispatches i64 -> Field::integer and     //
// emits parse::<i64>() for the value path. Validation fails when the    //
// input doesn't parse, and the error message names the field.          //
// --------------------------------------------------------------------- //

#[derive(Debug, umbra::forms::Form)]
struct ProductForm {
    name: String,

    price_cents: i64,

    weight_kg: f64,

    #[form(optional)]
    stock_count: Option<i64>,
}

#[tokio::test]
async fn numeric_form_parses_integers_and_floats() {
    let form = ProductForm::validate(&data(&[
        ("name", "widget"),
        ("price_cents", "1299"),
        ("weight_kg", "0.42"),
        ("stock_count", "100"),
    ]))
    .await
    .expect("happy");
    assert_eq!(form.price_cents, 1299);
    assert!((form.weight_kg - 0.42).abs() < 1e-9);
    assert_eq!(form.stock_count, Some(100));
}

#[tokio::test]
async fn numeric_form_rejects_non_numeric_input() {
    let err = ProductForm::validate(&data(&[
        ("name", "widget"),
        ("price_cents", "free"),
        ("weight_kg", "light"),
        ("stock_count", ""),
    ]))
    .await
    .expect_err("two parse failures");
    assert!(
        err.fields["price_cents"][0].contains("whole number"),
        "integer parse error: {:?}",
        err.fields["price_cents"]
    );
    assert!(
        err.fields["weight_kg"][0].contains("number"),
        "float parse error: {:?}",
        err.fields["weight_kg"]
    );
    assert_eq!(form_field_count(&err), 2);
}

#[tokio::test]
async fn numeric_form_optional_int_with_empty_input_is_none() {
    let form = ProductForm::validate(&data(&[
        ("name", "widget"),
        ("price_cents", "100"),
        ("weight_kg", "1.0"),
    ]))
    .await
    .expect("happy without stock");
    assert_eq!(form.stock_count, None);
}

// --------------------------------------------------------------------- //
// 4. fields() + render_html(). The macro emits a fields() that returns //
// one Field per struct field; the trait's default render_html walks it. //
// --------------------------------------------------------------------- //

#[test]
fn fields_returns_one_entry_per_struct_field_in_order() {
    let fields = SignupForm::fields();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["username", "email", "password", "bio", "is_admin"]
    );
}

#[tokio::test]
async fn render_html_emits_one_input_per_field_with_correct_types() {
    let prefill = data(&[("username", "alice")]);
    let html = SignupForm::render_html(&prefill).await;

    assert!(html.contains("name=\"username\""), "username field missing");
    assert!(html.contains("name=\"email\""), "email field missing");
    assert!(html.contains("name=\"password\""), "password field missing");
    assert!(html.contains("name=\"bio\""), "bio field missing");
    assert!(html.contains("name=\"is_admin\""), "is_admin field missing");

    // Per-attr input type dispatch.
    assert!(
        html.contains("type=\"email\""),
        "email should render type=email"
    );
    assert!(
        html.contains("type=\"password\""),
        "password should render type=password"
    );
    assert!(
        html.contains("type=\"checkbox\""),
        "boolean should render type=checkbox"
    );

    // Prefill round-trips.
    assert!(html.contains("value=\"alice\""), "prefill missing: {html}");
}

#[tokio::test]
async fn render_html_escapes_xss_in_prefill_values() {
    // MinimalForm's field is `title`, so the prefill key has to
    // match. An XSS payload in the prefill value must round-trip
    // through `html_escape` and emerge as `&lt;script&gt;`.
    let prefill = data(&[("title", "<script>alert(1)</script>")]);
    let html = MinimalForm::render_html(&prefill).await;
    assert!(!html.contains("<script>alert"), "raw XSS leaked: {html}");
    assert!(html.contains("&lt;script&gt;"), "escape missing: {html}");
}

fn form_field_count(err: &ValidationErrors) -> usize {
    err.fields.len()
}

// --------------------------------------------------------------------- //
// Task 1 — FormValidate is async. The minimal form must be awaitable.   //
// --------------------------------------------------------------------- //

#[tokio::test]
async fn async_validate_minimal_form_round_trips() {
    let form = MinimalForm::validate(&data(&[("title", "hello")]))
        .await
        .expect("should validate");
    assert_eq!(form.title, "hello");
}
