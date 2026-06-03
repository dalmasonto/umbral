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
    assert!(meta.singleton, "ModelMeta.singleton should mirror the const");
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
