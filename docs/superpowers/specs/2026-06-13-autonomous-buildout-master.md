# Autonomous build-out — master plan of record

Date: 2026-06-13
Status: executing
Driver: autonomous (user away; full authority granted to spec + build + close)

This is the plan-of-record for a large autonomous mandate. It survives context resets — a future session reads this to know what's done, what's next, and why each decision was made. Each phase commits independently; check `git log` + the per-item trackers (`planning/features.md`, `planning/REAL-GAPS.md`, `planning/orm_fixes.md`) for live status.

## The mandate (verbatim intent)

1. **Every umbral_website page backed by real ORM data — zero dead links, zero 404s.** Existing hardcoded pages → DB; orphan models → new pages; nav links must all resolve.
2. **`seed_orm_data` CLI command** — expand the seed to cover plugins, their features, and all data every page needs.
3. **Testing & factory library (features.md #79, #52; REAL-GAPS A#18)** — "a way to test out your models, your website." Prefer a plugin/crate. Factories that simulate realistic plugin submissions + model updates. Document well. (#74 fixtures already shipped; #79 adds factories + test client.)
4. **Close features.md #56–61** — admin widgets, autocomplete (FK/M2M/O2O search — mostly there), REST nested writes, REST auth (already wired — verify+close), REST action schemas, CSV/Excel.
5. **Implement features.md #65–70** — migration-safety command, collectstatic+compression, template tags/filters hook, middleware pipeline trait, DB routers, streaming/compressed responses.
6. **Review features.md + REAL-GAPS.md** — mark done items, pick low-hanging fruit (any number).
7. **Spec + plan + start SSE/WebSockets** (features.md #45 `umbral-realtime`) — heavy plugin; must support user-specific AND group/room-specific delivery. Detailed design below.

## Operating rules (from the user + CLAUDE.md)

- Never wipe the DB or migration files; seeds are idempotent (per-row/per-table short-circuit).
- Never `git stash` / discard the dirty tree.
- Never touch the running dev server (`:8100`); don't `cargo run`/`pkill` examples.
- Don't `cargo build` after every edit — batch per unit, build once to confirm before commit.
- If disk fills: `rm -rf crates/target` (this project only) + the stray `umbral_website/plugins/public/target`; then continue.
- Optimized ORM queries (no N+1). **If a query can't be expressed, fix the ORM and append to `planning/orm_fixes.md`** (file, failing query, fix, test) — then continue.
- One cohesive unit per commit. Ship a feature → ship its doc page (`documentation/docs/v0.0.1/...`).
- Don't fabricate metrics (github_stars/downloads stay `—` when unknown). Curated editorial facts (status/maturity/features/audit) ARE legitimate to seed.

## Execution order (top-down; commit each unit)

### PHASE 1 — Website, zero 404s  ← START HERE
Foundation then pages. Data lives in each plugin's `seed()`; `seed_orm_data` orchestrates.

- **1a. Seed foundation.** Add `feature_set: ReverseSet<PluginFeature>` to `Plugin` (mirrors `comment_set`; virtual, no migration). Extend `plugin_directory::seed` to seed `PluginFeature` rows per official plugin (curated feature lists w/ status+maturity+display_order — see prebuilt.html for admin/rest/auth lists; author the rest). Add `pub async fn seed()` to `community`, `features`, `showcase`, `site_content`. New `seed_orm_data` command via a tiny `SeedPlugin` in the binary crate (`umbral_website/src/`), registered in `main.rs`; `umbral_cli::dispatch` picks up `Plugin::commands()`. Keep `on_ready` minimal auto-seed so the live site never boots empty.
- **1b. `/prebuilt`** — load `source=official, moderation=approved` plugins, `.prefetch_related("feature_set")` (2 queries). One detailed card per official plugin with its real feature tracker (reuse detail page's `feature_status()` status→dot mapping). DELETE the "More official plugins" strip. Fix the literal `{}` icon bug.
- **1c. `/community`** — `SocialLink` (channels), `CommunityResource` (resources), `NewsletterConfig` (subscribe band). Replace hardcoded template blocks. SentinMail URL stays.
- **1d. `base.html` nav + footer** ← `NavigationItem`. Inject via an ambient context-processor mirroring `with_user_in_templates()` (load once into a `OnceLock`, refreshed by seed). If a framework hook is needed beyond the user-injection pattern, build it (it's the right contract) and document; fall back to a `nav_context()` helper spread into each handler only if the hook is genuinely out of reach. NavigationItem placements: `header` (with optional dropdown group) + `footer` (grouped sections).
- **1e. New pages (wire dead nav links):** `/features` (FrameworkFeature grouped by FeatureCategory, status badges), `/showcase` + maybe `/showcase/{slug}` (ShowcaseEntry gallery, approved/featured), `/blog` + `/blog/{slug}` (BlogPost published, markdown body, M2M tags), `/changelog` (FeatureStatusEvent timeline, or curated release notes). Each: route + view-model + template (frontend-design quality, match existing token system) + seeded content. Add a `/healthz`-style is not needed here.
- **1f. `/dashboard`** — point existing widgets at real queries (it's login-gated admin demo data today).
- **1g. Verify:** crawl every nav/footer link + in-page link; assert 200 (a render smoke-test like `plugin_directory/tests/render_pages.rs`). No 404, no dead link.

### PHASE 2 — Testing & factory library (#79)
New crate `umbral-test` (a dev-dependency crate, not a runtime plugin — but expose a `Factory` trait usable anywhere). Surface:
- `Factory` trait: `fn build() -> Self` (sensible fake defaults via `fake` crate) + `async fn create() -> Self` (persist via ORM). Derive macro `#[derive(Factory)]` reading field types + `#[factory(...)]` overrides (sequence, faker kind, sub-factory for FKs). Mirrors factory_boy/FactoryBot.
- `TestClient`: boots the App in-memory (sqlite::memory:), `.get/.post/.put/.delete` returning a response wrapper with status + json + html asserts. Per-test isolation via a fresh in-memory DB (transaction-rollback is a stretch — sqlite memory per test is simpler and correct).
- Realistic-submission helpers tuned to the website's models (a `PluginFactory`, `ReviewFactory`, etc. as examples).
- Document: `documentation/docs/v0.0.1/testing/{factories,test-client}.mdx`. Relate to #74 fixtures.
- Close #79, #52, REAL-GAPS A#18.

### PHASE 3 — Close features.md #56–61
- **#56 Admin dashboard widgets** — widget kinds exist in website (`src/widgets/`); the AdminPlugin needs a `dashboard_section`/`Widget` registry so apps add cards (KPI w/ currency+comma formatting+delta, multi-series line across N years, sparkline, donut, table-with-period-chips). Verify what admin already ships; build the registry + the complex widget kinds; document `admin/widgets.mdx`.
- **#57 Admin autocomplete** — `fk_picker.rs` exists; confirm FK/M2M/O2O search-as-you-type hits REST `?search=`; close or finish.
- **#58 REST nested writes** — `ResourceConfig::nested(...)`; create handler reads nested array, one transaction, parent then children. High value. Build + test + doc.
- **#59 REST auth** — `cfg.gate()` already wired; verify securitySchemes in OpenAPI (playground gap #4); close.
- **#60 REST action schemas** — `Action` gains optional input/output JsonSchema; validate body; emit into OpenAPI.
- **#61 CSV/Excel** — `csv` + `calamine`; admin export action + `importcsv` command via `bulk_create`.
- Update features.md statuses; archive closed write-ups to `planning/archive/`.

### PHASE 4 — features.md #65–70
- **#65** migration-safety command (`cargo run -- checkmigrations`): flag DROP COLUMN on non-null-no-default, un-renamed RENAME, NOT NULL without default on populated table. (`is_safe_cast`/destructive detection already exists in migrate.rs — wrap as a command + doc the expand-contract pattern.)
- **#66** `collectstatic` exists → add `tower_http::compression` (gzip/brotli) + `{% static %}` tag.
- **#67** `Plugin::register_template(&mut Environment)` hook so plugins add filters/tags; ship `now`, `url`, `currency` built-ins (admin already calls `add_filter` internally — generalize to the plugin surface).
- **#68** `Middleware` trait (`before_request`/`after_response`) + stack; adapt existing layers.
- **#69** `DbRouter` trait (`read_db_for::<T>`/`write_db_for::<T>`) over the existing `on(&pool)` seam.
- **#70** streaming `Response` body (axum `Stream`) + `.gzip()/.brotli()`.
- Each: doc page. Low-priority ones may ship minimal.

### PHASE 5 — Tracker review
- features.md: flip done boxes, archive write-ups, fix any stale status. REAL-GAPS.md: update the Umbral-status column for everything now shipped (OAuth A#12 ✅, file uploads A#10 ✅/⚠, health A→B#7 ✅, etc.), close done, harvest low-hanging fruit (e.g. `/healthz` already exists; structured-logging JSON layer; `?utm`; small ones).

### PHASE 6 — SSE / WebSockets (`umbral-realtime`, #45)  [detailed below]
Spec + plan + start. Heavy plugin.

---

## SSE / WebSockets detailed scope (`umbral-realtime`)

Goal: a developer can push **user-specific** data ("notify user 42") AND **group/room-specific** data ("everyone in `chat:123`" / "all staff") without hand-rolling channel bookkeeping. Built on axum's native SSE + `tokio-tungstenite` for WS, and the signals system (#38) so model changes can fan out.

### Core model — a broker over tokio broadcast/mpsc

```rust
// umbral-realtime
pub struct Realtime;                       // ambient handle, set in App::build (OnceLock)
impl Realtime {
    pub fn to_user(uid: i64)   -> Target;  // a single authenticated user (all their connections). It might not be i64, it might be UUID, so this is generic, remember that.
    pub fn to_group(name: &str) -> Target; // a named room/group ("chat:123", "staff", "tenant:7")
    pub fn broadcast()          -> Target;  // everyone connected
}
pub struct Target { /* ... */ }
impl Target {
    pub async fn send(self, event: &str, data: impl Serialize);   // typed event + JSON payload
}
```

Connection registry (in-process for v1; pluggable backend later):
- `ConnId` per socket. A `HashMap<ConnId, mpsc::Sender<Event>>` is the fan-out sink.
- Two index maps: `user_id -> HashSet<ConnId>` and `group -> HashSet<ConnId>`. `to_user`/`to_group` resolve to conn ids, push to each sender. Cleanup on disconnect removes the conn from all indexes.
- **Auth-aware:** a connection's `user_id` comes from the session/bearer identity at handshake (reuse umbral-auth's `resolve_identity`). Anonymous connections can still join public groups but `to_user` never targets them.
- **Group membership:** explicit `Realtime::join(conn_or_user, group)` / `leave`, OR declarative at subscribe time — the client opens `GET /realtime/sse?groups=chat:123,presence` and the server validates each requested group against an app-provided `GroupPolicy` (so a user can't subscribe to `tenant:99` they don't belong to). `GroupPolicy::can_join(identity, group) -> bool` is the security seam — default deny for non-public groups.

### Transports
- **SSE** (`GET /realtime/sse`): axum `Sse<impl Stream>`; the per-conn `mpsc::Receiver` becomes the event stream; heartbeat keep-alive comment every ~15s. This is the default (simplest, proxy-friendly, unidirectional server→client). Phase-2 `iterator()` Stream (features.md #29) lands `futures-util` here.
- **WebSocket** (`GET /realtime/ws`): axum `WebSocketUpgrade` + `tokio-tungstenite`; bidirectional. Inbound client messages dispatch to an app `MessageHandler` (chat send, presence ping). Outbound shares the same per-conn sink.

### Signals bridge
`RealtimePlugin::on_model::<Post>(|ev| Realtime::to_group(format!("post:{}", ev.pk)).send("updated", ...))` subscribes to `post_save`/`post_delete` (#38) and fans out — the "live dashboard/notifications" story with zero polling. The admin notification bell (#2) becomes one consumer: `to_group("staff")` on relevant signals.

### Scaling note (documented, not built v1)
Single-process broadcast works for one instance. Multi-instance needs a backplane (Redis pub/sub or Postgres LISTEN/NOTIFY) so `to_user(42)` reaches the instance holding that socket. Design the registry behind a `Broker` trait now (`publish(envelope)` / `subscribe()`), ship `InProcessBroker`, leave `RedisBroker` as the documented Phase-2 swap. This mirrors the alerts/cache backplane direction.

### Deliverables (in order)
1. Spec doc `docs/superpowers/specs/2026-06-13-umbral-realtime-design.md` (this section, expanded).
2. `umbral-realtime` crate: broker + registry + SSE transport + `Realtime` ambient + `GroupPolicy`.
3. WS transport + `MessageHandler`.
4. Signals bridge + `on_model`.
5. A demo on `umbral_website` (live plugin-submission feed to `staff`, or a presence counter) + playground "Realtime" tab (features.md #10 unblocks).
6. Docs `documentation/docs/v0.0.1/realtime/*.mdx`. Closes #45; unblocks #2, #10, #77 SseChannel.

---

## Decisions / rationale log

- **Seed location:** orchestrating `seed_orm_data` lives in the binary crate (already depends on every website plugin); each plugin owns its `seed()`. Avoids inter-plugin crate deps.
- **`feature_set` field:** additive `ReverseSet`, migration-free, consistent with `comment_set`; enables `prefetch_related` (2 queries) over N+1.
- **Nav injection:** ambient context-processor (mirror user-injection) is the right framework contract; a per-handler helper is the fallback, not the goal.
- **Testing as a crate, not a runtime plugin:** factories/test-client are dev-time; a runtime `Plugin` would wrongly ship in prod binaries. `Factory` trait is usable from any test.
- **Realtime in-process first, broker trait now:** correctness for single-instance today, clean multi-instance path later without an API break.

## Status ledger (update as phases land)
- [~] P1 website (nav-links resolve; nav-from-DB + dashboard widgets + /blog detail remain)
- [x] **P2 testing** — `Factory` trait added to umbral-testing (build/create/create_with/create_batch + `seq()` + `fake` re-export); marker-type shape for the orphan rule; doc pages (testing/factories + test-client). Commit 517fc84. (#79/#52 ✓)
- [~] **P3 #56–61** — #56/#57/#59 closed (c37e928); **#58 nested writes SHIPPED** (9ab22e8; true-DB-tx → orm_fixes #2); **#61 CSV export SHIPPED** (`?format=csv` on the list endpoint — 8103f70); **#60 action schemas SHIPPED** (1dea66a — `ResourceConfig::action_input_schema`/`action_output_schema`; body validated pre-handler → 400 on mismatch; `registered_action_schemas()` → OpenAPI path items; subset validator, full schema still ships to OpenAPI; `tests/action_schemas.rs`; doc `rest/actions.mdx`). **Still open:** #61 remainder (CSV import command, admin bulk-export action, Excel). Realtime SSE+WS retested green after the target wipe (full clean rebuild).
- [~] **P4 #65–70** (#65/66/67/68/70 shipped; #69 DB-routers deferred by design) — **#66 SHIPPED** (gzip/brotli compression via `AppBuilder::compression()`; collectstatic + `{% static %}` tag already existed — 5497823). **#65 SHIPPED** (c38dfce — `umbral checkmigrations [--strict]`: `migrate::classify_operation` tags pending ops SAFE/WARNING/UNSAFE, exits non-zero on unsafe → CI gate; `check_pending_safety[_in]` programmatic; pure + e2e tests; doc `migrations/checkmigrations.mdx`; full expand-contract ops guide still deferred). **#67 SHIPPED** (d08280f — `Plugin::template_registrars() -> Vec<TemplateRegistrar>` owned `'static` closures; `templates::init_with` stashes them in a `REGISTRARS` OnceLock so `build_env` re-applies on dev hot-reload, after built-ins so a plugin can override by name; example built-ins `now()`/`currency`; facade re-exports `TemplateRegistrar`+`Environment`; tests `template_tags.rs`; docs `templates/custom-tags.mdx`+`helpers.mdx`. **Deferred**: `{% url "name" %}` reverse-route tag — needs a named-route registry). **#68 SHIPPED** (72db36a — `middleware::Middleware` async trait `before_request`(Err short-circuits)/`after_response`; `MiddlewareStack` → one `from_fn_with_state` layer, before in-order / after reverse (onion); `AppBuilder::middleware` + `Plugin::middleware`; installed after 404 fallback, inside host/CORS/compression; facade `umbral::middleware::*` + prelude `Middleware` + `umbral::async_trait`; test `middleware_pipeline.rs`; doc `web/middleware.mdx`. **Deferred**: re-expressing CORS/rate-limit/cache tower layers on the trait — cosmetic). **#70 SHIPPED** (968ad70 — `web::StreamingResponse` impl `IntoResponse`: `from_chunks` (infallible) / `new` (fallible) over `Body::from_stream`; `content_type`/`attachment`/`inline`(CRLF-sanitized)/`status`; composes with compression; facade + prelude; test `streaming_response.rs`; doc `web/streaming.mdx`). **Remaining: #69 DB routers** — INTENTIONALLY DEFERRED: deep ORM pool-selection surgery (read-replica routing), genuinely premature, and would collide with the planned PrimaryKey refactor + ORM gap backlog. Leave until read-replica scaling is a real bottleneck.
- [~] **P5 trackers** — REAL-GAPS.md updated for shipped items (4b33b07). features.md #56/#57/#59 closed.
- [x] **P6 realtime — COMPLETE** — spec (60662b8) + phase 1 (0cb17bc) + phase 2 SSE (8035f9e) + phase 3 WebSocket (af86351) + phase 4 signals bridge (388d49e) + **live demo: SSE note feed on plugin pages** (e156ad5) + **phase 5 multi-instance `RedisBroker`** (4730467 — `RealtimePlugin::redis(url)` behind the `redis` feature; per-instance publish+subscribe pump on a shared Redis channel, reconnect-with-backoff; `Envelope`/`TargetKind` JSON wire format; validated end-to-end against live Redis, ~0.4s cross-instance relay; `tests/broker.rs`; doc `realtime/scaling.mdx`). **features.md #45 now [x]; 12 tests.** The only realtime-adjacent item left is the **playground "Realtime" tab — a separate Low-priority frontend feature (#10)** in the `umbral-playground` React SPA, NOT part of the realtime framework.

Status: **P6 COMPLETE** (Redis broker shipped). **P4 done** bar #69 (deferred by design). Session `[~]` cleanup: #45/#58/#65/#67/#68 flipped to `[x]` — they were shipped-with-a-deferred-note but mislabeled `[~]` (the file's convention is `[x]` + a `Deferred:` line, like #13/#16/#18). The remaining `[~]` (#19/#24/#26/#29/#33/#55/#12) are genuinely-partial pre-existing items: a usable core shipped, with a deliberately-deferred remainder ("when a consumer needs it"), several gated on the PrimaryKey refactor / ORM gap backlog — NOT mislabels, left intentionally.

Cheap-ORM cleanup done (user-chosen): **#24 closed** (b9dffc3 — trim/coalesce/concat on StrColExt, native sea-query exprs to avoid a cust_with_values bind-order swap; `now` deferred with SQLite-format rationale) + **#33 closed** (0868511 — auto-GIN index on every tsvector column in the PG render path). Remaining genuinely-partial `[~]`: #19 (reverse-FK prefetch — needs new ORM slot), #26 (correlated EXISTS), #29 (iterator), #55 (date hierarchy), #12 (playground tabs) — real infra/frontend scope, left intentionally.

**#61 CSV import shipped** (37ebf1e — `umbral importcsv <table> <file>`; `orm::import_table_rows` coerces cells per column type → `insert_json` per row; best-effort with per-line errors; test reads back typed rows). #61 stays `[~]`: admin "export selected → CSV" is **blocked on the unstarted #53 bulk-action UI**; Excel (`.xlsx`) is a separate binary-format add (rust_xlsxwriter/calamine), not yet done.

**PrimaryKey refactor COMPLETE — no edges** (user-prioritized — "avoid hardcoded i64 for ids"). Models key on i64, String/slug, or Uuid end-to-end on **both SQLite and Postgres**. All 8 phases shipped: keystone (620a27c), hydration (b624594), ReverseSet (0569078), OneToOne (4becbec), M2M (fe76509), in_bulk+join-dedup (d550409), backup+dynamic-filters (2522c56), **Postgres-uuid edge (8c19e89)**. The last one threaded the target `SqlType` into `fetch_related_as_json_by_pk` (uuid binds native on PG, not text; `BindKind` removed) + added a `Uuid` arm to `backend_pg::row_to_json`; verified by `pk_uuid_postgres.rs` against the user's live Postgres (forward select_related + reverse-FK prefetch on a uuid-PK model). 887 SQLite tests + the live-PG backup/queryset tests all green. Below is the original IN-PROGRESS note (superseded):

**[superseded] PrimaryKey refactor** (user-prioritized — "avoid hardcoded i64 for ids"). Prereq ORM-gap queue was already empty. Shipped: **keystone** `pk_as_json` + `pk_key` (620a27c); **hydration lift** reverse-FK + OneToOne hydrators → PK-agnostic (b624594); **ReverseSet win** (0569078, `tests/pk_string_reverse_fk.rs`); **OneToOne win** (4becbec — split into `fk: Option<C::PrimaryKey>` + `parent_pk: Option<Value>` mirroring ForeignKey; both directions work on a String PK, `tests/pk_string_one_to_one.rs`). i64 non-regression green throughout (884 umbral-core tests). **Remaining i64 sites** (see memory project-primary-key-refactor): **M2M slot + junction writes + M2M-junction prefetch** (the last + most entangled relation slot — only one still needing the typed `__pk`), in_bulk / join-dedup in queryset/mod.rs, backup.rs restore, dynamic.rs set_m2m_from_strings. Lift one slot-kind per commit with a String-PK e2e test.

Other open (user's call): Excel for #61, the genuinely-partial backlog (#26 correlated EXISTS, #29 iterator, #55 date hierarchy, #12/#10 playground). Everything is committed; nothing half-applied.

### P1 detail
- [x] 1a seed foundation (feature_set + seed_plugin_features + seed_orm_data cmd) — f1eb714
- [x] 1b /prebuilt backed (official plugins + features, dropped "more" strip) — f1eb714
- [x] 1c /community backed (SocialLink/Newsletter/CommunityResource) — 43c803c
- [x] 1e new pages — **every base.html nav/footer link now resolves (no 404s)**:
    - /features (FeatureCategory+FrameworkFeature, grouped) — ab5e56c
    - /reviews (Review testimonials) — ea76aec
    - /showcase (ShowcaseEntry, dogfooding-only, honest empty state) — c7ed7ab
    - /security (policy page, no DB) + /docs (landing, no DB) — aff126c
    - /changelog (curated, no DB) + /blog (BlogPost list, honest empty state) — 932a0fc
- [ ] 1d base nav + footer ← NavigationItem (currently hardcoded in base.html but RESOLVES; backing it is polish — needs an ambient context-processor; see spec §"nav injection")
- [ ] 1f /dashboard (the website's logged-in /dashboard, login-gated) → real widget queries (currently empty context, hardcoded template)
- [ ] 1g verify: render-crawl test for the new pages. Data-backed pages HAVE render tests (features/reviews/showcase/community/prebuilt). Static pages (security/docs/changelog) + /blog empty-state do NOT yet — add smoke-tests, and a /blog/{slug} detail route for when posts exist.

Verification status: every new page `cargo check`s clean; data-backed pages have passing render smoke-tests. The dev server at :8100 is running (cargo builds coexisted via target lock — never disrupted it).

Known ORM gap logged: planning/orm_fixes.md #1 (prefetch_related 2nd reverse-FK field → IN-batch workaround; proper fix = emit hydration arms for EVERY ReverseSet field in umbral-macros).

Pattern established for each page (replicate for remaining work): plugin gets `pub async fn seed()` (idempotent, editorial facts only — never fabricate metrics/adoption) + a `pub async fn render_*()` (IN-batch over N+1) + a `<plugin>/templates/<plugin>/*.html` extending base.html + tokio/tracing deps for on_ready + wire into `umbral_website/src/seed_command.rs` + a `tests/render_*.rs` (own test binary → own boot; register a TestStorage if the model has File/Image fields; one boot per binary since settings::init is one-shot).
