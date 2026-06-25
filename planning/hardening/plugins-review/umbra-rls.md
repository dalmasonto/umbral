# umbral-rls — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbral-rls/src/lib.rs` (435 lines, single file) + `tests/integration.rs`. Citations are `file:line` against real code. Nothing modified. NET-NEW findings marked **NEW**.

## Verdict

**REAL, not a stub — but minimal-scope and Postgres-only.** The plugin genuinely emits and applies Postgres RLS DDL: `ALTER TABLE … ENABLE ROW LEVEL SECURITY` per table and `DROP POLICY IF EXISTS … ; CREATE POLICY … FOR <action> USING (<expr>) [WITH CHECK (<expr>)]` per declared policy, executed against the live `PgPool` in `on_ready` (`lib.rs:310-338`). The builder API (`enable_on`, `policy`, `policy_with_check`), the DDL renderer (`render_policy_sql`, `render_enable_sql`), identifier escaping, and the SQLite skip-with-warn path are all implemented and unit-tested. There is **no `todo!()`, no `unimplemented!()`, no no-op placeholder.** It does exactly what its docstring claims.

What it is **not**: it is *not* a tenant-scoping or request-context framework. It is a thin, honest declarative wrapper over hand-written Postgres policy expressions. The app author writes the `USING`/`WITH CHECK` SQL verbatim (referencing `current_setting('app.user_id')` etc.); the plugin only manages the lifecycle of applying those policies at boot. The crucial missing half — *who sets `app.user_id` per request, and when* — is **entirely the application's responsibility and is not provided, documented, or wired anywhere.** That makes the plugin functionally real but operationally incomplete for the multitenancy use case it implies.

**Completeness one-liner:** the DDL-emission half of RLS is real and works; the request-scoped `SET LOCAL app.user_id = …` half (without which every policy referencing `current_setting('app.user_id')` errors or denies on a fresh connection) is **absent** — this is the gap that makes RLS actually enforce per-request, and it ties directly into the open #69 request-routing-context work.

## Completeness

| Capability | umbral-rls | Status |
|---|---|---|
| `ALTER TABLE … ENABLE ROW LEVEL SECURITY` | `render_enable_sql` + `apply_policies` | ✅ real |
| `CREATE POLICY … FOR {SELECT/INSERT/UPDATE/DELETE/ALL}` | `render_policy_sql`, `Action` enum | ✅ real |
| `USING (…)` clause | `Policy.using` | ✅ real |
| `WITH CHECK (…)` clause | `Policy.with_check` / `policy_with_check` | ✅ real |
| Idempotent re-apply | `DROP POLICY IF EXISTS` before `CREATE` | ✅ real |
| Identifier escaping (name/table) | `escape_ident` (`"`-doubling) | ✅ real |
| SQLite graceful skip | `on_ready` match arm + `tracing::warn` | ✅ real |
| Postgres backend gating | `match pool_dispatched()` | ✅ real |
| **Per-request session var injection** (`SET LOCAL app.user_id`) | — | ❌ **MISSING** (the operational keystone) |
| **`FORCE ROW LEVEL SECURITY`** (apply RLS to table owner too) | — | ❌ missing |
| **`RESTRICTIVE` vs `PERMISSIVE`** policy kind | — | ❌ missing (always PERMISSIVE) |
| **`TO <role>`** role targeting | — | ❌ missing (policies apply to all roles) |
| Policy removal / revoke across boots | — | ⚪ by-design append-only (`tests/integration.rs:128-134`) |
| `migrate`-tracked policy DDL | — | ⚪ applied in `on_ready`, not the migration engine |

The four missing items below the line are all real RLS features Postgres supports that the plugin doesn't model. The first (per-request session var) is the operationally critical one.

No stub code. The only "skip" is the documented SQLite no-op (`lib.rs:243-251`), which is the correct backend-gating pattern per CLAUDE.md, not a hidden gap.

## Findings

### NEW — Completeness / operational

- **NEW · Important (completeness) · whole plugin — no per-request session-variable mechanism.** Every realistic policy body the docs show references `current_setting('app.user_id')` (`lib.rs:21-25, 70-72, 393`). But **nothing in the plugin or framework sets `app.user_id` on the connection per request.** Postgres `current_setting('app.user_id')` raises `ERROR: unrecognized configuration parameter` unless the var was `SET`/`SET LOCAL` on that exact connection earlier in the transaction. With a pooled connection and no middleware doing `SET LOCAL app.user_id = $1` at request start, the policies either (a) error every query, or (b) if the operator adds `current_setting('app.user_id', true)` (missing-ok form) they silently return NULL → the predicate is false → **every row is hidden / every write denied.** So the plugin as shipped will, in the common case, make a table unreadable rather than tenant-scoped. The missing piece is a request-scoped "set the tenant/user var on the connection inside the request's transaction" hook — which is exactly the request-routing-context primitive #69 / #60 describe. Fix: either ship a `RlsContextLayer` middleware that `SET LOCAL`s a configured var from the resolved identity at request start, or document loudly that the app must do this itself and that policies must use the missing-ok `current_setting(name, true)` form. → fold into **#69** (request-scoped routing/tenant context) with a permissions/rls cross-ref; add a doc-callout now.

