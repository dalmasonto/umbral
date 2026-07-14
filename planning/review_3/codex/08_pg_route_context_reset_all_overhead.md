# PostgreSQL Session Vars Reset All GUCs On Checkout

Category: Performance, Operations
Severity: Medium

## Finding

When request session variables are used, PostgreSQL connection checkout resets all session state with `RESET ALL`, then applies every configured variable. This is safe for avoiding leaked request state, but it is heavy for hot paths and can erase intentional connection-level settings.

## Evidence

- `crates/umbral-core/src/db.rs:517-595` configures PostgreSQL pool acquisition and request GUC handling.
- `plugins/umbral-auth/src/session_user.rs` uses route context session variables for authenticated user state.
- `plugins/umbral-rls/src/lib.rs` depends on PostgreSQL session variables for RLS policies.

## Risk

Every pool checkout in an RLS/session-var path pays extra round trips or work. `RESET ALL` can also clear non-request GUCs that an app or extension expected to keep for the connection lifetime.

## Recommendation

Prefer targeted cleanup:

- Track the specific GUC names Umbra owns and reset only those.
- Consider transaction-local `set_config(..., true)` where feasible.
- Add a benchmark for request throughput with and without session vars.
- Document the interaction with application-managed PostgreSQL GUCs.

## Suggested Tests

- Session vars do not leak between requests.
- Non-Umbra connection settings survive checkout when they should.
- RLS request throughput does not regress beyond an agreed threshold.

