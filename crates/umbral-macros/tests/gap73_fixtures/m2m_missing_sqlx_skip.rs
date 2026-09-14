// Gap #73: an `#[umbral(m2m = ...)] M2M<T>` field with NO sibling
// `#[sqlx(skip)]` must fail at COMPILE time (it used to compile clean and
// only panic `ColumnNotFound` at runtime on the first fetch).

use umbral::orm::{M2M, Model};

// `Clone` is required even in this compile-fail fixture: the missing
// `#[sqlx(skip)]` produces a spanned `compile_error!` for the `tags` field
// specifically, but the REST of `#[derive(Model)]`'s output (including the
// heavy-relations epic's cache-aware M2M forward accessor, Task 2b, which
// clones the cached row set via `.to_vec()`) still gets emitted alongside
// it. Without `Clone` here, the compile would additionally fail with an
// unrelated `Tag: Clone` error, breaking this test's exact-stderr match.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
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
