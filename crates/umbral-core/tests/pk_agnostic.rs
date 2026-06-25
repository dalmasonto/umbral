//! PK refactor — keystone. `HydrateRelated::pk_as_json` returns a row's
//! primary key as a `serde_json::Value` whatever the PK type, and
//! `orm::pk_key` canonicalises it into a collision-free bucket key. These
//! are the PK-agnostic primitives the relation-hydration lift builds on.

#![allow(dead_code)]

use serde_json::json;
use umbral::orm::{HydrateRelated, pk_key};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "pk_int_post")]
pub struct IntPost {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "pk_tag")]
pub struct Tag {
    #[umbral(primary_key)]
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "pk_doc")]
pub struct Doc {
    #[umbral(primary_key)]
    pub id: uuid::Uuid,
    pub title: String,
}

#[test]
fn pk_as_json_reads_the_pk_for_every_pk_type() {
    let post = IntPost {
        id: 42,
        title: "x".into(),
    };
    assert_eq!(post.pk_as_json(), Some(json!(42)));

    let tag = Tag {
        slug: "rust".into(),
        name: "Rust".into(),
    };
    assert_eq!(tag.pk_as_json(), Some(json!("rust")));

    let uid = uuid::Uuid::nil();
    let doc = Doc {
        id: uid,
        title: "t".into(),
    };
    // A Uuid serializes to its hyphenated string form.
    assert_eq!(doc.pk_as_json(), Some(json!(uid.to_string())));
}

#[test]
fn pk_key_namespaces_by_shape_so_42_and_str_42_never_collide() {
    assert_eq!(pk_key(&json!(42)), "n:42");
    assert_eq!(pk_key(&json!("42")), "s:42");
    assert_ne!(pk_key(&json!(42)), pk_key(&json!("42")));

    // A Uuid (string-shaped) and a slug both land in the `s:` namespace,
    // distinct from any numeric id.
    assert_eq!(pk_key(&json!("rust")), "s:rust");
    let uid = uuid::Uuid::nil().to_string();
    assert_eq!(pk_key(&json!(uid)), format!("s:{uid}"));
}

#[test]
fn pk_as_json_round_trips_through_pk_key() {
    // The two primitives compose: a parent's pk_as_json keyed by pk_key
    // must match a child's FK value (also a Value) keyed by pk_key — this
    // is exactly how the hydration buckets line parents up with children.
    let tag = Tag {
        slug: "rust".into(),
        name: "Rust".into(),
    };
    let parent_key = pk_key(&tag.pk_as_json().unwrap());
    let child_fk_value = json!("rust"); // a child's FK column pointing at the tag
    assert_eq!(parent_key, pk_key(&child_fk_value));
}
