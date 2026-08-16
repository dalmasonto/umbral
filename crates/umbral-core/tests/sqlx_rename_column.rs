// The private test model emits public column constants from `#[derive(Model)]`.
#![allow(dead_code, private_interfaces)]

//! umbral honours `#[sqlx(rename = "…")]` for a field's DB column name, so the
//! Rust field can be pretty (`author`) while the column is the database's
//! (`author_id`). Without this, the migration would create an `author` column
//! while `sqlx::FromRow` reads `author_id` — a silent mismatch. This is also
//! what `inspectdb --framework django` relies on.

use umbral::orm::{ForeignKey, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "srx_user")]
struct User {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "srx_post")]
struct Post {
    id: i64,
    title: String,
    // Pretty Rust field, database column is `author_id`.
    #[sqlx(rename = "author_id")]
    author: ForeignKey<User>,
    // A renamed plain column too.
    #[sqlx(rename = "view_count")]
    views: i32,
}

#[test]
fn sqlx_rename_becomes_the_column_name() {
    let by_field: std::collections::HashMap<&str, &umbral::orm::FieldSpec> =
        <Post as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    // The FieldSpec.name (the DB column) is the renamed value, not the Rust
    // field identifier.
    assert!(
        by_field.contains_key("author_id"),
        "the FK column must be `author_id` (the sqlx rename), got fields: {:?}",
        by_field.keys().collect::<Vec<_>>()
    );
    assert!(
        by_field.contains_key("view_count"),
        "the renamed plain column must be `view_count`"
    );
    assert_eq!(by_field.get("author_id").unwrap().ty, SqlType::ForeignKey);
    // The Rust field identity is NOT a column.
    assert!(
        !by_field.contains_key("author"),
        "the Rust field name `author` must not be the column name"
    );

    // The generated column const is NAMED after the Rust field (post::AUTHOR)
    // but targets the `author_id` column — a filter compiles and points right.
    let _pred = post::AUTHOR.eq(1);
    let _pred2 = post::VIEWS.gt(10);
}
