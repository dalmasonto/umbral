// Gap #73: the same runtime `ColumnNotFound` hazard applies to
// `ReverseSet<C>` — a `#[umbral(reverse_fk = ...)] ReverseSet<C>` field with
// NO sibling `#[sqlx(skip)]` must fail at COMPILE time.

use umbral::orm::{ForeignKey, Model, ReverseSet};

// `Clone` is required (independent of the fixture's point): `Post` is
// `Comment.post`'s FK target, and the heavy-relations epic's cache-aware
// to-one accessor clones the cached row on a `select_related` hit. Without
// it this fixture would fail with a SECOND, unrelated `Post: Clone` error,
// which would break the exact-stderr match this `compile_fail` test relies
// on (`reverse_set_missing_sqlx_skip.stderr` expects only the missing-
// `#[sqlx(skip)]` diagnostic below).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Post {
    id: i64,
    title: String,
    // Missing `#[sqlx(skip)]` — rejected by the derive.
    #[umbral(reverse_fk = "post")]
    #[serde(skip)]
    comment_set: ReverseSet<Comment>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
struct Comment {
    id: i64,
    body: String,
    post: ForeignKey<Post>,
}

fn main() {}
