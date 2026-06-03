//! Tests for `#[umbra(display = "...", icon = "...")]` attributes on
//! `#[derive(Model)]` structs (gap 44).

#![allow(dead_code, private_interfaces)]
//!
//! Covers:
//!   1. A model with explicit `display` and `icon` attributes emits the
//!      right `Model::DISPLAY` and `Model::ICON` constants.
//!   2. A model with no `#[umbra(...)]` attributes falls back to the
//!      defaults: `DISPLAY == NAME` and `ICON == "database"`.
//!   3. A model with only `display` set gets the custom label and the
//!      default icon.
//!   4. A model with only `icon` set gets the default display and the
//!      custom icon.

use serde::{Deserialize, Serialize};

use umbra::orm::Model;

// =========================================================================
// Test models.
// =========================================================================

/// Both display and icon explicitly set.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(display = "Users", icon = "users")]
struct AuthUser {
    id: i64,
    username: String,
}

/// No umbra attributes — all defaults.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
struct BlogPost {
    id: i64,
    title: String,
}

/// Only display set.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(display = "Articles")]
struct Article {
    id: i64,
    body: String,
}

/// Only icon set.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(icon = "file-text")]
struct Document {
    id: i64,
    content: String,
}

// =========================================================================
// Tests.
// =========================================================================

#[test]
fn explicit_display_and_icon_propagate() {
    assert_eq!(<AuthUser as Model>::DISPLAY, "Users");
    assert_eq!(<AuthUser as Model>::ICON, "users");
}

#[test]
fn no_attributes_fall_back_to_defaults() {
    // DISPLAY defaults to NAME (the struct ident string).
    assert_eq!(<BlogPost as Model>::DISPLAY, <BlogPost as Model>::NAME);
    assert_eq!(<BlogPost as Model>::NAME, "BlogPost");
    // ICON defaults to "database".
    assert_eq!(<BlogPost as Model>::ICON, "database");
}

#[test]
fn display_only_uses_custom_label_and_default_icon() {
    assert_eq!(<Article as Model>::DISPLAY, "Articles");
    assert_eq!(<Article as Model>::ICON, "database");
}

#[test]
fn icon_only_uses_default_display_and_custom_icon() {
    // DISPLAY falls back to the struct name.
    assert_eq!(<Document as Model>::DISPLAY, "Document");
    assert_eq!(<Document as Model>::ICON, "file-text");
}

// =========================================================================
// BUG-9 from bugs/tests/testBugs.md: `#[umbra(singleton)]` flips
// the `Model::SINGLETON` const + the `ModelMeta.singleton` flag so
// admin and any tool can detect single-row models.
// =========================================================================

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(singleton)]
struct SiteSettings {
    id: i64,
    title: String,
    maintenance_mode: bool,
}

#[test]
fn singleton_attribute_flips_const() {
    assert!(
        <SiteSettings as Model>::SINGLETON,
        "Model::SINGLETON should be true for a model with #[umbra(singleton)]",
    );
    let meta = umbra::migrate::ModelMeta::for_::<SiteSettings>();
    assert!(
        meta.singleton,
        "ModelMeta.singleton should mirror the const"
    );
}

#[test]
fn singleton_defaults_to_false_when_unset() {
    assert!(
        !<BlogPost as Model>::SINGLETON,
        "Model::SINGLETON should default to false",
    );
    let meta = umbra::migrate::ModelMeta::for_::<BlogPost>();
    assert!(!meta.singleton);
}

#[test]
fn singleton_round_trips_through_json_snapshot() {
    let meta = umbra::migrate::ModelMeta::for_::<SiteSettings>();
    let json = serde_json::to_string(&meta).unwrap();
    assert!(
        json.contains("\"singleton\":true"),
        "snapshot must carry singleton:true; got: {json}",
    );
    let round: umbra::migrate::ModelMeta = serde_json::from_str(&json).unwrap();
    assert!(round.singleton);
}

// =========================================================================
// IMP-3: `#[umbra(min = N)]` / `#[umbra(max = N)]` propagate from the
// macro into FieldSpec + ModelMeta.
// =========================================================================

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
struct PersonRow {
    id: i64,
    #[umbra(min = 0, max = 150)]
    age: i32,
    #[umbra(min = 1)]
    score: i32,
    plain: i32,
}

