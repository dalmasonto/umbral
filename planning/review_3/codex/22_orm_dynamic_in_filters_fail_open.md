# Dynamic IN Filters Can Fail Open

Category: Security, Correctness
Severity: High

## Finding

The dynamic ORM fixed the dangerous single-value equality case by making invalid filters match no rows. The multi-value helpers still keep older no-op semantics when all supplied values are invalid, when an M2M field is unknown, or when all child IDs fail coercion.

For optional UI filters, no-op can be reasonable when the filter was not supplied. For a supplied filter that is meant to scope a bulk action or row set, no-op widens the query to every row currently in the queryset.

## Evidence

- `crates/umbral-core/src/orm/dynamic.rs:201-207` defines `never_matches` as the safe fallback.
- `crates/umbral-core/src/orm/dynamic.rs:220-223` documents that invalid equality filters must produce no rows, never no filter.
- `crates/umbral-core/src/orm/dynamic.rs:817-838` implements that fail-closed behavior for `filter_eq_string`.
- `crates/umbral-core/src/orm/dynamic.rs:740-745` documents `filter_in_strings` all-unparseable and unknown-column inputs as no-ops.
- `crates/umbral-core/src/orm/dynamic.rs:751-811` returns the unchanged queryset when all parsed numeric, float, or UUID values are invalid.
- `crates/umbral-core/src/orm/dynamic.rs:667-738` does the same in `filter_m2m_contains_any` for unknown M2M fields and all-invalid child ID lists.
- `plugins/umbral-admin/src/config.rs:242-245`, `plugins/umbral-admin/src/config.rs:289-292`, and `plugins/umbral-admin/src/config.rs:334-338` use `filter_in_strings` before delete, restore, and hard-delete bulk actions.
- `plugins/umbral-admin/src/rows.rs:47-55` uses `filter_m2m_contains_any` and `filter_in_strings` for changelist filters.

## Risk

An all-invalid ID list can accidentally broaden a bulk action, restore, hard delete, or filtered queryset. The most concerning path is admin bulk action handling: an invalid selected ID payload should affect zero rows or fail validation, not drop the primary-key predicate.

## Recommendation

Split the semantics:

- Keep a clearly named optional-filter helper for UI cases where absent input should be ignored.
- Make supplied but all-invalid filters fail closed by pushing `never_matches`.
- Treat unknown field names as programmer errors or fail-closed in mutating/bulk-action contexts.

At minimum, `filter_in_strings` should distinguish `vals.is_empty()` from `vals` present but all rejected after coercion. The former can remain no-op; the latter should match no rows.

## Suggested Tests

- `filter_in_strings("id", &["garbage"])` on an integer primary key returns zero rows.
- Admin `delete_selected`, `restore_selected`, and `delete_permanently` with all-invalid IDs affect zero rows.
- `filter_m2m_contains_any("tags", &["garbage"])` returns zero rows for integer child PKs.
- Existing mixed-value behavior remains: valid values still match while invalid values are dropped.
