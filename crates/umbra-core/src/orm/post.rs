//! The hardcoded `Post` model.
//!
//! M1 ships exactly one model so the QuerySet machinery has something
//! concrete to query against. Per CLAUDE.md M1: "QuerySet builder → SQL
//! for one hard-coded model (no macros)." The model is intentionally
//! tiny: an autoincrement primary key, two text columns, and a nullable
//! datetime column. That covers the basic field-type repertoire (i64,
//! String, Option<DateTime<Utc>>) the column module needs to demonstrate.
//!
//! At M2 the `Model` trait gets extracted from this concrete shape; at M3
//! the trait impl gets generated from a `#[derive(Model)]` on the struct.
//! The struct itself is the eventual target both abstractions converge on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A blog post. The M1 hardcoded model.
///
/// The struct derives `sqlx::FromRow` so a sea-query SELECT can be
/// executed via `sqlx::query_as::<_, Post>(...)` and rows come back
/// already typed. This is the M1 stand-in for the M3 derive macro that
/// will eventually generate the same impl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<DateTime<Utc>>,
}

impl Post {
    /// Entry point for queries: `Post::objects().filter(...).fetch().await`.
    ///
    /// Returns a `Manager<Post>` which in turn produces `QuerySet<Post>`s
    /// via its chainable methods. See `docs/specs/03-orm-querysets.md`.
    pub fn objects() -> crate::orm::Manager<Post> {
        crate::orm::Manager::new()
    }
}

/// The hand-written `Model` impl. M3 will generate this exact impl from
/// `#[derive(Model)]` on the struct above.
///
/// `Post::TABLE` and `Post::FIELDS` are reached via the trait (not as
/// inherent consts); call sites use `<Post as Model>::TABLE` or just
/// `Post::TABLE` when the trait is in scope.
impl crate::orm::Model for Post {
    type PrimaryKey = i64;

    const NAME: &'static str = "Post";

    const TABLE: &'static str = "post";

    const FIELDS: &'static [crate::orm::FieldSpec] = &[
        crate::orm::FieldSpec {
            name: "id",
            ty: crate::orm::SqlType::BigInt,
            primary_key: true,
            nullable: false,
            supported_backends: &[],
        },
        crate::orm::FieldSpec {
            name: "title",
            ty: crate::orm::SqlType::Text,
            primary_key: false,
            nullable: false,
            supported_backends: &[],
        },
        crate::orm::FieldSpec {
            name: "body",
            ty: crate::orm::SqlType::Text,
            primary_key: false,
            nullable: false,
            supported_backends: &[],
        },
        crate::orm::FieldSpec {
            name: "published_at",
            ty: crate::orm::SqlType::Timestamptz,
            primary_key: false,
            nullable: true,
            supported_backends: &[],
        },
    ];

    fn primary_key(&self) -> i64 {
        self.id
    }
}

/// The sibling column module.
///
/// Each column constant here is the typed handle used in `filter` /
/// `order_by` predicates: `post::ID.eq(2)`, `post::PUBLISHED_AT.is_not_null()`,
/// etc. M3 will generate this module from the `#[derive(Model)]` on the
/// struct above.
///
/// The double-`post` path (`umbra_core::orm::post::post::ID`) reads oddly
/// but matches the spec's `<model>.rs` file + sibling `mod <model>` of
/// column constants convention. clippy's `module_inception` lint is
/// silenced because the pattern is intentional.
#[allow(clippy::module_inception)]
pub mod post {
    use super::Post;
    use crate::orm::column::{IntCol, NullableDateTimeCol, StrCol};

    pub const ID: IntCol<Post> = IntCol::new("id");
    pub const TITLE: StrCol<Post> = StrCol::new("title");
    pub const BODY: StrCol<Post> = StrCol::new("body");
    pub const PUBLISHED_AT: NullableDateTimeCol<Post> = NullableDateTimeCol::new("published_at");
}
