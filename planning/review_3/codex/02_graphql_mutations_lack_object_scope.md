# GraphQL Mutations Lack Object-Level Scope Checks

Category: Security, Correctness
Severity: High

## Finding

GraphQL mutation authorization is table-level. The `mutable_if` closure receives identity only, while update/delete operations target rows by primary key without a row-level ownership or object-scope check.

REST has richer controls for exposed models, including object scopes and hard denied fields. GraphQL does not appear to have an equivalent row-aware mutation guard.

## Evidence

- `plugins/umbral-graphql/src/lib.rs:267-325` exposes mutation controls through `mutable_if`, but the policy shape is identity-based.
- `plugins/umbral-graphql/src/mutation.rs:205-215` updates by primary key after `guard_write`.
- `plugins/umbral-graphql/src/mutation.rs:245-254` deletes by primary key after `guard_write`.
- `plugins/umbral-rest/src/lib.rs` has object scope and owner-field concepts that GraphQL does not mirror.

## Risk

If an app exposes a mutable GraphQL model to normal authenticated users, any authorized user for that table may be able to update or delete another user's row by guessing or obtaining the primary key. This is especially risky for tenant-owned or user-owned records.

## Recommendation

Add row-aware mutation authorization before update/delete. Practical options:

- Extend `mutable_if` to receive the target row, primary key, and request identity.
- Add an owner-field or object-scope API equivalent to REST.
- Require RLS for mutable GraphQL models unless an explicit row-aware policy exists.

## Suggested Tests

- Two users own different rows in the same model.
- User A can update own row.
- User A cannot update or delete User B's row through GraphQL.
- The same checks should hold for tenant-owned tables.

