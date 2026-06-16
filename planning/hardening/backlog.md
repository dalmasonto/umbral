# Hardening backlog — synthesized

> Deduplicated, severity-ranked synthesis of the 11 review reports in `planning/hardening/{docs-audit,reviews}/`. Each item cites `file:line`, the fix shape, and a gap ref (existing # or **NEW #71+** filed in `gaps2.md`). This is the hardening plan; fixes are focused follow-up PRs, **router #69 + multitenancy first**, then P0 → P1 → P2.

**Totals:** docs audit ~99 findings (~8 Critical); code review: 10 Critical, 40+ Important across 6 lenses. Cross-confirmed signals (found by ≥2 lenses) are flagged ⚑ — fix those first.

---

## Strategic (user-prioritized — do before the rest)
- **#69 — swappable `DatabaseRouter` + multitenancy (schema-per-tenant).** The keystone; fix early while the ORM is malleable. Absorbs #22 (done) + #23. Needs its own brainstorm → spec.
- **#66 — quick win:** document minijinja filters + custom template tags. (And the docs-audit found `helpers.mdx` already omits `static()`/`media_url()`/`highlight_styles()` — fold in.)

---

## P0 — Critical (data loss / corruption / security / broken)

### Concurrency & data integrity (reviews/race-conditions.md) — all NEW
- **⚑ Session `set_data` lost update** — `plugins/umbra-sessions/src/lib.rs:400-456`. Read-modify-write of the whole session JSON; concurrent same-cookie requests drop each other's keys (cart/flash/CSRF). Also flagged by static-analysis (corrupt-data→empty map, no log, `:389,447`). Fix: atomic merge in a transaction / `SELECT … FOR UPDATE`, and log decode failures. → **NEW #71**
- **⚑ `set_user_groups` non-transactional DELETE+INSERT** — `plugins/umbra-permissions/src/membership.rs:77-97`. Empty-membership window = transient privilege loss; every sibling path is already transactional. Fix: wrap in one tx. → **NEW #71**
- **`update_or_create` / `get_or_create` no transaction** — `crates/umbra-core/src/orm/queryset/mod.rs:3886-3996`. SELECT-then-write → duplicate rows (no UNIQUE) or spurious `UniqueViolation`. Fix: tx + UNIQUE backstop + catch-violation; make `add_user_to_group`/`grant_user_permission` catch the UNIQUE backstop (idempotent under races). → **NEW #71**

### Soft-delete on the dynamic path (reviews/correctness-domain.md)
- **⚑ Dynamic path hard-deletes + lists trashed rows (gaps2 #35)** — `crates/umbra-core/src/orm/dynamic.rs:595-611`. `DynQuerySet` never reads `meta.soft_delete` (correctly populated `migrate.rs:411`), so admin/REST `DELETE FROM` permanently destroys data on the 23 soft-delete website models + returns trashed rows. Highest-impact data-loss item.
- **`update_values`/`update_expr` mutate trashed rows (gaps2 #34)** — `queryset/mod.rs:3233, 2937`. #34's line-ref is stale (`~2403`) and misses `update_expr`. Update the entry.
- **NEW soft-delete hole: relation hydration** — `orm/queryset/hydration.rs:654`. prefetch / reverse-FK / M2M child SELECTs have no `deleted_at IS NULL` → return trashed children. Not covered by #34/#35. → fold into **#35** as a 3rd surface.

### Endpoint-reachable unbounded fetches (reviews/performance-scalability.md) — NEW
- **Admin M2M form loads the entire target table** (no LIMIT) on every add/edit render — `plugins/umbra-admin/src/view.rs:511`. The FK picker beside it is paginated; mirror it. → **NEW #72**
- **REST `?format=csv` bypasses the 1000-row cap** and buffers `SELECT *` into memory — `plugins/umbra-rest/src/lib.rs:1748` (`page=None` skips the clamp). Fix: stream + clamp. → **NEW #72**

### Silent wrong-writes (reviews/correctness-domain.md) — NEW
- **Non-i64 M2M child ids dropped from form junction writes** — `crates/umbra-core/src/orm/forms_runtime.rs:226` (reports success, writes nothing). → **NEW #73**
- **Floats bypass `min`/`max` validation** — `orm/dynamic.rs:1348, 2656` (reports success, stores out-of-range). → **NEW #73**
- **`inline_edit` silently writes `""` on parse failure** — `plugins/umbra-admin/src/inline_edit.rs:163` (vs `actions.rs:44` which 400s). → **NEW #73**

### Unsafe migrations (reviews/correctness-domain.md)
- **`nullable→NOT NULL` and `unique false→true` emit ALTERs with no NULL/dup pre-check** — abort mid-migration, advisory-warning only, untested. Maps to **gaps #79** (migration safety) — add the pre-check + test.

### Docs that break or actively mislead (docs-audit/*)
- `orm/querying.mdx` — `FColExt` claimed in `umbra::prelude` (it's in `umbra::orm`).
- `orm/relationships.mdx` — documents non-existent `#[umbra(m2m = "...")]`.
- `realtime/sse.mdx` + `realtime/scaling.mdx` — stray `</content>`/`</invoke>` tool-gen artifacts break MDX.
- `migrations/checkmigrations.mdx` — `umbra checkmigrations` (no registry) → use `cargo run -- checkmigrations`.
- `plugins/admin.mdx` — wrong CSS path `/static/admin/admin.css` (it's `/admin/static/...`); false "`on_ready` runs DDL" (it's a no-op; tables via migration engine).
- `rest/nested.mdx` — **FIXED** (`6f...` this session; was our own #2 debt).

---

## P1 — Important

### Security (reviews/security.md) — 0 Critical, all NEW
- **OAuth: no PKCE + replayable `state`** — `plugins/umbra-oauth/src/routes.rs:143-218`. Add PKCE (S256) + single-use state. → **NEW #74**
- **Empty `SECRET_KEY` silently signs CSRF with an empty HMAC key** — `plugins/umbra-security/src/lib.rs:392-394`. Fail-closed / boot-warn on empty key. → **NEW #75**
- **`password_hash` serde-serialized, guarded only by the block-list** — `plugins/umbra-auth/src/lib.rs:233-234`. One `.expose(["auth_user"])` without `.hide()` leaks argon2 hashes. Fix: `#[umbra(noform)]`-style "never serialize" / auto-hide for `password_hash`. → **NEW #75**
- **Inactive-superuser permission bypass on a live session** — `plugins/umbra-permissions/src/rest.rs:97-105`. Re-check `is_active`. → **NEW #75**
- Misconfig exposure: Host-validation off in Dev, HSTS/CSP opt-in — boot-time `check.rs` warnings (ties to gaps2 #25). FYI/Important.

### Performance (reviews/performance-scalability.md)
- **Per-row registry deep-clone** — `umbra-rest/src/lib.rs:779 → migrate.rs:85` (1000-row page = 1000 full-registry clones). Clone once. → **NEW #72**
- **~12-14 serial permission queries per changelist** — `plugins/umbra-admin` `AdminPerms::load`/`require`; `user_perms()` (one query) is the fix. → **NEW #72**
- **No auto-index on FK columns or `deleted_at`** — `migrate.rs` DDL. The #63 200M-row cliff; soft-delete filter scans. → fold into **#63** + a new index-emission item.

### Correctness / silent failures (reviews/correctness-domain.md + static-analysis.md)
- **⚑ Sessions: corrupt data → empty map, no log** — `umbra-sessions/lib.rs:389,447` (also in P0 set_data fix). → **#71**
- `Masked` malformed key → silent `None` keyring — `crates/umbra-core/src/orm/masked.rs:204`. Surface the error.
- REST CSV writer errors dropped — `umbra-rest`.
- **FK `on_delete` is DDL-only** — no ORM cascade collector / `post_delete` signal on cascades. → **gap #68** follow-up.
- `storage.rs:186` `.expect` panics every request if `MediaPlugin` unregistered → boot system-check instead. → **NEW #73**

### Architecture (reviews/architecture-modularity.md)
- **⚑ Boundary violation: `umbra-auth` → `umbra-rest`** for `Authentication`/`Identity` traits — forces `umbra-rest` into every app; contradicts the CLAUDE.md "REST-free app compiles with zero serializer code" contract. Fix: lift those traits into `umbra-core`/facade. → **NEW #76**
- Duplication: `to_snake_case` ×3 (`umbra-macros`, `inspect.rs`, `queryset/mod.rs`), `pascal_case` ×2 (`umbra-openapi`, `umbra-cli`) → an `umbra-naming` internal helper or facade fn. → **NEW #77**

### Docs (Important — long tail, docs-audit/*)
- `.on()` is SQLite-only but used without caveat across pages (e.g. `aggregates.mdx`); add a global note. · REST block-list says "3 tables", code blocks 10 (`rest/exposure.mdx`). · `OrPermission` "strongest error code" vs actual last-write (`rest/permissions.mdx`). · `auth/user-in-templates.mdx` stale "reverse traversal not implemented" callout (it ships) + anonymous sentinel missing `is_staff`/`is_superuser` (`session_user.rs:506`). · `umbra-email` referenced as shipped (crate doesn't exist). · `TenantUser` `fn id(&self) -> i64` vs real `-> <Self as Model>::PrimaryKey`. · `auth/authentication.mdx` `use async_trait::async_trait` → `umbra::async_trait`. · `web/error-pages.mdx` internal `umbra_core::errors::…` path. · `examples/basic.mdx` Cargo.toml missing `"chrono"`. · `templates/helpers.mdx` omits 3 built-in functions. · permissions.mdx example arg-order + `.on(&pool)` that won't compile. → all **doc-fix batch** (cheap, high-value).

---

## P2 — Cleanup / maintainability

### File splits → cohesive modules (reviews/architecture-modularity.md) → **NEW #78**
Ranked by pain-relief (proposed module trees in the report):
1. `orm/queryset/mod.rs` (4846) → `{builder, joins, read, write, values, manager, m2m_dedup}`.
2. `migrate.rs` (4660) → `{registry, types, engine, diff, render, tracking}` (render ~900 LOC is orthogonal).
3. `umbra-macros/src/lib.rs` (4521) → `{derive_model/, field_meta, column_const, kind, derive_form, derive_choices, task_macro}`.
4. `orm/dynamic.rs` (3009) → 8 sub-modules; collapse the **4 parallel decode fns** (`decode_to_string`/`_pg`/`_to_json`/`_pg_to_json`) — every new column type must be edited in all four (maintenance trap).
5. `orm/column.rs` (2845) → 8 type-family sub-modules (PG-only families feature-gateable).

### Static-analysis cleanup (reviews/static-analysis.md)
- 135 clippy warnings, 0 errors — mostly style (`needless_borrow` ×15 in `dynamic.rs`, malformed rustdoc ×16, `useless_conversion` ×10 in `m2m.rs`). One real: `result_large_err` `forms.rs:1123` (`FormErrors` ≥128B in Err). `cargo clippy --fix` clears most.
- `#[allow(dead_code)]` in prod src: `umbra-rest/src/filtering.rs:94` (`into_condition`), `umbra-admin/src/rows.rs:93` (`too_many_arguments`).
- No TODO/FIXME in `src` (clean).

---

## NEW gaps2 entries to file (#71–#78)
- **#71** Concurrency hardening — transactions/UNIQUE backstops for session `set_data`, `set_user_groups`, `update_or_create`/`get_or_create`; log corrupt-session decode.
- **#72** Endpoint scalability — paginate admin M2M form, clamp+stream REST CSV, clone registry once, batch admin permission load, auto-index FKs/`deleted_at`.
- **#73** Silent wrong-writes — non-i64 M2M form junction, float min/max bypass, inline_edit empty-string write, `storage` unregistered-plugin panic → boot check.
- **#74** OAuth PKCE + single-use state.
- **#75** Secret/auth hardening — empty `SECRET_KEY` fail-closed, `password_hash` never-serialize, inactive-superuser live-session re-check.
- **#76** Plugin-contract fix — lift `Authentication`/`Identity` out of `umbra-rest` into core (REST-free apps).
- **#77** Dedup `to_snake_case` / `pascal_case`.
- **#78** Module splits for the 5 files >2,800 LOC.
- (Existing updated: **#34** stale ref + `update_expr`; **#35** + hydration 3rd surface; **#63** FK/`deleted_at` indexes; **#68** `on_delete` ORM cascade; **#79** unsafe-ALTER pre-check.)
- **Doc-fix batch** — the ~8 Critical + long-tail doc drifts (not a gaps2 entry; a single docs PR).

---

## Wave C — per-plugin review (all 19 built-in plugins)

> One report per plugin in `planning/hardening/plugins-review/<plugin>.md` (the detailed single source). Standard 5 axes **plus completeness** (stubs / no-op hooks / half-features / what's missing vs the Django·Celery·DRF peer). Net-new findings filed as **gaps2 #79–#86**; the per-plugin reports hold the full long tail.

**Verdicts**

| Plugin | Verdict | Headline net-new |
|---|---|---|
| umbra-rest | Solid (strongest) | `?ordering=` reserved+doc'd but never read → silent unsorted (#79); confirms #76 from REST side |
| umbra-auth | Solid | `Identity` drops `is_superuser` at REST boundary (#80); no pw-strength/throttle (#82) |
| umbra-sessions | Solid | no rolling expiry / no `clearsessions` / no `SessionStore` trait (#80/#82) |
| umbra-permissions | Solid (~90% DRF parity) | `table_app_label` splits at first `_` → wrong Permission row (#80) |
| umbra-email | Solid v1 | no CRLF/header-injection guard+test on `subject` (#81); console leaks tokens |
| umbra-signals | Solid (w/ fix) | async handler panics not isolated → kill the request (#80) |
| umbra-realtime | Solid (single-instance) | hard non-optional `umbra-auth` dep (#84); no Last-Event-ID resume (#82) |
| umbra-livereload | Complete | `to_bytes(body, usize::MAX)` uncapped buffer (report) |
| umbra-health | Complete | `probe_database` raw `sqlx` ORM bypass → add `umbra::db::ping()` (#84); no per-check timeout |
| umbra-playground | Complete (real plugin) | naive multi-`replace()` placeholder chain → injection breakout (#81) |
| umbra-cache | Has gaps | `cache_page` Host/Cookie poisoning (#81); single backend (#82) |
| umbra-static | Has gaps | embedded ETag/304 missing + symlink loop (#81); no manifest hash (#82) |
| umbra-tasks | Lean v1 | fragile `starts_with("handler not found")` non-retriable check (#80); no SKIP-LOCKED/beat/result-backend (#82) |
| umbra-oauth | Real, incomplete | token refresh never implemented (#79); reqwest no-timeout (#81); PKCE = #74 |
| umbra-openapi | Accurate schema, path holes | CRUD paths hardcode `/api/...` ignoring `base_path()` (#79) |
| umbra-security | Solid for scope | `csrf_exempt_paths` segment-boundary bug (#81); empty-key = #75 |
| umbra-media | Has gaps | no file-lifecycle cleanup + fully-buffered `Storage` trait (#82) |
| umbra-admin | Has gaps | `TabularInline` stored-but-never-rendered stub (#79); base-path 404s + view-perm disclosure (#83) |
| umbra-rls | Real but non-functional | emits policy DDL but nothing sets `app.user_id` per request → deny-all (#79, ties #69) |

**Contract:** clean across the board — facade-only imports, no raw `sqlx::query`/`query_as` in plugin `src/` (the lone `umbra-health` `SELECT 1` is the one exception → #84), each owns its migrations. The two dependency-direction smells are #76 (`auth→rest`) and #84 (`realtime→auth` hard dep).

**New gaps2 entries (#79–#86):** #79 advertised-but-non-functional surfaces · #80 reliability/correctness (signals async-panic, sessions expiry, tasks claim, permissions app_label, auth superuser) · #81 plugin security (csrf boundary, email injection, playground, cache/static, oauth timeout) · #82 missing breadth (media/static/cache/tasks/rest deferred features) · #83 admin base-path + authz · #84 plugin-contract + shared `db::ping()`/`block_on_ready` primitives · #85 security/correctness test-coverage holes · #86 plugin doc drifts.
