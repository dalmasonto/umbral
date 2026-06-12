//! Behavioral coverage for `FileField` / `ImageField` form fields. A
//! model that derives both `Model` and `Form` with a file/image column
//! (e.g. the website's `plugin_directory::Plugin` with `logo:
//! Option<ImageField>`) must compile WITHOUT `#[umbra(noform)]` — the
//! Form derive classifies these into a `Field::file` (an
//! `<input type="file">`), and validate() constructs the typed newtype
//! from the submitted storage-key string the admin's multipart handler
//! already stored in the form data.
//!
//! Pure compile-time + sync — no DB, no async runtime needed beyond the
//! async `validate`. File-field validation never touches the DB (the key
//! is opaque), so no boot is required.

#![allow(dead_code)]

use std::collections::HashMap;

use umbra::forms::{FormValidate, InputKind};

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// A model that derives BOTH Model and Form with file/image fields and
// NO `#[umbra(noform)]`. The whole point of the fix: this must compile.
#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbra::orm::Model,
    umbra::forms::Form,
)]
#[umbra(table = "fff_listing")]
pub struct Listing {
    #[umbra(primary_key)]
    pub id: i64,
    pub attachment: umbra::orm::FileField,
    pub avatar: Option<umbra::orm::ImageField>,
}

#[tokio::test]
async fn required_file_with_optional_image_round_trips() {
    let form = Listing::validate(&data(&[("attachment", "k1.png")]))
        .await
        .expect("required file present, optional image absent");
    assert_eq!(form.attachment.key(), "k1.png");
    assert_eq!(form.avatar, None);
}

#[tokio::test]
async fn optional_image_present_constructs_some() {
    let form = Listing::validate(&data(&[("avatar", "a.png"), ("attachment", "k1.png")]))
        .await
        .expect("both present");
    assert_eq!(form.attachment.key(), "k1.png");
    assert_eq!(form.avatar, Some(umbra::orm::ImageField::from("a.png")));
}

#[tokio::test]
async fn missing_required_file_is_a_validation_error() {
    let err = Listing::validate(&data(&[("avatar", "a.png")]))
        .await
        .expect_err("missing required attachment fails");
    assert!(
        err.fields.contains_key("attachment"),
        "attachment should be a required-field error: {err:?}"
    );
    assert!(err.fields["attachment"][0].contains("required"));
}

#[test]
fn file_field_renders_as_file_input_kind() {
    let fields = Listing::fields();
    let attachment = fields
        .iter()
        .find(|f| f.name == "attachment")
        .expect("attachment field present");
    assert!(matches!(attachment.kind, InputKind::File));
    assert!(attachment.required, "non-Option file field is required");

    let avatar = fields
        .iter()
        .find(|f| f.name == "avatar")
        .expect("avatar field present");
    assert!(matches!(avatar.kind, InputKind::File));
    assert!(!avatar.required, "Option<ImageField> is optional");
}

#[tokio::test]
async fn file_input_does_not_echo_the_key_as_value() {
    // A file input must not be pre-filled with the key as its `value`
    // attribute (browsers reject programmatic file-input values, and it
    // would leak the storage key into the markup).
    let prefill = data(&[("attachment", "secret-key.png")]);
    let html = Listing::render_html(&prefill).await;
    assert!(html.contains("type=\"file\""), "file input missing: {html}");
    assert!(
        !html.contains("secret-key.png"),
        "file input must not echo the key as value: {html}"
    );
}
