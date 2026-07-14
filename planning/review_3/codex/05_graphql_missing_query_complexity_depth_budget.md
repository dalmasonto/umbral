# GraphQL Missing Depth And Complexity Budget

Category: Performance, Security
Severity: High

## Finding

GraphQL list fields are capped, but the schema does not appear to enforce query depth, query complexity, resolver count, or total work budget. A client can compose deeply nested relation queries that multiply work across loaders and database calls.

## Evidence

- `plugins/umbral-graphql/src/lib.rs` exposes a dynamic schema and `MAX_LIMIT` for list size.
- `plugins/umbral-graphql/src/loader.rs` uses batching, but batching does not cap nested fanout.
- No depth/complexity validation was found in the GraphQL plugin entry points.

## Risk

A single request can consume disproportionate CPU, memory, and database time. This is a denial-of-service risk for public GraphQL endpoints and a performance risk for internal endpoints.

## Recommendation

Add configurable GraphQL request budgets:

- Max depth.
- Max complexity or max field count.
- Max relation nesting.
- Optional resolver timeout.
- Production defaults that are conservative unless the app opts out.

## Suggested Tests

- A query exceeding depth limit is rejected before resolver execution.
- A query below the budget succeeds.
- A high-fanout relation query is blocked or paginated.

