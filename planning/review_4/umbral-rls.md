# review_4 · umbral-rls

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 969 LOC (`plugins/umbral-rls`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 1 high · 2 medium · 1 low · 0 info — 4 new, 2 known |

## Summary

- `RlsPlugin` renders and applies Postgres row-level-security DDL: `ENABLE` + `FORCE ROW LEVEL SECURITY` per table, a `pg_policies` reconcile that drops undeclared policies, and one `DROP POLICY IF EXISTS` + `CREATE POLICY` per declared policy (`src/lib.rs:393-477`).
- The audit_2 hardening held up: FORCE ships with its rationale in-code, SQLite is a hard error under `Environment::Prod`, the reconcile fixed the append-only staleness, and ORM-only is respected (the only raw SQL is the sanctioned policy DDL plus the `pg_policies` catalog read).
- **The INSERT path cannot work at all (RLS-1, HIGH).** `render_policy_sql` always emits `USING (...)`; Postgres rejects `USING` on an INSERT policy (even when `WITH CHECK` is present), so *any* `.policy(..., Action::Insert, ...)` — including the crate's module example, its documented "read but not write" recipe, and its PG test — makes `App::build()` fail. No builder can express an insert-only policy.
- Applying is not transactional (RLS-2), so the boot that fails on a bad (or INSERT) policy has already dropped the previous policy from the live DB; and policies only ever reach the schema/database the boot pool resolves (RLS-3), so the `umbral-tenants` pairing advertised in `planning/features.md:310` protects nothing per tenant.
- Docs and comments drifted from shipped behaviour in several places (RLS-4) — most consequentially the INSERT/`WITH CHECK` semantics paragraph, which is simply false.

## Findings

### RLS-1 · HIGH · correctness · self-verified
**Every declared INSERT policy produces invalid DDL — `render_policy_sql` always emits a `USING` clause, and Postgres rejects `USING` on `FOR INSERT` even when `WITH CHECK` is present — so the INSERT action class is unusable and any app that declares one fails `App::build()`.**

- **Where**: `plugins/umbral-rls/src/lib.rs:342-359` (`render_policy_sql`), `51-55` (`Policy` doc), `20-23` (module example), `166-176` ("read but not write" recipe), `565-578` (unit test); `plugins/umbral-rls/tests/integration.rs:80-86`.
- **Evidence**:
  ```rust
  let mut create_sql = format!(
      "CREATE POLICY \"{}\" ON \"{}\" FOR {} USING ({})",
      escape_ident(&p.name),
      escape_ident(&p.table),
      p.action.sql_keyword(),
      p.using
  );
  if let Some(check) = &p.with_check {
      create_sql.push_str(&format!(" WITH CHECK ({check})"));
  }
  ```
  `USING` is unconditional; `with_check` only appends. Postgres: "An `INSERT` policy cannot have a `USING` expression" (CREATE POLICY docs); the implementation rejects it even when `WITH CHECK` is also present — `src/backend/commands/policy.c`: `polcmd == ACL_INSERT_CHR && stmt->qual != NULL` → `ERROR: only WITH CHECK expression allowed for INSERT` (SQLSTATE 42601). The same grammar makes `policy_with_check(..., Action::Select | Action::Delete, ...)` invalid ("A `SELECT` policy cannot have a `WITH CHECK` expression"; likewise DELETE). The `Policy` doc asserts the opposite of both facts:
  ```
  /// optional `WITH CHECK` clause (for INSERT/UPDATE). When
  /// `with_check` is `None` and the action is INSERT or UPDATE,
  /// Postgres uses `USING` for `WITH CHECK` automatically.
  ```
  (The auto-fallback exists only for `ALL`/`UPDATE`.) The module example (20-23), the read-but-not-write recipe (166-176: `policy_with_check("post", ..., Action::Insert, "true", "EXISTS (...)")`) and the PG integration test (80-86) all use the broken INSERT shape; the unit test (565-578) asserts only `contains("FOR INSERT")` / `contains("WITH CHECK")`, so it codifies the invalid statement rather than catching it.
- **Impact** (traced; PG grammar verified against PostgreSQL docs + `policy.c`, no runtime execution): an app writes the crate's own advertised recipe — `.policy("post", "user_can_create", Action::Insert, "user_id = current_setting('app.user_id')::int")` — then boots. `on_ready` → `block_on_ready(self.apply_policies(pool))` → `ENABLE`/`FORCE` succeed → `drop_undeclared_policies` runs → `DROP POLICY IF EXISTS "user_can_create" ON "post"` → `CREATE POLICY ... FOR INSERT USING (...)` → PG 42601, which propagates up and fails `App::build()`. The insert-only rule the developer asked for cannot be expressed at all — INSERT is one of the five actions the API names, and half the documented examples in the crate are unwriteable. HIGH, not CRITICAL: the failure is loud at boot (nothing silently grants), but a whole reachable action class plus the crate's own docs are broken. (RLS-2 makes the failed boot worse: the previous policy was already dropped.)
- **Fix**: branch the renderer by action — `Insert` → `FOR INSERT WITH CHECK (<with_check or using>)` and never emit `USING`; `Select`/`Delete` → `USING` only, rejecting a set `with_check`; `All`/`Update` unchanged. Fix the `Policy` doc (drop the false auto-fallback sentence), the module example, the 166-176 recipe, `rls.mdx`'s quickstart, and the unit test (assert the exact statement per action). Un-ignoring the PG test then exercises something that can pass.

### RLS-2 · MEDIUM · correctness/ops (non-transactional apply)
**Policies are applied DROP-then-CREATE in autocommit with no transaction, so a failed boot has already mutated the live database — and rolling back the deploy does not restore it.**

- **Where**: `plugins/umbral-rls/src/lib.rs:393-436` (`apply_policies`), `443-477` (reconcile runs first), `400`/`407` (ENABLE/FORCE commit before the loop).
- **Evidence**:
  ```rust
  self.drop_undeclared_policies(pool).await?;
  for policy in &self.policies {
      let (drop_sql, create_sql) = self.render_policy_sql(policy);
      …
      sqlx::query(&drop_sql).execute(pool).await?;
      …
      sqlx::query(&create_sql).execute(pool).await?;
  }
  ```
  Each statement commits on its own (`execute(pool)` on the pool, no `begin`); ENABLE/FORCE and the reconcile have already committed before the first policy is dropped. The doc at `338-341` claims idempotency, not atomicity — the gap is invisible in the docstring.
- **Impact** (traced): deploy a policy set containing a typo'd predicate (or any INSERT policy — RLS-1). The boot fails loudly (good), but the DB is left mutated: `"user_can_read_own"` was dropped and its replacement never created. RLS is enabled **and forced** on that table, so with no covering policy it is default-deny for every non-`BYPASSRLS` role — including the app itself, on every replica still serving the old release during a rolling deploy: a live outage on that table until someone boots a good version. A mid-loop failure additionally leaves some tables on the new definition and others on the old — a half-applied authorization state. MEDIUM: reachable, bounded, loud at the trigger, and the next good boot repairs it; not HIGH because no path silently *grants* access; not LOW because the damage outlives the failed process.
- **Fix**: wrap the whole apply in one transaction — Postgres DDL is transactional: `let mut tx = pool.begin().await?;` … execute ENABLE/FORCE, reconcile, DROP, CREATE with `&mut *tx` … `tx.commit().await?`. A failed boot then leaves the prior policy set exactly as it was.

### RLS-3 · MEDIUM · architecture/drift (tenant interaction)
**Policies only ever land in the schema/database the boot pool resolves (`public`), so the features.md-advertised `umbral-tenants` pairing (schema- or database-per-tenant) leaves every tenant's data uncovered — silently in the mixed cases.**

- **Where**: `plugins/umbral-rls/src/lib.rs:361-379` (`render_enable_sql`/`render_force_sql` interpolate a bare table name), `290-330` (`on_ready` uses `pool_dispatched()` — the default pool only), `455` (schema-blind `pg_policies` query); pairing advertised at `planning/features.md:310` ("Row-level isolation via `umbral-rls`"); sibling `plugins/umbral-tenants` (schema mode routes via search_path; database mode routes to per-tenant pools).
- **Evidence**:
  ```rust
  pub fn render_enable_sql(&self, table: &str) -> String {
      format!(
          "ALTER TABLE \"{}\" ENABLE ROW LEVEL SECURITY",
          escape_ident(table)
      )
  }
  ```
  and in `on_ready`: `let pool = umbral::db::pool_dispatched();` (291) — the only pool ever used. The reconcile is schema-blind too:
  ```rust
  sqlx::query_scalar("SELECT policyname FROM pg_policies WHERE tablename = $1")
  ```
  (matches a same-named table in *every* schema; the DROP then resolves via search_path — the comment at 451-453 assumes these are the same table).
- **Impact** (traced): schema mode with a tenant-owned table that exists only in tenant schemas — boot runs `ALTER TABLE "post" …` against default search_path `public` → "relation does not exist" → **loud** boot failure (the operator learns, confusingly, that a tenants app cannot use RLS). If `post` also exists in `public` (early unfiltered `migrate`, or a table shared between both worlds), boot succeeds but only `public.post` is enabled/policied; tenant-scoped requests are schema-qualified to `tenant_x.post` by `umbral-tenants`, so the queries that actually run are unprotected — **silent**. Database mode: `register_tenant_database` migrates tenant apps into per-tenant pools and never calls `apply_policies`, so tenant databases get no policies at all — silent. Nothing warns when both plugins are registered, and `rls.mdx` never mentions tenancy. MEDIUM: an opt-in combination, advertised in features.md, silent in exactly the mixed cases that look configured; not HIGH because pure schema-mode use fails loudly at boot.
- **Fix**: at minimum, detect the combination at boot (tenants plugin registered alongside RLS) and warn/refuse, and document that RLS as shipped covers only the default schema/pool. The real fix is per-tenant application: policies applied once per tenant schema (schema mode) and once per registered tenant pool (database mode).

### RLS-4 · LOW · docs/tests drift
**Module docs, an inline comment, and the integration-test comment describe behaviour this crate no longer has — and `rls.mdx` repeats two of them.**
- **Where**: `src/lib.rs:28-36` (module "Backend gating": "The plugin doesn't refuse to boot against SQLite — RLS policies are simply silently skipped"), `390-392` (`apply_policies` claims `tokio::runtime::Handle::current().block_on()`), `tests/integration.rs:128-134` ("declarations are append-only across boots"); `documentation/docs/v0.0.1/plugins/rls.mdx:174-193` ("does not refuse to boot… silently skipped") and `195-209` ("policies are **not** removed when you delete them").
- **Evidence**: shipped `on_ready` contradicts the module doc:
  ```
  if is_prod {
      return Err(Box::<dyn std::error::Error + Send + Sync>::from(
          "umbral-rls: Row-Level Security is Postgres-only, but the active \
           backend is SQLite — it provides NO row isolation. …
  ```
  (`lib.rs:302-308`); the append-only claim is contradicted by the shipped reconcile (`443-477`, test `undeclared_policy_is_dropped_on_reapply` at 150+), and the `Handle::current().block_on()` comment by `block_on_ready(...)` (`lib.rs:321-326`).
- **Impact**: an operator reading rls.mdx concludes SQLite deployments still boot (false in Prod) and that deleting a `.policy(...)` line needs manual `DROP POLICY` cleanup (unnecessary since audit_2 R5). Fix: one docs/comment sweep — `lib.rs:28-36`, `390-392`, the test comment at 128-134, and the two `rls.mdx` sections.

## Strengths

- FORCE-with-rationale is in the code (`lib.rs:370-379`, `401-407`), tying the DDL directly to audit_2 C2/R1 — how a security backstop should carry its history.
- Fail-closed backend posture is explicit: SQLite is a hard boot error in `Environment::Prod` and a loud warn in Dev (`293-318`), with dedicated suites (`tests/sqlite_prod_fail.rs`, `tests/on_ready_runtime.rs`).
- Identifiers are quoted through `escape_ident` for every policy/table name, with a unit test; predicate bodies are documented as verbatim developer SQL (`71-90`) — the accepted R6 posture, tracked for typed builders.
- The reconcile (`drop_undeclared_policies`) properly fixes the "stale permissive policy keeps granting" class, with a behavioural PG test; `render_*`/`apply_to` are public so tests can drive the exact DDL.
- ORM-only rule respected: the sole `sqlx::query` hits are the sanctioned policy DDL statements plus the `pg_policies` catalog read the reconcile needs.

## Clean areas checked

- **SQLite/PG divergence**: no silent fallback path — non-PG is warn-or-refuse, never a fake guarantee.
- **Session-var lifecycle under pooling** (routed lead): verified held elsewhere — the session variable predicates reference is set per acquire by core's `pg_pool_options().before_acquire` hook with a scoped reset (`crates/umbral-core/src/db.rs:578-620`), wired via `AuthPlugin::with_db_session_var` (gaps3 #44/#45). This crate only names the var inside predicate strings.
- **Identifier injection**: every identifier escaped; values cannot be bound in DDL — documented, not a new finding.
- **`unsafe`/panics**: zero `unsafe`; no `unwrap`/`expect` on the apply path.
- **`on_ready` runtime bridging**: uses the shared `block_on_ready` helper (handles multi-thread/current-thread/no-runtime) rather than a bespoke `block_on` — the stale comment (RLS-4) is only a comment.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps5 #17` — typed policy builders + lint for raw policy SQL bodies `[open]`
- `audit_2 R4` — the only PG enforcement test is still `#[ignore]`d (`tests/integration.rs:44`) `[open]` — note: given RLS-1 it could not pass if un-ignored as written (its INSERT policy at 80-86 is invalid DDL), so it proves nothing today either.
- `audit_2 R1` (ENABLE without FORCE) `[fixed/verified held]`; `R2` (no session-var plumbing) `[fixed/verified held]`; `R3` (SQLite silent skip) `[fixed/verified held]`; `R5` (append-only policies) `[fixed/verified held]`; `R6` (verbatim predicate SQL) `[fixed/verified held]` (accepted + documented; superseded by gaps5 #17).
- `review_1 AUTH-3` — RLS policies with no context mechanism `[fixed/verified held]` (gaps3 #44/#45).

## Out-of-scope notes

- `plugins/umbral-tenants` — tenant side of RLS-3 (RLS covers only the default schema/pool; schema and database modes leave tenant tables uncovered); cross-ref `review_4/umbral-tenants.md`.
- `crates/umbral-core/src/db.rs` — `pg_pool_options().before_acquire` is the session-var plumbing RLS depends on (verified held; owned by the core review).
- `planning/features.md:310` — the multi-tenancy entry advertises "Row-level isolation via `umbral-rls`"; RLS-3 shows the pair as shipped does not deliver it.