#[test]
fn min_max_attributes_reach_field_spec() {
    let age = <PersonRow as Model>::FIELDS
        .iter()
        .find(|f| f.name == "age")
        .expect("age field missing");
    assert_eq!(age.min, Some(0));
    assert_eq!(age.max, Some(150));

    let score = <PersonRow as Model>::FIELDS
        .iter()
        .find(|f| f.name == "score")
        .expect("score field missing");
    assert_eq!(score.min, Some(1));
    assert_eq!(score.max, None);

    let plain = <PersonRow as Model>::FIELDS
        .iter()
        .find(|f| f.name == "plain")
        .expect("plain field missing");
    assert_eq!(plain.min, None);
    assert_eq!(plain.max, None);
}

// =========================================================================
// BUG-6 / BUG-7 / BUG-8: struct-level `unique_together`, `indexes`,
// `ordering` propagate from the macro into the Model trait consts and
// the ModelMeta snapshot.
// =========================================================================

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(
    unique_together = [["tenant_id", "slug"], ["author_id", "year"]],
    indexes = [["tenant_id", "created_at"], ["status"]],
    ordering = ["-published_at", "id"]
)]
struct PostWithStructAttrs {
    id: i64,
    tenant_id: i64,
    slug: String,
    author_id: i64,
    year: i32,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    published_at: chrono::DateTime<chrono::Utc>,
}

#[test]
fn struct_attrs_reach_model_consts() {
    assert_eq!(
        <PostWithStructAttrs as Model>::UNIQUE_TOGETHER,
        &[&["tenant_id", "slug"][..], &["author_id", "year"][..],][..],
    );
    assert_eq!(
        <PostWithStructAttrs as Model>::INDEXES,
        &[&["tenant_id", "created_at"][..], &["status"][..]][..],
    );
    assert_eq!(
        <PostWithStructAttrs as Model>::ORDERING,
        &[("published_at", true), ("id", false)][..],
    );
}

// =========================================================================
// BUG-11 / BUG-12 / BUG-13: `Slug` / `Email` / `Url` wrapper types
// propagate through the macro into the `text_format` marker on
// FieldSpec / Column.
// =========================================================================

use umbra::orm::{Email, Slug, Url, ValidatorError};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
struct ValidatorRow {
    id: i64,
    slug: Slug,
    email: Email,
    homepage: Url,
    plain: String,
}

#[test]
fn validator_types_emit_text_format_marker() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <ValidatorRow as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    assert_eq!(by_name["slug"].text_format, Some("slug"));
    assert_eq!(by_name["slug"].ty, umbra::orm::SqlType::Text);
    assert_eq!(by_name["email"].text_format, Some("email"));
    assert_eq!(by_name["email"].ty, umbra::orm::SqlType::Text);
    assert_eq!(by_name["homepage"].text_format, Some("url"));
    assert_eq!(by_name["homepage"].ty, umbra::orm::SqlType::Text);
    assert_eq!(by_name["plain"].text_format, None);
    assert_eq!(by_name["plain"].ty, umbra::orm::SqlType::Text);
}

#[test]
fn validator_types_validate_inputs() {
    assert!(Slug::new("hello-world").is_ok());
    assert!(matches!(
        Slug::new("bad slug"),
        Err(ValidatorError::InvalidSlug(_))
    ));

    assert!(Email::new("a@b.c").is_ok());
    assert!(matches!(
        Email::new("plain"),
        Err(ValidatorError::InvalidEmail(_))
    ));

    assert!(Url::new("https://example.com/").is_ok());
    assert!(matches!(
        Url::new("not-a-url"),
        Err(ValidatorError::InvalidUrl(_))
    ));
}

#[test]
fn validator_text_format_round_trips_through_meta_snapshot() {
    let meta = umbra::migrate::ModelMeta::for_::<ValidatorRow>();
    let json = serde_json::to_string(&meta).unwrap();
    // The three wrapper fields carry their marker; `plain` omits it
    // entirely (Option::is_none → skip).
    assert!(json.contains("\"text_format\":\"slug\""), "{json}");
    assert!(json.contains("\"text_format\":\"email\""), "{json}");
    assert!(json.contains("\"text_format\":\"url\""), "{json}");
    let back: umbra::migrate::ModelMeta = serde_json::from_str(&json).unwrap();
    let by_name: std::collections::HashMap<_, _> = back
        .fields
        .iter()
        .map(|c| (c.name.as_str(), c.text_format.as_deref()))
        .collect();
    assert_eq!(by_name["slug"], Some("slug"));
    assert_eq!(by_name["email"], Some("email"));
    assert_eq!(by_name["homepage"], Some("url"));
    assert_eq!(by_name["plain"], None);
}

