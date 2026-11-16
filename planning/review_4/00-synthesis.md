# 00-synthesis — review_4 whole-project sweep

**Sweep:** 2026-10-10 · pinned to commit `e0d5e18b` · 28 workspace crates (274k LOC) + repo hygiene · **29 review files, ~202 numbered findings** · point-in-time, new-findings-only policy (see [README](README.md)).

**Standing severity totals:** **1 CRITICAL · 28 HIGH · ~61 MEDIUM · ~81 LOW · ~28 INFO.** Every CRITICAL/HIGH was spot-verified against the cited source by the coordinator: **30 candidates → 29 confirmed at stated severity** (including the one CRITICAL), **1 downgraded** after a cross-agent refutation, plus two refuted *extensions* and two resolved contradictions (full log below).

## The verdict in one paragraph

The framework's centre — the query builder and its decode paths, the migration engine's refusal surface, the web layer's hardening, and the plugin contract — is in better shape than its perimeter. The defect mass concentrates in three places: **typed write terminals that re-implement the `create()` lifecycle without its guards**, **authorization/visibility surfaces in admin/rest/graphql that miss one sibling's rule**, and **the seams where plugins meet core (trait defaults, settings plumbing, dynamic writes)**. None of the top findings are missing features; they are rules that already exist, implemented at call sites instead of primitives, and forgotten exactly where a new sibling appeared. The most dangerous single finding is **STORAGE-1 (CRITICAL)** — arbitrary file read through the gated media proxy. The highest-leverage systemic fix is the one review_1 already recommended and this sweep re-confirms: **make the safe path the default path, structurally** — put the rule in the primitive, make the default deny.

## Cross-crate themes

### 1. Drift is this codebase's signature: one rule, N implementations, fixed in N−1
The recurring failure is not a missing guard — it is a guard that exists on the sibling and not here, because it was patched per-site instead of lifted into one seam. Examples across the sweep: `save`/`upsert` return M2M-unseeded rows while `create()` calls that seeding load-bearing (CORE-2); six typed terminals skip the view-refusal/validation guards their five siblings carry (CORE-9, CORE-10); `RestNestedGate` implements `check_create` but not `check_update`, inheriting the trait's `Ok(())` default while its own comment claims update permission is enforced (REST-2); `fetch_where_in` keeps the flat `LIMIT` its sibling `fetch_children_batch` exists to avoid (GRAPHQL-2); admin inlines pass `inline.readonly_fields` instead of `readonly_set` (ADMIN-1); admin search never got the hidden-column exclusion REST's search got (ADMIN-2); auth's password flows use sync hashing the crate's own gate contract forbids (AUTH-1); session max-age enforcement lives in `read_session` but not in the layer every request goes through (SESS-1); `db_constraint` is absent from both diff trigger lists (CORE-6). **Review_3's own throughline ("a visibility rule enforced in one place, forgotten in a sibling") recurred within two months of being swept** — evidence that per-site patches do not converge. The fix shape that does: one primitive, default-deny, every caller inherits.

### 2. Guards at call sites, defaults that allow
Several core traits default to *permissive* or *inert*, so any new caller that forgets the override silently loses the rule: `NestedWriteGate::check_update` → `Ok(())` (REST-2); `Storage::put` → `Unsupported` (an unimplemented override turns every deferred upload into a silent background failure — STORAGE-3); `TaggedCache::set_tagged_bool_guarded` has an unguarded default (cache out-of-scope note); `DynQuerySet::search`/`filter`/`sort` carry **no visibility policy at all** (ADMIN-2's root, also REST's historic class); `insert_form`/`update_form` have no intrinsic `noform` guard (admin out-of-scope note). The recommended direction for every one of these: the default must refuse, the escape hatch must be explicit and named.

### 3. Comments and docs assert the behavior the code lacks
An unusually productive review lens for this tree: read the comment, check the claim. `decode_agg`'s "Postgres preserves integer widths" (CORE-1 — it widens for SUM/AVG); "dropping the `JoinHandle` aborts the still-running handler task" (TASKS-1 — it detaches); inlines' "no mass-assignment surface to guard" (ADMIN-1); `update_nested`'s "(create/update permission...)" (REST-2); the typegen module's "the model struct serialised by serde" (CORE-7 — the REST wire is column-keyed); the storage proxy's traversal-safe framing (STORAGE-1 — and core's `file_field` doc-comment asserting "the storage layer already blocks path traversal" is false for this path); "the size cap still bounds how much gets buffered" (STORAGE-2 — the buffering precedes the cap). Recommendation: when fixing each, the comment correction ships in the same commit — a false comment is how the next regression gets introduced.

