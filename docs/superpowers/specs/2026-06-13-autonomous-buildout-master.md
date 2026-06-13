# Autonomous build-out — master plan of record

Date: 2026-06-13
Status: executing
Driver: autonomous (user away; full authority granted to spec + build + close)

This is the plan-of-record for a large autonomous mandate. It survives context resets — a future session reads this to know what's done, what's next, and why each decision was made. Each phase commits independently; check `git log` + the per-item trackers (`planning/features.md`, `planning/REAL-GAPS.md`, `planning/orm_fixes.md`) for live status.

## The mandate (verbatim intent)

1. **Every umbra_website page backed by real ORM data — zero dead links, zero 404s.** Existing hardcoded pages → DB; orphan models → new pages; nav links must all resolve.
2. **`seed_orm_data` CLI command** — expand the seed to cover plugins, their features, and all data every page needs.
3. **Testing & factory library (features.md #79, #52; REAL-GAPS A#18)** — "a way to test out your models, your website." Prefer a plugin/crate. Factories that simulate realistic plugin submissions + model updates. Document well. (#74 fixtures already shipped; #79 adds factories + test client.)
4. **Close features.md #56–61** — admin widgets, autocomplete (FK/M2M/O2O search — mostly there), REST nested writes, REST auth (already wired — verify+close), REST action schemas, CSV/Excel.
5. **Implement features.md #65–70** — migration-safety command, collectstatic+compression, template tags/filters hook, middleware pipeline trait, DB routers, streaming/compressed responses.
6. **Review features.md + REAL-GAPS.md** — mark done items, pick low-hanging fruit (any number).
7. **Spec + plan + start SSE/WebSockets** (features.md #45 `umbra-realtime`) — heavy plugin; must support user-specific AND group/room-specific delivery. Detailed design below.

## Operating rules (from the user + CLAUDE.md)

- Never wipe the DB or migration files; seeds are idempotent (per-row/per-table short-circuit).
- Never `git stash` / discard the dirty tree.
- Never touch the running dev server (`:8100`); don't `cargo run`/`pkill` examples.
- Don't `cargo build` after every edit — batch per unit, build once to confirm before commit.
- If disk fills: `rm -rf crates/target` (this project only) + the stray `umbra_website/plugins/public/target`; then continue.
- Optimized ORM queries (no N+1). **If a query can't be expressed, fix the ORM and append to `planning/orm_fixes.md`** (file, failing query, fix, test) — then continue.
- One cohesive unit per commit. Ship a feature → ship its doc page (`documentation/docs/v0.0.1/...`).
- Don't fabricate metrics (github_stars/downloads stay `—` when unknown). Curated editorial facts (status/maturity/features/audit) ARE legitimate to seed.

## Execution order (top-down; commit each unit)

### PHASE 1 — Website, zero 404s  ← START HERE
Foundation then pages. Data lives in each plugin's `seed()`; `seed_orm_data` orchestrates.

- **1a. Seed foundation.** Add `feature_set: ReverseSet<PluginFeature>` to `Plugin` (mirrors `comment_set`; virtual, no migration). Extend `plugin_directory::seed` to seed `PluginFeature` rows per official plugin (curated feature lists w/ status+maturity+display_order — see prebuilt.html for admin/rest/auth lists; author the rest). Add `pub async fn seed()` to `community`, `features`, `showcase`, `site_content`. New `seed_orm_data` command via a tiny `SeedPlugin` in the binary crate (`umbra_website/src/`), registered in `main.rs`; `umbra_cli::dispatch` picks up `Plugin::commands()`. Keep `on_ready` minimal auto-seed so the live site never boots empty.
- **1b. `/prebuilt`** — load `source=official, moderation=approved` plugins, `.prefetch_related("feature_set")` (2 queries). One detailed card per official plugin with its real feature tracker (reuse detail page's `feature_status()` status→dot mapping). DELETE the "More official plugins" strip. Fix the literal `{}` icon bug.
- **1c. `/community`** — `SocialLink` (channels), `CommunityResource` (resources), `NewsletterConfig` (subscribe band). Replace hardcoded template blocks. SentinMail URL stays.
- **1d. `base.html` nav + footer** ← `NavigationItem`. Inject via an ambient context-processor mirroring `with_user_in_templates()` (load once into a `OnceLock`, refreshed by seed). If a framework hook is needed beyond the user-injection pattern, build it (it's the right contract) and document; fall back to a `nav_context()` helper spread into each handler only if the hook is genuinely out of reach. NavigationItem placements: `header` (with optional dropdown group) + `footer` (grouped sections).
- **1e. New pages (wire dead nav links):** `/features` (FrameworkFeature grouped by FeatureCategory, status badges), `/showcase` + maybe `/showcase/{slug}` (ShowcaseEntry gallery, approved/featured), `/blog` + `/blog/{slug}` (BlogPost published, markdown body, M2M tags), `/changelog` (FeatureStatusEvent timeline, or curated release notes). Each: route + view-model + template (frontend-design quality, match existing token system) + seeded content. Add a `/healthz`-style is not needed here.
- **1f. `/dashboard`** — point existing widgets at real queries (it's login-gated admin demo data today).
- **1g. Verify:** crawl every nav/footer link + in-page link; assert 200 (a render smoke-test like `plugin_directory/tests/render_pages.rs`). No 404, no dead link.

### PHASE 2 — Testing & factory library (#79)
New crate `umbra-test` (a dev-dependency crate, not a runtime plugin — but expose a `Factory` trait usable anywhere). Surface:
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
- features.md: flip done boxes, archive write-ups, fix any stale status. REAL-GAPS.md: update the Umbra-status column for everything now shipped (OAuth A#12 ✅, file uploads A#10 ✅/⚠, health A→B#7 ✅, etc.), close done, harvest low-hanging fruit (e.g. `/healthz` already exists; structured-logging JSON layer; `?utm`; small ones).

### PHASE 6 — SSE / WebSockets (`umbra-realtime`, #45)  [detailed below]
Spec + plan + start. Heavy plugin.

---

## SSE / WebSockets detailed scope (`umbra-realtime`)

Goal: a developer can push **user-specific** data ("notify user 42") AND **group/room-specific** data ("everyone in `chat:123`" / "all staff") without hand-rolling channel bookkeeping. Built on axum's native SSE + `tokio-tungstenite` for WS, and the signals system (#38) so model changes can fan out.

### Core model — a broker over tokio broadcast/mpsc

```rust
// umbra-realtime
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
- **Auth-aware:** a connection's `user_id` comes from the session/bearer identity at handshake (reuse umbra-auth's `resolve_identity`). Anonymous connections can still join public groups but `to_user` never targets them.
- **Group membership:** explicit `Realtime::join(conn_or_user, group)` / `leave`, OR declarative at subscribe time — the client opens `GET /realtime/sse?groups=chat:123,presence` and the server validates each requested group against an app-provided `GroupPolicy` (so a user can't subscribe to `tenant:99` they don't belong to). `GroupPolicy::can_join(identity, group) -> bool` is the security seam — default deny for non-public groups.

### Transports
- **SSE** (`GET /realtime/sse`): axum `Sse<impl Stream>`; the per-conn `mpsc::Receiver` becomes the event stream; heartbeat keep-alive comment every ~15s. This is the default (simplest, proxy-friendly, unidirectional server→client). Phase-2 `iterator()` Stream (features.md #29) lands `futures-util` here.
- **WebSocket** (`GET /realtime/ws`): axum `WebSocketUpgrade` + `tokio-tungstenite`; bidirectional. Inbound client messages dispatch to an app `MessageHandler` (chat send, presence ping). Outbound shares the same per-conn sink.

### Signals bridge
`RealtimePlugin::on_model::<Post>(|ev| Realtime::to_group(format!("post:{}", ev.pk)).send("updated", ...))` subscribes to `post_save`/`post_delete` (#38) and fans out — the "live dashboard/notifications" story with zero polling. The admin notification bell (#2) becomes one consumer: `to_group("staff")` on relevant signals.

### Scaling note (documented, not built v1)
Single-process broadcast works for one instance. Multi-instance needs a backplane (Redis pub/sub or Postgres LISTEN/NOTIFY) so `to_user(42)` reaches the instance holding that socket. Design the registry behind a `Broker` trait now (`publish(envelope)` / `subscribe()`), ship `InProcessBroker`, leave `RedisBroker` as the documented Phase-2 swap. This mirrors the alerts/cache backplane direction.

### Deliverables (in order)
1. Spec doc `docs/superpowers/specs/2026-06-13-umbra-realtime-design.md` (this section, expanded).
2. `umbra-realtime` crate: broker + registry + SSE transport + `Realtime` ambient + `GroupPolicy`.
3. WS transport + `MessageHandler`.
4. Signals bridge + `on_model`.
5. A demo on `umbra_website` (live plugin-submission feed to `staff`, or a presence counter) + playground "Realtime" tab (features.md #10 unblocks).
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
- [x] **P2 testing** — `Factory` trait added to umbra-testing (build/create/create_with/create_batch + `seq()` + `fake` re-export); marker-type shape for the orphan rule; doc pages (testing/factories + test-client). Commit 517fc84. (#79/#52 ✓)
- [~] **P3 #56–61** — #56/#57/#59 closed (c37e928); **#58 nested writes SHIPPED** (9ab22e8; true-DB-tx → orm_fixes #2); **#61 CSV export SHIPPED** (`?format=csv` on the list endpoint — 8103f70); **#60 action schemas SHIPPED** (1dea66a — `ResourceConfig::action_input_schema`/`action_output_schema`; body validated pre-handler → 400 on mismatch; `registered_action_schemas()` → OpenAPI path items; subset validator, full schema still ships to OpenAPI; `tests/action_schemas.rs`; doc `rest/actions.mdx`). **Still open:** #61 remainder (CSV import command, admin bulk-export action, Excel). Realtime SSE+WS retested green after the target wipe (full clean rebuild).
- [~] **P4 #65–70** — **#66 SHIPPED** (gzip/brotli compression via `AppBuilder::compression()`; collectstatic + `{% static %}` tag already existed — 5497823). **#65 SHIPPED** (c38dfce — `umbra checkmigrations [--strict]`: `migrate::classify_operation` tags pending ops SAFE/WARNING/UNSAFE, exits non-zero on unsafe → CI gate; `check_pending_safety[_in]` programmatic; pure + e2e tests; doc `migrations/checkmigrations.mdx`; full expand-contract ops guide still deferred). Remaining: #67 template tags/filters Plugin hook (friction: ENGINE is a built-once OnceLock, so the `&mut Environment` hook must thread through `templates::init`/`build_env`), #68 middleware-pipeline trait, #69 DB routers, #70 streaming response bodies.
- [~] **P5 trackers** — REAL-GAPS.md updated for shipped items (4b33b07). features.md #56/#57/#59 closed.
- [x] **P6 realtime CORE** — spec (60662b8) + phase 1 (0cb17bc) + phase 2 SSE (8035f9e) + phase 3 WebSocket (af86351) + phase 4 signals bridge (388d49e) + **live demo: SSE note feed on plugin pages** (e156ad5 — create_note broadcasts to `public:plugin-<id>` watchers; detail page EventSource banner; `Realtime::send` made graceful when uninstalled). features.md #45 core closed; 10 tests. **Deferred:** multi-instance Redis broker, the playground "Realtime" tab (#10).

Next-session priority order: P4 #67–70 → P3 #61 remainder. Everything is committed; nothing half-applied.

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

Known ORM gap logged: planning/orm_fixes.md #1 (prefetch_related 2nd reverse-FK field → IN-batch workaround; proper fix = emit hydration arms for EVERY ReverseSet field in umbra-macros).

Pattern established for each page (replicate for remaining work): plugin gets `pub async fn seed()` (idempotent, editorial facts only — never fabricate metrics/adoption) + a `pub async fn render_*()` (IN-batch over N+1) + a `<plugin>/templates/<plugin>/*.html` extending base.html + tokio/tracing deps for on_ready + wire into `umbra_website/src/seed_command.rs` + a `tests/render_*.rs` (own test binary → own boot; register a TestStorage if the model has File/Image fields; one boot per binary since settings::init is one-shot).
