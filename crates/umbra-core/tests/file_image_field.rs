//! Wave 2 — `FileField` / `ImageField`:
//!
//! - macro classification: `FieldSpec.ty == Text`, the type-derived
//!   default `widget` (`"file"` / `"image"`), nullable propagation for
//!   `Option<FileField>`, and explicit `#[umbra(widget)]` override.
//! - `FileField` value behaviour that needs no ambient storage: `From`,
//!   `key`, `is_empty`, `Display`, and `url()`'s raw-key fallback when no
//!   backend is registered.
//! - serde: serialises as the bare key string (REST/JSON parity with a
//!   plain `String` column).
//!
//! The storage-resolved `url()` path and the SQLite round-trip live in
//! their own test binaries (`file_field_storage_resolution.rs`,
//! `file_field_roundtrip.rs`) because the storage OnceLock + the
//! settings OnceLock are per-process.

use std::collections::HashMap;

use umbra::orm::{FieldSpec, FileField, ImageField, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, umbra::orm::Model)]
#[umbra(table = "umbra_file_doc")]
pub struct Doc {
    pub id: i64,
    pub attachment: FileField,
    pub cover: ImageField,
    pub thumbnail: Option<FileField>,
    // Explicit widget overrides the type-derived default.
    #[umbra(widget = "file")]
    pub avatar: String,
    // Explicit widget wins even on an ImageField.
    #[umbra(widget = "file")]
    pub banner: ImageField,
}

fn fields() -> HashMap<&'static str, &'static FieldSpec> {
    <Doc as Model>::FIELDS.iter().map(|f| (f.name, f)).collect()
}

#[test]
fn file_field_classifies_as_text_with_file_widget() {
    let by_name = fields();
    let attachment = by_name.get("attachment").expect("attachment field");
    assert_eq!(attachment.ty, SqlType::Text, "FileField stores as TEXT");
    assert_eq!(attachment.widget, Some("file"), "default widget is file");
    assert!(!attachment.nullable, "non-Option FileField is NOT NULL");
}

#[test]
fn image_field_classifies_as_text_with_image_widget() {
    let by_name = fields();
    let cover = by_name.get("cover").expect("cover field");
    assert_eq!(cover.ty, SqlType::Text, "ImageField stores as TEXT");
    assert_eq!(cover.widget, Some("image"), "default widget is image");
    assert!(!cover.nullable, "non-Option ImageField is NOT NULL");
}

#[test]
fn nullable_file_field_is_nullable_and_keeps_file_widget() {
    let by_name = fields();
    let thumb = by_name.get("thumbnail").expect("thumbnail field");
    assert_eq!(thumb.ty, SqlType::Text);
    assert!(thumb.nullable, "Option<FileField> is nullable");
    assert_eq!(
        thumb.widget,
        Some("file"),
        "Option<FileField> still gets the file widget"
    );
}

#[test]
fn explicit_widget_overrides_type_default() {
    let by_name = fields();
    // A plain String with an explicit widget.
    let avatar = by_name.get("avatar").expect("avatar field");
    assert_eq!(avatar.widget, Some("file"));
    // An ImageField whose default "image" is overridden to "file".
    let banner = by_name.get("banner").expect("banner field");
    assert_eq!(
        banner.widget,
        Some("file"),
        "explicit #[umbra(widget)] must beat the type-derived image default"
    );
}

#[test]
fn widget_survives_fieldspec_to_column_conversion() {
    // The admin reads migrate::Column, not FieldSpec directly — make sure
    // the file/image widget makes it through the conversion.
    let cols: Vec<umbra_core::migrate::Column> =
        <Doc as Model>::FIELDS.iter().map(Into::into).collect();
    let cover = cols.iter().find(|c| c.name == "cover").unwrap();
    assert_eq!(cover.widget.as_deref(), Some("image"));
    let attachment = cols.iter().find(|c| c.name == "attachment").unwrap();
    assert_eq!(attachment.widget.as_deref(), Some("file"));
}

// =====================================================================
// FileField value behaviour (no ambient storage).
// =====================================================================

#[test]
fn from_str_round_trips_through_key() {
    let f = FileField::from("ab12-photo.jpg");
    assert_eq!(f.key(), "ab12-photo.jpg");
    let g = FileField::from(String::from("doc.pdf"));
    assert_eq!(g.key(), "doc.pdf");
}

#[test]
fn default_is_empty() {
    let f = FileField::default();
    assert!(f.is_empty());
    assert_eq!(f.key(), "");
    let nonempty = FileField::from("x");
    assert!(!nonempty.is_empty());
}

#[test]
fn display_renders_the_key() {
    let f = FileField::from("ab12-photo.jpg");
    assert_eq!(f.to_string(), "ab12-photo.jpg");
    let img = ImageField::from("logo.png");
    assert_eq!(img.to_string(), "logo.png");
}

#[test]
fn url_falls_back_to_raw_key_when_no_storage() {
    // No backend registered in this binary, so url() returns the key
    // verbatim rather than panicking.
    let f = FileField::from("ab12-photo.jpg");
    assert_eq!(f.url(), "ab12-photo.jpg");
}

#[test]
fn image_field_derefs_to_file_field_behaviour() {
    let img = ImageField::from("hero.webp");
    // key(), is_empty(), url() all reachable through Deref.
    assert_eq!(img.key(), "hero.webp");
    assert!(!img.is_empty());
    assert_eq!(img.url(), "hero.webp");
}

#[test]
fn serde_serialises_as_bare_key_string() {
    let f = FileField::from("ab12-photo.jpg");
    let json = serde_json::to_string(&f).expect("serialize");
    assert_eq!(json, "\"ab12-photo.jpg\"", "serialises as the bare key");

    let back: FileField = serde_json::from_str("\"doc.pdf\"").expect("deserialize");
    assert_eq!(back.key(), "doc.pdf");

    // ImageField shares the wire shape.
    let img = ImageField::from("logo.png");
    assert_eq!(serde_json::to_string(&img).unwrap(), "\"logo.png\"");
    let img_back: ImageField = serde_json::from_str("\"x.png\"").unwrap();
    assert_eq!(img_back.key(), "x.png");
}
