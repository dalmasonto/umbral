# Raw Realtime Group Publishing Bypasses Sender Policy

Category: Security, API Simplicity
Severity: Low

## Finding

The realtime plugin offers safe inbound publish helpers through `MessageContext`, but the lower-level `Realtime::to_group` API sends directly to a group and bypasses sender policy checks. The docs warn about this, but the API remains easy to misuse from application code.

## Evidence

- `plugins/umbral-realtime/src/lib.rs:756-785` defines group policy behavior.
- `plugins/umbral-realtime/src/lib.rs:838-860` checks sender policy through `MessageContext::publish`.
- Plugin docs note that raw group publishing is unrestricted and should be used for trusted server-side events.

## Risk

Application handlers can accidentally use the raw API for user-originated messages and bypass group membership checks.

## Recommendation

Make safe usage harder to miss:

- Rename raw publish APIs to indicate trusted server-side use.
- Provide a typed wrapper for user-originated publish paths that requires `MessageContext`.
- Add lint-like docs and examples that prefer the safe helper.

## Suggested Tests

- User-originated publish path denies unauthorized group sends.
- Trusted server-side publish path remains available and clearly named.

