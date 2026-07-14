# REST Documentation Contradicts Hardened Defaults

Category: Correctness, Security Documentation
Severity: Medium

## Finding

The REST plugin rustdoc still describes earlier unsafe defaults: exposing every model except a small internal list, no built-in auth gate, and open routes. The implementation now appears much more conservative.

## Evidence

- `plugins/umbral-rest/src/lib.rs:18-36` says the default exposes every model except a few internals and ships with no auth gate.
- `plugins/umbral-rest/src/lib.rs:95-122` contains a broader internal deny list.
- `plugins/umbral-rest/src/lib.rs:381-390` sets default permission to `ReadOnly`.
- `plugins/umbral-rest/src/lib.rs:724-742` defaults to no-store, read-only, no-auth unless configured.
- `plugins/umbral-rest/src/lib.rs:398-437` contains system warnings for risky configurations.

## Risk

Stale security docs can cause users to overcompensate, misconfigure, or misunderstand the real defaults. It also makes external audits harder because docs and code disagree on the security model.

## Recommendation

Update the REST rustdoc and examples to match current behavior:

- Default model exposure.
- Default permission mode.
- Default authentication behavior.
- Production warnings and intended hardening path.

## Suggested Tests

- Add doc tests or snapshot tests for default REST configuration summaries.
- Keep docs and default config in one generated or shared source where feasible.

