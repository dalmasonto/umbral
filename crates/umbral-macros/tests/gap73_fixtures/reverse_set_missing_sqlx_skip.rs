// Gap #73: the same runtime `ColumnNotFound` hazard applies to
// `ReverseSet<C>` — a `#[umbral(reverse_fk = ...)] ReverseSet<C>` field with
// NO sibling `#[sqlx(skip)]` must fail at COMPILE time.

use umbral::orm::{ForeignKey, Model, ReverseSet};

#[derive(Debug, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Post {
    id: i64,
    title: String,
    // Missing `#[sqlx(skip)]` — rejected by the derive.
    #[umbral(reverse_fk = "post")]
    #[serde(skip)]
    comment_set: ReverseSet<Comment>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Comment {
    id: i64,
    body: String,
    post: ForeignKey<Post>,
}

fn main() {}
