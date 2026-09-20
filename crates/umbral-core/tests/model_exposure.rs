//! The model-exposure contract: `ModelMeta`'s derived views over a model's
//! declared field facts. Behavioural — real models via `ModelMeta::for_::<T>()`
//! (the same value the runtime registry caches), asserting the resulting field
//! SETS, not generated SQL. See docs/decisions/2026-09-15-model-exposure-contract.md.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use umbral::migrate::ModelMeta;
use umbral::orm::{ForeignKey, Model};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "mx_category")]
pub struct Category {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "mx_product")]
pub struct Product {
    pub id: i64, // primary key -> server-managed, never client-writable
    #[umbral(string)] // is_string_repr -> the display field
    pub name: String,
    #[umbral(private)] // confidential read; still WRITABLE (private is a read fact)
    pub cost: String,
    #[umbral(secret)] // never serialized; still writable (secret is a read fact)
    pub signing_key: String,
    #[umbral(privileged, default = "false")] // mass-assignment guarded -> not writable
    pub is_featured: bool,
    #[umbral(auto_now_add)] // server-populated -> not writable
    pub created_at: DateTime<Utc>,
    pub category: ForeignKey<Category>, // foreign key
}

fn names<'a>(it: impl Iterator<Item = &'a umbral::migrate::Column>) -> Vec<String> {
    it.map(|c| c.name.clone()).collect()
}

#[test]
fn field_lookup_finds_by_name_and_misses_cleanly() {
    let meta = ModelMeta::for_::<Product>();
    assert_eq!(meta.field("cost").map(|c| c.name.as_str()), Some("cost"));
    assert!(meta.field("does_not_exist").is_none());
}

#[test]
fn display_field_is_the_is_string_repr_column() {
    let meta = ModelMeta::for_::<Product>();
    assert_eq!(meta.display_field().map(|c| c.name.as_str()), Some("name"));
}

#[test]
fn public_fields_exclude_private_and_secret() {
    let meta = ModelMeta::for_::<Product>();
    let public = names(meta.public_fields());
    assert!(public.contains(&"name".to_string()));
    assert!(public.contains(&"id".to_string()));
    assert!(
        !public.contains(&"cost".to_string()),
        "private field must be excluded"
    );
    assert!(
        !public.contains(&"signing_key".to_string()),
        "secret field must be excluded"
    );
}

#[test]
fn serializable_fields_adds_back_private_when_allowed_never_secret() {
    let meta = ModelMeta::for_::<Product>();

    let locked = names(meta.serializable_fields(false));
    assert_eq!(locked, names(meta.public_fields()), "false == public set");

    let unlocked = names(meta.serializable_fields(true));
    assert!(
        unlocked.contains(&"cost".to_string()),
        "private added back when allowed"
    );
    assert!(
        !unlocked.contains(&"signing_key".to_string()),
        "secret is NEVER serialized, unlock or not"
    );
}

#[test]
fn is_server_managed_covers_pk_and_auto_columns() {
    let meta = ModelMeta::for_::<Product>();
    let by = |n: &str| meta.field(n).map(|c| meta.is_server_managed(c)).unwrap();
    assert!(by("id"), "primary key is server-managed");
    assert!(by("created_at"), "auto_now_add is server-managed");
    assert!(!by("name"), "a plain field is not server-managed");
}

#[test]
fn writable_fields_exclude_pk_auto_and_privileged_but_keep_private_and_secret() {
    let meta = ModelMeta::for_::<Product>();
    let writable = names(meta.writable_fields());
    assert!(writable.contains(&"name".to_string()));
    // private/secret are READ facts; the fields stay writable (write-blocking
    // is noform/privileged, which these are not):
    assert!(writable.contains(&"cost".to_string()));
    assert!(writable.contains(&"signing_key".to_string()));
    assert!(!writable.contains(&"id".to_string()), "pk excluded");
    assert!(
        !writable.contains(&"created_at".to_string()),
        "auto_now_add excluded"
    );
    assert!(
        !writable.contains(&"is_featured".to_string()),
        "privileged excluded"
    );
}

#[test]
fn foreign_keys_returns_only_fk_columns_with_their_target() {
    let meta = ModelMeta::for_::<Product>();
    let fks: Vec<_> = meta.foreign_keys().collect();
    assert_eq!(fks.len(), 1, "one FK on Product");
    assert_eq!(fks[0].fk_target.as_deref(), Some("mx_category"));
    // A model with no FK returns an empty iterator.
    assert_eq!(ModelMeta::for_::<Category>().foreign_keys().count(), 0);
}
