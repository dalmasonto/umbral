# Bearer Authentication Writes On Every Request

Category: Performance, Scalability
Severity: Medium

## Finding

Bearer authentication updates token `last_used` on every successful authenticated request. This turns all token-authenticated reads into database writes.

## Evidence

- `plugins/umbral-auth/src/bearer_auth.rs:83-102` authenticates token, loads user, then calls `token.touch_last_used().await`.

## Risk

High-volume API clients can create avoidable write amplification, row contention, replication lag, and WAL growth. A single hot token can also become a database hotspot.

## Recommendation

Throttle or coalesce `last_used` updates:

- Only update if the previous value is older than a configured interval.
- Send usage updates to a background worker.
- Make token usage tracking configurable for latency-sensitive APIs.

## Suggested Tests

- Multiple requests within the threshold update `last_used` once.
- A request after the threshold updates it again.
- Authentication still succeeds if usage tracking fails non-critically, if that behavior is chosen.

