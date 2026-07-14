# ORM try_for_each Uses Offset Pagination

Category: Performance, Correctness
Severity: Medium

## Finding

`QuerySet::try_for_each` is the streaming-ish terminal for large exports, migrations, and batch transforms. It keeps memory bounded, but internally it paginates with increasing `OFFSET`.

That shape gets slower as the scan progresses, and it can skip or repeat rows if the table changes during iteration. The rustdoc notes the consistency caveat, but the method is positioned as the tool for million-row jobs where offset pagination is most likely to hurt.

## Evidence

- `crates/umbral-core/src/orm/queryset/mod.rs:1796-1802` describes `try_for_each` as the right shape for million-row exports, migrations, and batch transforms.
- `crates/umbral-core/src/orm/queryset/mod.rs:1817-1822` documents concurrent-mutation caveats and states related hooks are not applied.
- `crates/umbral-core/src/orm/queryset/mod.rs:1834-1836` initializes an offset counter.
- `crates/umbral-core/src/orm/queryset/mod.rs:1840-1851` applies `limit(...).offset(offset)` for each chunk on both backends.
- `crates/umbral-core/src/orm/queryset/mod.rs:1868` increments the offset by fetched row count.

## Risk

For large tables, later chunks can become increasingly expensive because the database still has to walk past skipped rows. For live tables, deletes and inserts before the current offset can shift later pages, causing missed or repeated records.

This is lower risk than a direct security issue because the caveat is documented, but it is a performance and correctness footgun for the exact workloads the method targets.

## Recommendation

Add a keyset or cursor-based variant:

- Require a stable unique order, usually the primary key.
- Track the last seen key and use `WHERE pk > last_pk ORDER BY pk ASC LIMIT chunk`.
- For descending order, use the symmetric `< last_pk` condition.

Keep offset-based `try_for_each` for arbitrary ordered querysets where keyset pagination cannot be expressed, but document the keyset variant as the recommended path for full-table batch jobs.

## Suggested Tests

- A keyset chunk iterator visits every row exactly once on integer and UUID/string primary-key models where supported.
- Deleting a row before the current cursor during iteration does not skip the next row.
- The offset variant keeps its current behavior and caveat.
