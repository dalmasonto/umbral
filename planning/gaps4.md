# gaps4 — review_3 framework sweep (claude + codex)

Findings from the full-framework security/correctness/simplicity sweep, consolidated from `planning/review_3/claude/` and `planning/review_3/codex/`. Ordered by severity. Each is verified against the code before fixing — a finding that turns out to be intended or already-guarded is marked `[~]` with the rationale, not silently dropped.

Status: `[ ]` open · `[x]` fixed · `[~]` reviewed, no change (with reason) · `[>]` deferred (with reason)

Numbers are identifiers within this file. Dedup note: claude C2 == codex #21 (same Postgres readback leak) — tracked once, as #2.

---

## CRITICAL

1. [x] **Admin inline cell-edit → superuser escalation.** FIXED: `update_one` now default-denies unauthorized `privileged` + `noform`/`noedit` columns (errors, not silent no-op); `cell_edit_post` rejects them for a clean 403. Regression tests `update_one_refuses_privileged_by_default` / `_honors_privileged_when_authorized`. `cell_edit_post` (umbral-admin/handlers/inline_edit.rs) gates only on `readonly_fields`; `DynQuerySet::update_one` (umbral-core/orm/dynamic.rs:1215) is the one dynamic write terminal that skips the `privileged`/`noform` mass-assignment guard. A staff user with `Change` on `auth_user` POSTs `is_superuser=true` to their own row. Fix at both altitudes: guard `update_one`, and reject privileged/noform/noedit in the handler. (claude C1; codex overlaps via #21 family)

2. [x] **`Masked<T>` backup restore double-seals.** FIXED: added the write-side `DynQuerySet::presealed()` — the twin of `unredacted_for_backup()` — threaded through `build_insert_plan` to bind Masked values verbatim on restore instead of re-sealing. `load_fixture` sets it. Regression test `presealed_insert_stores_ciphertext_verbatim_not_double_sealed`. (claude C-masked)

---

## HIGH

3. [x] **Secret/private/Masked columns leak in the Postgres write-back.** FIXED: all three PG response loops in dynamic.rs now `.filter(|c| self.may_serialize(c))`, mirroring their SQLite twins. PG-only path — verified by symmetry with the tested SQLite arm (no local PG). (== codex #21, also closed) `insert_json` / `insert_json_in_tx` / fetch-one-json twin filter through `may_serialize` on the SQLite arm, iterate unfiltered on the Postgres arm (orm/dynamic.rs:1822/1966/2130). Postgres-first → production leaks. Fix: filter all three PG arms; extract one shared row-serializer. (claude C2 == codex #21)

4. [x] **REST list filter/search is a blind extraction oracle over hidden columns.** FIXED: the list handler now builds a `queryable_fields` set (`model.fields` minus `cfg.is_field_hidden`) and drives `parse_filters` / `parse_search` / `parse_ordering` off it — the same visibility predicate the response serializer uses. A hidden column is now an unknown-field 400, and the enumerated valid-fields list no longer discloses secret column names. Regression tests `hidden_column_cannot_be_used_as_a_filter_oracle` / `_for_ordering`. (claude C3) `parse_filters`/`parse_search` (umbral-rest/filtering.rs, lib.rs:3061) validate a key against the full field list with no visibility check → `?password_hash__startswith=…` leaks via row count, anonymously under the `ReadOnly` default; the error even enumerates secret column names. Fix: build the filter/search/order surface over reader-visible columns only. (claude C3)

5. [x] **`Masked<T>` admin edit crypto-shreds the secret.** FIXED: `build_update_form_query` (the shared form-update builder behind both `update_form` and `update_form_in_tx`) now skips a Masked column whose submitted value is empty or the redaction marker — "leave the stored secret unchanged", honoring the same contract `Masked::Deserialize` does. Regression test `empty_form_value_leaves_masked_ciphertext_unchanged`. (claude C-masked)

6. [~] **`Masked<T>` REST read-modify-write double-encrypts.** REVIEWED — no code change. The default dynamic REST path is already safe: the derive auto-marks Masked `secret` (macros lib.rs:1310), so `may_serialize` strips it from every response — the client never receives ciphertext to echo back. The residual is the hand-rolled `Json(typed_model)` path: typed `Serialize` emits ciphertext *by deliberate, tested design* (masked.rs test asserts `!= REDACTED`; the doc directs you to `.hide()` it), and echoing that back re-seals. Changing `Serialize` would fight an intentional decision; the in-process typed update stays safe (Sealed clones through). Residual risk noted, not a default-path bug. (claude C-masked)

7. [x] **Dynamic IN filters fail open.** FIXED: `filter_in_strings` / `filter_m2m_contains_any` now `fail_closed()` (push `never_matches`) when values were supplied but all failed coercion — the admin bulk delete/restore/hard-delete path could otherwise drop the PK predicate and hit every row. Empty-input stays a no-op. Regression test `filter_in_strings_all_invalid_values_fail_closed`. (codex #22)

8. [x] **OAuth routes require sessions but the plugin declares only `auth`.** FIXED: `OAuthPlugin::dependencies()` now returns `&["auth", "sessions"]`. The framework already errors at `App::build()` on a declared dependency that names no registered plugin, so this converts a runtime sign-in failure into a clear boot error. (codex #01) Missing dependency → login flow breaks or state is lost at runtime with no boot error. Fix: declare the dep + boot check. (codex #01)

9. [>] **GraphQL mutations lack object-level scope.** DEFERRED — feature-sized, needs its own focused PR with two-user tests. The fix: an `owned_by(table, owner_col)` / object-scope API on `GraphqlPlugin` mirroring REST's, threaded into `Exposed`, applied in `mutation.rs` update/delete by adding `.filter_eq_string(owner_col, caller_pk)` to the WHERE (resolvers currently filter by PK only, after the identity-based `guard_write`). Interim mitigation, already available: protect mutable GraphQL models with Postgres RLS (`umbral-rls`), which scopes the UPDATE/DELETE at the DB layer. Half-shipping a row-authorization control is worse than a clear gap. (codex #02)

10. [x] **GraphQL has no depth/complexity budget.** FIXED: `schema::build` now applies `limit_depth(12)` + `limit_complexity(500)` by default (conservative, ON out of the box), overridable via `GraphqlPlugin::max_depth` / `max_complexity`. async-graphql rejects an over-budget query at validation, before any resolver runs. Regression tests `a_query_deeper_than_the_budget_is_rejected` / `a_normal_shallow_query_is_within_the_budget`. (codex #05; claude triage graphql/schema.rs:439/456)

11. [x] **Tenant inverse mode can share forgotten tables.** FIXED (make-visible): `on_ready` now logs the tenant-owned app set at boot under `tenant_apps` mode so the shared/tenant split is auditable, and the `tenant_apps` doc's "forgotten → shared is safe" claim is corrected — it's only safe if the app has no per-tenant data. A blanket require-classification error would break inverse mode's ergonomics (unlisted=shared is the design), so this surfaces the risk without changing routing. (codex #09)

---

## MEDIUM

12. [ ] **GraphQL subscription/SSE context misses auth + private-field unlocks** that the POST path carries. (codex #03)
13. [ ] **GraphQL child loader applies one global relation limit** across all parents → truncated relations. (codex #04)
14. [ ] **Admin M2M writes are not atomic with the parent save** → orphaned/partial state on failure. (codex #06)
15. [ ] **Bearer auth writes `last_used` on every request** → a write on every authenticated read. (codex #07)
16. [ ] **PG route-context resets ALL GUCs on checkout** instead of only umbra-owned ones. (codex #08)
17. [x] **Storage media is public by default** unless an access policy is set. FIXED: `on_ready` now emits a boot warning when media is served with no `media_access` gate — the same posture REST takes for anonymous-readable resources — so the public-vs-private call is deliberate. (codex #10)
18. [ ] **Custom storage backend still mounts a local `ServeDir`.** (codex #11)
19. [ ] **Realtime Redis broker uses an unbounded internal queue** → memory growth under backpressure. (codex #12)
20. [x] **REST rustdoc still describes the old unsafe defaults** (pre safe-by-default). FIXED: the module `## Auth` section rewritten — it claimed "every exposed route is open" when the default is `ReadOnly` (writes 403 until opted in). Now documents the real posture: ReadOnly default, hard-denied/secret/private stripping, no-store, object scopes, throttles, boot warnings. (codex #13)
21. [x] **Redis cache `clear()` uses `FLUSHDB`** → wipes co-tenant keys in a shared Redis. FIXED: keys are namespaced under a prefix (`umbral:cache:`, overridable via `connect_with_prefix`), and `clear()` now `SCAN`s `<prefix>*` + `UNLINK`s in batches instead of `FLUSHDB`. Compile-verified (no local Redis). (codex #15)
22. [ ] **Analytics auto-pageviews send raw paths** (may contain tokens/PII in the path). (codex #16)
23. [ ] **Page cache buffers eligible responses with no object-size cap.** (codex #18)
24. [ ] **Postgres-only typed ORM terminals drift from the generic terminal behavior.** (codex #23)
25. [ ] **Raw SQL escape hatch has no bound-param variant and defaults to read routing.** (codex #24)
26. [ ] **Dynamic form inserts assume an i64 PK** (returns 0 for String/Uuid). (codex #25 — overlaps known gap #73)
27. [ ] **`try_for_each` chunked iteration uses OFFSET pagination** (O(n²) on large scans) and overwrites a caller LIMIT. (codex #26; claude triage queryset/mod.rs:1841)

---

## LOW

28. [ ] **Global `OnceLock` registries make multi-app / test isolation brittle.** (codex #14 — note: the ambient pool is intentional per CLAUDE.md; scope to the non-intentional registries.)
29. [ ] **Task enqueue timeout is accepted but never persisted/enforced.** (codex #17)
30. [ ] **Low-level realtime group publish bypasses sender policy checks.** (codex #19)
31. [ ] **Plugin route metadata can drift from actual routes** (affects audit surfaces). (codex #20)
