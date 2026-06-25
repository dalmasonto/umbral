# Hardening review — charter

> The single source of truth for the hardening pass. Every review stream writes its findings into this folder; `backlog.md` synthesizes them into a prioritized, deduplicated list that feeds `planning/gaps.md` / `gaps2.md` / `features.md`.

## Goal & posture

**Harden what we already have. No new plugins.** The framework surface is wide enough; the job now is to make it correct, safe, fast, and maintainable, and to make the docs match the code. Two strategic items are called out as "fix early to avoid later chaos":

- **The database router (gaps2 #69)** — extract routing into a swappable `DatabaseRouter` trait. It is the keystone for **multitenancy** (schema-per-tenant) and absorbs #22 (done) / #23 (read-write split). Best done early while the ORM is still malleable.
- **Multitenancy** — schema-per-tenant (`django-tenants` model), built-in. Depends on #69 + a per-request routing context.

Quick win to fold in: **gaps2 #66** (document minijinja filters + custom template tags).

## How findings flow

1. Each **review stream** (below) is run by a subagent against real files and writes a severity-labeled report into this folder.
2. **Severity labels** (from `code-review-and-quality`): `Critical:` (blocks — data loss / security / broken), *(no prefix)* = required, `Important:`, `Optional:`/`Consider:`, `Nit:`, `FYI`.
3. Each finding records: **file:line**, the problem, the fix shape, and a **gaps ref** (existing #, or "NEW" to file).
4. `backlog.md` deduplicates + ranks everything into a hardening plan; NEW items get appended to `gaps2.md` (max+1, never renumber).

## Streams & outputs

### A. Docs audit — `docs-audit/<area>.md`
"From the first app to the last page: is what's documented actually there, and does it behave as described?" 75 MDX pages across `orm` (14), `plugins` (18), `web` (9), `rest` (7), `migrations` (5), `auth` (4), `examples`/`templates` (3 each), `backends`/`cli`/`realtime`/`testing`/`getting-started` (2 each), `admin`/`about` (1 each). Each page's claimed APIs/attributes/behaviors are grepped against the code; drift is logged. (Precedent: this session already fixed the OneToOne parent-side example, the "no Postgres-only fields" admin claim, the non-existent `#[umbral(rename)]`/`column` attribute, and the `Decimal`/`Cidr` half-wiring.)

### B. Code review streams — `reviews/<stream>.md`
Run against the core (ORM, migrations, macros, the dynamic write path, app/build, plugins). Five-axis review per `code-review-and-quality`, split by lens:

- **`reviews/race-conditions.md`** — *the ORM is DB-heavy: can two concurrent requests diverge data?* Audit the ambient pool (`OnceLock`), session create/lookup, signals registry, `update_or_create`/`get_or_create`, M2M junction writes, the dynamic `insert_json`/`update_json` paths, soft-delete filters, and any check-then-write (TOCTOU) — does it need a transaction / `SELECT … FOR UPDATE` / unique constraint? Concurrency tests where gaps exist.
- **`reviews/security.md`** — SQL injection (every `sqlx::query`/`query_as` + `Expr::cust` + any string-built SQL incl. `filter_sql`, search branch SQL), authn/authz gates (REST default perms, admin staff gating, permissions plugin), CSRF, secrets in code/logs, input validation at boundaries, XSS/template autoescape, the `Masked` field, allowed-hosts/CORS. Reuse `references/security-checklist.md`.
- **`reviews/performance-scalability.md`** — N+1 (prefetch/select_related coverage, REST `?include=`, admin changelists), unbounded queries / missing pagination/LIMIT, hot paths (per-request allocations, `to_value`), the 200M-row concern (gaps2 #63), index coverage. Reuse `references/performance-checklist.md`.
- **`reviews/correctness-domain.md`** — does the ORM/migrations/plugin behavior match the spec + Django parity? Edge cases (empty/null/boundary), error paths (not just happy path), soft-delete dynamic-path gaps (gaps2 #34/#35), PK-type assumptions (the i64 lift), the migration autodetector's known holes (gaps #89/#90/#93/#94).
- **`reviews/architecture-modularity.md`** — module boundaries + **file size / cohesion** (QA): the 5 files >2,800 LOC (`queryset/mod.rs` 4846, `migrate.rs` 4660, `umbral-macros/lib.rs` 4521, `dynamic.rs` 3009, `column.rs` 2845) are split candidates — propose **module** splits (a directory of focused files that belong together), not arbitrary file cuts. Dead code, the plugin-contract/facade boundary, dependency direction, duplication.
- **`reviews/static-analysis.md`** — `cargo clippy --workspace --all-targets` landscape (umbral-admin alone had ~44 warnings; umbral-macros has several), `unwrap()`/`expect()`/`panic!` in non-test/prod paths, `unwrap_or_default()` masking, `.ok()` swallowing, `#[allow(...)]` audit, TODO/FIXME inventory.

## Work-lists (snapshot)
- **Crates:** `umbral-core`, `umbral-macros`, `umbral` (facade), `umbral-cli`, `umbral-testing`.
- **Plugins (19):** admin, auth, cache, email, health, livereload, media, oauth, openapi, permissions, playground, realtime, rest, rls, security, sessions, signals, static, tasks.
- **Largest files (split candidates):** `crates/umbral-core/src/orm/queryset/mod.rs` (4846), `migrate.rs` (4660), `crates/umbral-macros/src/lib.rs` (4521), `orm/dynamic.rs` (3009), `orm/column.rs` (2845), `plugins/umbral-rest/src/lib.rs` (2668), `plugins/umbral-openapi/src/lib.rs` (1658), `forms.rs` (1561), `umbral-cli/src/scaffold.rs` (1467), `app.rs` (1409).

## Execution
Subagents run in waves (docs audit, then code-review lenses), each writing its report here. The controller reviews each report, then produces `backlog.md`. Nothing is auto-fixed from this pass — it produces the **prioritized hardening backlog**; fixes happen in focused follow-up PRs (router/#69 + multitenancy first, then by severity).
