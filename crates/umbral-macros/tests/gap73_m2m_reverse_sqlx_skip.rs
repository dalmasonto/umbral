//! Gap #73 — `#[derive(Model)]` turns the "virtual relation field with no
//! `#[sqlx(skip)]`" trap into a COMPILE error instead of a runtime
//! `Sqlx(ColumnNotFound(...))` on the first fetch.
//!
//! An `M2M<T>` (and a `ReverseSet<C>`) stores no column on the parent table.
//! The sibling `#[derive(sqlx::FromRow)]` can't be told that from inside the
//! `Model` derive, so without `#[sqlx(skip)]` FromRow generates a decoder that
//! reads the field as a real column — clean compile, runtime crash on read.
//! The `Model` derive DOES see every field's full attribute list, so it now
//! detects the missing `#[sqlx(skip)]` and emits a spanned `compile_error!`.
//!
//! - The two `compile_fail` fixtures prove the missing-attr case is rejected
//!   with the guiding message.
//! - The models defined inline below prove that WITH `#[sqlx(skip)]` the same
//!   shapes compile — if this file compiles, that half passed.

use umbral::orm::{ForeignKey, M2M, Model, ReverseSet};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Tag {
    id: i64,
    name: String,
}

// M2M WITH `#[sqlx(skip)]` — the correct form. Compiles.
//
// `Clone` is required here (not just decorative): `Post` is `Comment.post`'s
// FK target, and the heavy-relations epic's cache-aware to-one accessor
// (`comment.post()`) clones the cached row on a `select_related` hit — every
// `#[derive(Model)]` struct is expected to derive `Clone` for exactly this
// reason (see `ForeignKey`'s doc comment).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Post {
    id: i64,
    title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "tag")]
    tags: M2M<Tag>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "post")]
    comment_set: ReverseSet<Comment>,
}

// ReverseSet WITH `#[sqlx(skip)]` — the correct form. Compiles.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Comment {
    id: i64,
    body: String,
    post: ForeignKey<Post>,
}

#[test]
fn m2m_and_reverse_set_with_sqlx_skip_compile() {
    // Reaching this line means the correctly-formed models above compiled.
    assert_eq!(<Post as Model>::M2M_RELATIONS.len(), 1);
    assert_eq!(<Post as Model>::TABLE, "post");
}

#[test]
fn m2m_without_sqlx_skip_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/gap73_fixtures/m2m_missing_sqlx_skip.rs");
}

#[test]
fn reverse_set_without_sqlx_skip_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/gap73_fixtures/reverse_set_missing_sqlx_skip.rs");
}
