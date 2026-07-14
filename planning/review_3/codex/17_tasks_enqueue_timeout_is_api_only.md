# Task Enqueue Timeout Is Accepted But Not Enforced

Category: Correctness, Simplicity
Severity: Low

## Finding

`EnqueueOptions` exposes a `timeout` field, but the documentation says it is API-only and not persisted. That makes it easy for callers to believe a timeout has been configured when the worker cannot actually enforce it across persistence boundaries.

## Evidence

- `plugins/umbral-tasks/src/lib.rs:480-487` documents `timeout` as API-only and not currently stored in `TaskRow`.
- Tasks are persisted and later claimed by workers, so enqueue-time runtime-only values are easy to lose.

## Risk

Callers may rely on a timeout that does not survive queue persistence. Long-running jobs can exceed expected limits without an obvious configuration error.

## Recommendation

Either implement persisted task timeouts or remove/deprecate the option until it is enforceable. If keeping it temporarily, emit a warning when set.

## Suggested Tests

- Enqueuing with timeout either persists the timeout and worker enforces it, or fails/warns clearly.
- Worker behavior is deterministic after process restart.

