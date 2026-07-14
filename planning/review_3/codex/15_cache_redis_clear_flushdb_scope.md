# Redis Cache Clear Uses FLUSHDB

Category: Operations, Correctness, Security
Severity: Medium

## Finding

The Redis cache backend clears entries with `FLUSHDB`. The code warns that Redis should be dedicated to Umbra cache, but the operation is still very broad.

## Evidence

- `plugins/umbral-cache/src/lib.rs:411-413` and `plugins/umbral-cache/src/lib.rs:468-472` clear Redis cache with `FLUSHDB`.
- The plugin docs warn operators to use a dedicated logical Redis database.

## Risk

If the Redis database is shared with sessions, rate limits, queues, another app, or manual keys, a cache clear can delete unrelated production data.

## Recommendation

Prefer namespaced cache clearing:

- Require or default a key prefix.
- Delete only matching keys using `SCAN` plus batched `DEL` or `UNLINK`.
- Keep `FLUSHDB` only behind an explicit "dedicated database" mode.

## Suggested Tests

- Cache clear deletes `umbra-cache:*` keys.
- Cache clear leaves unrelated keys in the same Redis DB intact.
- Dedicated-DB mode can still use `FLUSHDB` if explicitly selected.

