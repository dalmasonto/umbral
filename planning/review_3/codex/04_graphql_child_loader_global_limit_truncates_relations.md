# GraphQL Child Loader Applies A Global Relation Limit

Category: Correctness, Performance
Severity: Medium

## Finding

The child relation loader batches multiple parent IDs into one query, then applies one `MAX_LIMIT` to the combined result set. That means one parent with many children can consume the entire limit and silently starve other parents in the same batch.

## Evidence

- `plugins/umbral-graphql/src/loader.rs:107-148` loads children for a batch of parent keys.
- `plugins/umbral-graphql/src/loader.rs:229-254` applies `limit(crate::MAX_LIMIT)` to the combined `WHERE fk IN (...)` query.
- `plugins/umbral-graphql/src/lib.rs` defines `MAX_LIMIT` as a framework-level list cap.

## Risk

Nested GraphQL results can be silently incomplete. The error is data-dependent and may only appear under batch loading with mixed parent sizes, which makes it hard to debug.

## Recommendation

Use per-parent limits instead of a single global limit. Options include:

- Window function query partitioned by parent ID.
- Separate bounded queries per parent when the batch is small.
- Cursor pagination for child relation fields.

## Suggested Tests

- Parent A has more than `MAX_LIMIT` children.
- Parent B has several children.
- A query for `[A, B] { children { ... } }` should still return B's children.

