// Gap #73: an `#[umbral(m2m = ...)] M2M<T>` field with NO sibling
// `#[sqlx(skip)]` must fail at COMPILE time (it used to compile clean and
// only panic `ColumnNotFound` at runtime on the first fetch).

use umbral::orm::{M2M, Model};

#[derive(Debug, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Post {
    id: i64,
    title: String,
    // Missing `#[sqlx(skip)]` — this is the mistake the derive now rejects.
    #[umbral(m2m = "tag")]
    #[serde(skip)]
    tags: M2M<Tag>,
}

fn main() {}