- **NEW · Important (correctness on re-declare) · `apply_policies` / `tests/integration.rs:117-147` — policies are append-only across boots with no reconciliation.** The plugin only `DROP`s policies it is *about to recreate* (`lib.rs:322-336`). If boot N declares policy `p` on table `t` and boot N+1 *removes* `p` from the builder, the stale `p` **stays live on the table** and keeps enforcing — the test even asserts this as intended (`tests/integration.rs:128-134`). For an access-control feature this is a real footgun: deleting a policy from your code does **not** remove it from the DB, so a policy you believe you revoked still gates rows. Django-style "the declaration is the source of truth" would diff and drop orphans. Fix: track applied policy names (a small `umbral_rls_policy` ledger, or query `pg_policies` for plugin-managed names by a name prefix) and drop any live policy no longer declared. → **NEW gap** (rls policy reconciliation / orphan-drop).

- **NEW · Optional (correctness) · `render_policy_sql:285-294` — `WITH CHECK` is not identifier-escaped *and* `with_check` is interpolated without the documented injection caveat being enforced.** Both `using` (`:290`) and `with_check` (`:293`) are interpolated verbatim — which is the *documented, intended* contract (developer-authored SQL, no binding possible in DDL; the docstrings at `lib.rs:74-92, 158-164` are thorough and the security review already cleared this as author-trusted). Noting only that `WITH CHECK` interpolation uses a bare `format!` (`:293`) rather than going through the same code path as `using`, so a future refactor that adds validation to `using` could miss `with_check`. Cosmetic; no defect. → no gap.

- **NEW · Optional · `lib.rs:240-267` (`on_ready` async bridge) — bare `Handle::current().block_on` panics under `#[tokio::test]`.** The docstring (`lib.rs:43-46`) openly admits this. The sibling `umbral-permissions` plugin uses the *correct* runtime-tolerant form (`block_in_place` + fallback `Runtime`, `permissions/lib.rs:150-163`) and its own comment calls out that rls does it the panicking way. This is why the PG integration test path can't be a normal `#[tokio::test]` exercising `on_ready` — the SQLite skip test works only because it never reaches `block_on`. Fix: adopt the permissions plugin's bridge, or (better) a shared `umbral::plugin::block_on_ready(fut)` framework helper so both plugins get the correct form. → fold into the **NEW shared-on_ready-bridge gap** filed from the permissions review.

### Plugin-contract assessment (clean)

- **Facade-only:** ✅ Imports `umbral::plugin::{AppContext, Plugin, PluginError}`, `umbral::web::Router`, `umbral::db::{pool_dispatched, DbPool}`. The only non-facade dep is `sqlx::PgPool` in `apply_policies` — which is correct and unavoidable: this plugin's entire job is Postgres-specific DDL, an explicitly allowed raw-SQL exception (CLAUDE.md narrow-exception #2, "backend-specific features the ORM doesn't model," Postgres-gated). The security review already cleared the raw SQL here (`security.md:19`).
- **Owns migrations:** ⚪ N/A by design — RLS policies are *not* schema in the migration sense; they're applied imperatively in `on_ready`. This is a defensible choice (policies reference runtime session state, not just structure) but means policy DDL is **not** captured in the migration history / audit trail, and is **not** subject to `makemigrations`/`migrate`. Worth a doc note: an operator inspecting `migrations/` sees no record that RLS is active. (Related to the append-only orphan-drop finding above.)
- **No dep cycle:** ✅ Depends only on `umbral` facade + `sqlx` + `tokio` + `tracing` (`Cargo.toml:12-16`). No plugin-to-plugin deps. Cleanest dep graph of any plugin.
- **Backend gating:** ✅ Textbook — `match pool_dispatched() { Sqlite => warn+skip, Postgres => apply }` (`lib.rs:242-267`), exactly the CLAUDE.md-prescribed shape. The SQLite branch is a genuine skip-with-warn, not a silently-diverging fallback.

## Tests

**Coverage: good for what's implemented; the operationally-critical path is untestable as written.**

`src/lib.rs` unit tests (9 tests, `:350-434`): builder ordering, auto-enable-on-policy, no-duplicate-enable, `render_enable_sql` quoting, drop-then-create rendering, `WITH CHECK` rendering, every `Action` keyword, `escape_ident` quote-doubling. Solid DDL-string coverage.

`tests/integration.rs` (2 tests): (1) SQLite boot skips without failing — runs, passes. (2) **PG round-trip is `#[ignore]`'d** behind `UMBRAL_TEST_POSTGRES_URL` (`:43-44`), so the *only* test that proves `apply_policies` actually creates rows in `pg_policies` and enables `relrowsecurity` **does not run in normal CI.** Everything that exercises real DDL execution is gated behind a manually-set env var.

Test gaps vs findings:

1. **The per-request session-var path is completely untested** (and can't be, because the plugin doesn't provide it) — there is no test that a policy referencing `current_setting('app.user_id')` actually scopes rows for a request that set the var, vs. one that didn't. This is the single most important behavior of an RLS feature and it has zero coverage. The PG test (`:91-115`) only asserts the *policy exists*, never that it *enforces* against real rows under two different `app.user_id` values.
2. **No orphan-policy / reconciliation test** beyond the one that asserts the (undesirable) append-only behavior as intended (`:117-147`).
3. **`escape_ident` is identifier-only** — there is no test (nor possible defense) for a malicious `using` expression, which is by-design out of scope (author-trusted), but the asymmetry (names escaped, bodies verbatim) is untested at the boundary.

Bottom line: the tests prove the plugin *emits correct DDL*; they do not prove RLS *enforces* anything end-to-end (the enforcement test is `#[ignore]`'d and, more fundamentally, the request-scoping half it would need to test doesn't exist).