### 4. Silent no-ops and swallowed errors — against the framework's own doctrine
CLAUDE.md says errors are `Result` values plumbing through `?`; the sweep found the opposite in the seams: a failed session save returns success plus a cookie from the fallback path (SESS-2); M2M `add` on a `save()`-returned row is `Ok(())` with no junction written (CORE-2); `merge_dotenv` drops malformed lines via `flatten()` (CORE-52); `to_bytes().unwrap_or_default()` blanks >64KiB error messages before the log and the dev page (CORE web, WEBL-2); cache read paths swallow backend errors while writes log (CACHE-2); the M2M `add` contract itself documents "Returns `Ok(())` when the M2M slot is unattached". Each is a place where the framework's headline promise ("errors you can see") is locally inverted.

### 5. The verification story: why the above shipped
**There is no CI job that runs the workspace test suite** (HYGIENE-1: the only Rust commands in all six workflows are `cargo audit`, one `--ignored` scaffold test, and the website build). Beyond that, the existing tests repeatedly assert the safe-looking path instead of the dangerous one: the max-age test calls `read_session` (the function that has the check) rather than the layer (SESS-1); the timeout test asserts row state, not cancellation (TASKS-1); typegen has no renamed-field test (CORE-7's residual); aggregate tests are SQLite-only while the bug is PG-only (CORE-1); transferdata's only test is SQLite→SQLite while resume is dead on PG (CORE-5); no test flips `db_constraint` (CORE-6). The **SQLite-loose / Postgres-strict asymmetry** is a systematic blind spot: several HIGHs exist only because dev and CI run where the constraint is invisible.

### 6. The authorization/visibility cluster — one workstream all of it
The single most concentrated risk is the family of rules about *who sees / writes what*, enforced unevenly: `.action_by` GET resolves without object-scope (REST-1); nested child UPDATE default-allows the child's permission (REST-2); the resolved-row echo bypasses every output strip (REST-4); admin inline writes accept any skip-listed column (ADMIN-1); admin search is a blind oracle over hidden columns (ADMIN-2); the CSRF guard misses the GraphQL WS/SSE transports, and the security exemption is segment-prefix (GRAPHQL-1, SEC-1); login throttle keys skip normalization (AUTH-2); file-key validation misses the dynamic write path (CORE-4/MD-2); the media proxy hands decoded keys to an unvalidating backend (STORAGE-1). Treat this as one workstream with one principle, not nine patches.

## Ranked fix order (recommended)

| # | Item | Why first |
|---|---|---|
| 1 | **STORAGE-1** (CRITICAL) | Arbitrary file read via `/files/%2Fetc%2Fpasswd` on gated FsStorage mounts; fix = validate the decoded key before the backend (and never `join` absolute keys). |
| 2 | **HYGIENE-1** | Add a test+clippy+fmt CI gate before fixing anything else — otherwise every fix below can drift back exactly as review_3's did. Cheapest, highest leverage. |
| 3 | **Authorization batch** — REST-1, REST-2, REST-4, ADMIN-1, ADMIN-2, GRAPHQL-1, SESS-1, AUTH-2 | One workstream, one principle (rule in the primitive, default-deny). Every one is confirmed at source with a concrete exploit/trace. |
| 4 | **Silent data-loss batch** — CORE-2 (M2M seeding), CORE-3 (self-referential soft-delete cascade), ADMIN-3/ADMIN-4 (both password flows dead), TASKS-1 (timeout doesn't cancel) | Writes reported successful that didn't happen, or flows that cannot complete. |
| 5 | **First-run / PG breakage** — CORE-1 (aggregates on PG), CORE-5 (transferdata resume on PG), CORE-6 (`db_constraint` diff blind), RLS-1 (INSERT policies invalid DDL), TASKS-2 (slim build fails) | Documented features unusable on the primary backend or first touch. |
| 6 | **Availability** — RT-1 (aborted-handshake registration leak), STORAGE-2 (unbounded buffering), STORAGE-3 (deferred uploads all fail under `.accept`) | Unauthenticated or ambient paths that take the process down or break whole features. |
| 7 | **Primitive refactors** — visibility policy into `DynQuerySet`; `noform` into the form writers; `NestedWriteGate::check_update`/`Storage::put` defaults to deny/unimplemented-error-with-effect; `session_layer` routing through the guarded read | Structural; do once, retire a whole drift class. |
| 8 | The MEDIUM parity batch (CORE-9…CORE-13), drift corrections (CORE-15/16), docs/comment corrections in the same commits as their fixes | Second tier. |

## Verification log (CRITICAL/HIGH)

All confirmed against cited source unless noted. IDs are as published in the crate files (core = final `CORE-n`).

| Finding | Crate | Verdict |
|---|---|---|
| CORE-1 (QW-1) PG aggregate decode | core | ✅ confirmed (sqlx `type_compatible == self == other`) |
| CORE-2 (QW-2) M2M seeding on save/upsert | core | ✅ confirmed |
| CORE-3 (MD-1) self-referential cascade | core | ✅ confirmed |
| CORE-4 (MD-2) storage-key guard vs dynamic path | core | ✅ confirmed (HIGH, defense-in-depth; storage review later refined the backstop picture) |
| CORE-5 (MIG-1) transferdata resume on PG | core | ✅ confirmed (INT4 ≠ INT8 strict decode) |
| CORE-6 (MIG-2) `db_constraint` invisible to diff | core | ✅ confirmed |
| CORE-7 (RUN-1) typegen wire name | core | **⬇ downgraded HIGH → MEDIUM** — cross-agent refutation: REST wire is column-keyed and the emitted key matches it; residual = false module-doc claim + typed-vs-dynamic divergence (see the finding's coordinator note) |
| TESTING-1 `boot()` drops later configure closures | umbral-testing | ✅ confirmed |
| MACROS-1 `#[derive(Dto)]` ignores `#[serde(flatten)]` | macros | ✅ confirmed |
| AUTH-1 sync hashing in async handlers | auth | ✅ confirmed |
| AUTH-2 throttle keys bypass normalization | auth | ✅ confirmed |
| SESS-1 `max_session_age` missing from the layer | sessions | ✅ confirmed |
| SESS-2 failed save returns success + cookie | sessions | ✅ confirmed |
| RT-1 aborted-handshake registration leak | realtime | ✅ confirmed |
| TASKS-1 timeout never cancels | tasks | ✅ confirmed |
| TASKS-2 feature-gate regression (slim build) | tasks | ✅ confirmed |
| REST-1 `.action_by` skips object-scope | rest | ✅ confirmed |
| REST-2 nested UPDATE default-allows permission | rest | ✅ confirmed |
| REST-4 resolved-row echo skips output strips | rest | ✅ confirmed |
| ADMIN-1 inline skip-set | admin | ✅ confirmed |
| ADMIN-2 changelist search oracle | admin | ✅ confirmed |
| ADMIN-3 change-password always 400s | admin | ✅ confirmed |
| ADMIN-4 create-with-password drops the hash | admin | ✅ confirmed |
| STORAGE-1 (CRITICAL) media proxy LFI | storage | ✅ confirmed |
| STORAGE-2 unbounded buffering under `.accept` | storage | ✅ confirmed |
| STORAGE-3 deferred uploads fail under `.accept` | storage | ✅ confirmed |
| GRAPHQL-1 CSRF guard misses WS/SSE | graphql | ✅ confirmed (bounded by documented `SameSite=None` config) |
| GRAPHQL-2 flat loader limit | graphql | ✅ confirmed |
| RLS-1 INSERT policies emit invalid DDL | rls | ✅ confirmed |
| HYGIENE-1 no test CI | repo | ✅ confirmed |

**Refuted extensions (honest negatives):** the typegen finding's openapi "twins" don't exist (openapi mirrors the column-keyed wire correctly — `umbral-openapi.md` out-of-scope #1); WEBL-3's storage reachability is refuted (no stored content-type reaches a response builder — `umbral-storage.md` out-of-scope #2).

**Contradictions resolved in-file:** CORE-25/CORE-49 (missing `model.table.unique` check vs stale docs — verified complementary against the 15-id catalogue; coordinator note added); CORE-7 (see above).

## Unrouted cross-crate notes (targets already reviewed — for the triage pass)

- `crates/umbral-core/src/auth_contract.rs` `FnAuthentication` doc example cannot compile against the real auth API (uninferable `U`) — core edit; flagged by both the auth review and core slice 5.
- `crates/umbral-core/src/signals.rs`: async subscribers awaited in series under one 30s cap (write-path latency amplifier), and no per-subscription `unsubscribe` — from the tasks/signals review.
- `crates/umbral-core/src/cache.rs`: `TaggedCache` read/guarded defaults (bust_epoch → 0, unguarded default store) violate the guard silently for non-overriding consumers — from cache review.
- `crates/umbral-core/src/app.rs:1513-1517`: BUG-20 comment stale ("picks the first" vs deliberate last-write-wins merge) — verified from the openapi side.
- permissions-to-sessions/auth compensations (`current_user_id_str` handling) — **verified held; do not double-report** (from sessions/oauth/permissions cross-checks).
- Fix-verification targets: `examples/shop` and `examples/derive-demo` are live consumers of the ADMIN-3/4 password flows; `crates/umbral-macros` `#[task]` expansion is un-reviewed territory.
- Doc-vs-reality residues: `planning/REAL-GAPS.md:35` (rollback-per-test), `arch.md:101` (`test_with_pool`), `arch.md:65-66` + `docs/decisions/2026-05-30` (escape-hatch table), `documentation/.../testing/setup.mdx:26` (boot's dropped-closure claim) — hygiene HYGIENE-8/9 + facade UMBRAL-2/U-notes.

## What happens next (recommendation)

1. **Triage**: convert accepted findings into tracker entries (a `gaps7.md`, or additions to `gaps6.md`) — this review is point-in-time and does not self-track. Unaccepted ones stay in the files as documented leads.
2. **Fix order** as ranked above; **CI gate first**.
3. For each fix: ship the test named in the finding, and correct the comment/doc that lied — the two habits this sweep shows are load-bearing.
4. Keep the pattern that worked: verification inside each reviewer + coordinator spot-checks; sized to finish (review_3's fan-out died on budget — this one didn't).