// =========================================================================
// BUG-16 step 2: `M2M<T>` parent_id hydration.
// =========================================================================

use umbra::orm::{HydrateRelated, M2M};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, Model)]
struct PostWithTags {
    id: i64,
    title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    tags: M2M<Tag>,
}

#[test]
fn m2m_relations_propagate_to_model_const() {
    let relations = <PostWithTags as Model>::M2M_RELATIONS;
    assert_eq!(
        relations.len(),
        1,
        "expected one M2M relation on PostWithTags"
    );
    assert_eq!(relations[0].field_name, "tags");
    assert_eq!(relations[0].target_table, "tag");
    assert_eq!(relations[0].target_name, "Tag");
    let meta = umbra::migrate::ModelMeta::for_::<PostWithTags>();
    assert_eq!(meta.m2m_relations.len(), 1);
    assert_eq!(meta.m2m_relations[0].field_name, "tags");
}

#[test]
fn set_m2m_parent_ids_writes_pk_into_each_m2m_field() {
    let mut row = PostWithTags {
        id: 42,
        title: "hello".to_string(),
        tags: M2M::empty(),
    };
    assert_eq!(row.tags.parent_id(), None);
    row.set_m2m_parent_ids();
    assert_eq!(
        row.tags.parent_id(),
        Some(42),
        "set_m2m_parent_ids must seed each M2M<U> from the parent's PK",
    );
}

#[test]
fn set_m2m_parent_ids_is_a_noop_for_models_without_m2m_fields() {
    // BlogPost has no M2M field — calling set_m2m_parent_ids must not
    // panic or touch anything. The macro's empty-branch path.
    let mut row = BlogPost {
        id: 1,
        title: "t".to_string(),
    };
    row.set_m2m_parent_ids();
    // No assertion required beyond "this compiles + returns without panicking".
    let _ = row.title.len();
}

#[test]
fn struct_attrs_round_trip_through_model_meta() {
    let meta = umbra::migrate::ModelMeta::for_::<PostWithStructAttrs>();
    assert_eq!(
        meta.unique_together,
        vec![
            vec!["tenant_id".to_string(), "slug".to_string()],
            vec!["author_id".to_string(), "year".to_string()],
        ],
    );
    assert_eq!(
        meta.indexes,
        vec![
            vec!["tenant_id".to_string(), "created_at".to_string()],
            vec!["status".to_string()],
        ],
    );
    assert_eq!(
        meta.ordering,
        vec![
            ("published_at".to_string(), true),
            ("id".to_string(), false),
        ],
    );

    // serde round-trip: explicit values survive, defaults are skipped.
    let json = serde_json::to_string(&meta).unwrap();
    let back: umbra::migrate::ModelMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(back.unique_together, meta.unique_together);
    assert_eq!(back.indexes, meta.indexes);
    assert_eq!(back.ordering, meta.ordering);
}

#[test]
fn min_max_round_trip_through_model_meta_snapshot() {
    let meta = umbra::migrate::ModelMeta::for_::<PersonRow>();
    let json = serde_json::to_string(&meta).unwrap();
    // Only the `age` field carries both bounds; `plain` omits them
    // entirely thanks to skip_serializing_if=Option::is_none.
    assert!(
        json.contains("\"min\":0"),
        "snapshot must carry min:0; got: {json}"
    );
    assert!(
        json.contains("\"max\":150"),
        "snapshot must carry max:150; got: {json}"
    );

    let round: umbra::migrate::ModelMeta = serde_json::from_str(&json).unwrap();
    let age = round
        .fields
        .iter()
        .find(|c| c.name == "age")
        .expect("age column missing after round-trip");
    assert_eq!(age.min, Some(0));
    assert_eq!(age.max, Some(150));
}
