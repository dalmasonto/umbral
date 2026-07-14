# Dynamic ORM Postgres Readbacks Leak Hidden Fields

Category: Security, Correctness
Severity: High

## Finding

The dynamic ORM read policy is applied consistently for normal dynamic reads, but several Postgres write/readback paths decode every model field into the returned JSON object. SQLite branches filter through `may_serialize`; Postgres branches do not.

This matters because REST and GraphQL create/update paths return these maps directly. A model with `password_hash`, `#[umbral(secret)]`, or `#[umbral(private)]` fields can be redacted on normal reads but exposed in Postgres create responses, bulk create responses, nested write responses, or transactional readbacks.

## Evidence

- `crates/umbral-core/src/orm/dynamic.rs:363-397` centralizes read-side field policy in `may_serialize` and `visible_select_cols`.
- `crates/umbral-core/src/orm/dynamic.rs:1586-1650` and `crates/umbral-core/src/orm/dynamic.rs:1658-1758` apply that policy before normal string/JSON reads.
- `crates/umbral-core/src/orm/dynamic.rs:1946-1948` filters SQLite `insert_json` readback fields through `may_serialize`.
- `crates/umbral-core/src/orm/dynamic.rs:1961-1968` Postgres `insert_json` uses `RETURNING *` and inserts every field into the response.
- `crates/umbral-core/src/orm/dynamic.rs:2110-2112` filters SQLite `insert_json_in_tx` readback fields through `may_serialize`.
- `crates/umbral-core/src/orm/dynamic.rs:2125-2132` Postgres `insert_json_in_tx` inserts every field into the response.
- `crates/umbral-core/src/orm/dynamic.rs:1805-1807` filters SQLite `fetch_one_json_in_tx` through `may_serialize`.
- `crates/umbral-core/src/orm/dynamic.rs:1820-1824` Postgres `fetch_one_json_in_tx` inserts every field into the response.
- `plugins/umbral-rest/src/lib.rs:3571`, `plugins/umbral-rest/src/lib.rs:3636`, `plugins/umbral-rest/src/lib.rs:3857`, `plugins/umbral-rest/src/lib.rs:3985`, and `plugins/umbral-rest/src/lib.rs:4302` return or consume these transactional dynamic JSON paths.
- `plugins/umbral-graphql/src/mutation.rs:181-186` returns `insert_json` output from create mutations.

## Risk

Hidden fields can leak on Postgres but not SQLite. That backend split is especially risky because local test suites often run SQLite while production uses Postgres. The leak path is also on write responses, so a user who can create a row may see server-owned or secret fields even when regular GET/list routes hide them.

## Recommendation

Create one helper for dynamic row-to-JSON decoding that takes a backend row and iterates only fields where `may_serialize` is true. Use it in:

- `fetch_one_json_in_tx`
- `insert_json`
- `insert_json_in_tx`

For Postgres inserts, prefer returning only visible columns rather than `RETURNING *` when possible. If SeaQuery makes explicit returning cumbersome, still decode only visible fields and avoid placing hidden data into response maps.

## Suggested Tests

- On Postgres, create a model with `password_hash`, one `#[umbral(secret)]` field, and one `#[umbral(private)]` field.
- Assert `DynQuerySet::insert_json` does not return those fields unless private access was explicitly allowed.
- Assert `DynQuerySet::insert_json_in_tx` has the same behavior.
- Assert `fetch_one_json_in_tx` after an update does not return hidden fields.
- Add REST create, REST bulk create, nested write, and GraphQL create smoke tests against Postgres for the same model.
