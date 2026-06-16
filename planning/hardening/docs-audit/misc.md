# Documentation Audit — misc pages
<!-- Audited 2026-06-16. Read-only pass against actual code. -->

---

## templates/rendering-html.mdx

**Nit:** `"Shipped"` card (line 173) — lists `"Core custom filters (img, markdown, sanitize)"` but omits the three additional globals and functions that ARE registered at boot: `static()`, `media_url()`, and `highlight_styles()`. All three are wired unconditionally in `build_env` (`crates/umbra-core/src/templates.rs:717–738`). The card's claim isn't wrong, it's incomplete, and the incompleteness directly contradicts `templates/custom-tags.mdx` line 61 which already names `static` and `media_url` as built-ins. Fix: add `static`, `media_url`, `highlight_styles` to the card's filter list.

**Nit:** `"Worked example"` section (line 195) — doc claims `examples/derive-demo` has `"five templates"` (base + home + articles list + article detail + 404). Actual filesystem has **8** templates: `base.html`, `home.html`, `articles_list.html`, `article_detail.html`, `404.html`, `500.html`, `not_found.html`, `test-500.html` (`examples/derive-demo/templates/`). Fix: update count to eight (or just drop the enumeration).

**Nit:** `"Worked example"` section (line 195) — doc claims `"four routes serving HTML, one keeping the JSON shape"`. The actual route list in `examples/derive-demo/src/main.rs:238–246` is five HTML routes (`/`, `/500`, `/articles`, `/articles/{id}`, plus the auth default routes from `.with_default_routes()`) and one JSON route (`/api/articles`). At minimum there are five user-declared HTML routes. Fix: say "five routes serving HTML" or drop the count.

---

## templates/helpers.mdx

**Important:** Built-in filters/globals table (lines 18–25) — documents `img`, `markdown`, `sanitize`, `now`, `currency`, `csrf_token`, `csrf_input`, `user` but **omits three built-ins that ARE registered by `build_env`**: `static()` (template function, `crates/umbra-core/src/templates.rs:731`), `media_url()` (template function, line 738), and `highlight_styles()` (template function, line 722). A plugin author reading this table will not know these exist even though `custom-tags.mdx:61` names `static` and `media_url` in its registrar-ordering note. Fix: add rows for `static`, `media_url`, and `highlight_styles` to the table.

**Nit:** Line 41 — "The admin plugin additionally registers a `naturaltime` filter against its *private* environment." This is a reasonable claim but unverifiable from `crates/` alone (the admin plugin templates engine is in `plugins/umbra-admin/`). No code contradiction found; mark as unverified-but-plausible.

---

## templates/custom-tags.mdx

OK — `Plugin::template_registrars`, `TemplateRegistrar` type alias, `Box<dyn Fn(&mut Environment<'static>) + Send + Sync>`, the ordering note (registrars run after built-ins in plugin dependency order), and the `Fn`-not-`FnOnce` rule all match `crates/umbra-core/src/templates.rs:127`, `build_env:779–787`, and the module-level comments. The built-in list in line 61 (`img`, `static`, `media_url`, `markdown`, `now`, `currency`, `sanitize`) is accurate and complete for currently-registered items.

---

## realtime/sse.mdx

**Critical:** File ends with stray XML artifact `</content>\n</invoke>` (last two lines of the file). This will render as literal text or break any MDX parser. Same artifact is present in `realtime/scaling.mdx`. Fix: delete those trailing two lines from `sse.mdx`.

**FYI:** Module-level doc in `plugins/umbra-realtime/src/lib.rs:19` says routes are `GET /realtime/sse` and `GET /realtime/ws`; `sse.mdx` line 24 says `RealtimePlugin::default()` `"mounts /realtime/sse + /realtime/ws"`. Matches `Plugin::routes` at `lib.rs:745–749`. OK.

**FYI:** `sse.mdx` line 139 cites design at `docs/superpowers/specs/2026-06-13-umbra-realtime-design.md`. File exists at that exact path. OK.

---

## realtime/scaling.mdx

**Critical:** File ends with stray XML artifact `</content>` (last line). Will render as literal text in MDX. Fix: delete that trailing line from `scaling.mdx`.

**FYI:** Claims `RedisBroker` pub/sub channel is `"umbra:realtime:events"`. Matches `plugins/umbra-realtime/src/lib.rs:299` (`const CHANNEL: &'static str = "umbra:realtime:events"`). OK.

**FYI:** Claims the `redis` feature gates `RedisBroker`; `RealtimePlugin::redis()` method is `#[cfg(feature = "redis")]` at `lib.rs:664`. OK.

**FYI:** References integration test at `plugins/umbra-realtime/tests/broker.rs`. File exists at that path. OK.

---

## testing/test-client.mdx

OK — `TempPool`, `TestClient`, `TestResponse` all exist in `crates/umbra-testing/src/lib.rs`. Every method cited in the doc matches the actual public API:
- `get(uri)`, `delete(uri)` — bodyless, line 172/204
- `post(uri, body)` — raw Body, line 176
- `post_json(uri, &value)` / `put_json(uri, &value)` — JSON with Content-Type, lines 182/193
- `send(method, uri, body)` — generic, line 210
- `set_default_header(name, value)` — line 156
- `cookie(name)` — line 164
- All `TestResponse` assertions (`assert_status`, `assert_status_ok`, `assert_body_contains`, `assert_header`, `body_text`, `body_bytes`, `body_json`, `status`, `headers`, `header`) — lines 275–345
- Cookie jar described as automatic (stateful jar in `request()`, lines 245–248). OK.

---

## testing/factories.mdx

OK — `Factory` trait, `build()`, `create()`, `create_with(tweak)`, `create_batch(n)`, `seq()`, and the `fake` re-export all exist and match in `crates/umbra-testing/src/lib.rs` (lines 432–475, 354, 365). The orphan-rule rationale for the marker-type pattern is accurate. The `create_with` signature matches: takes `FnOnce(&mut Self::Model) + Send` (line 456). The ambient-pool dependency note for `create*` is accurate (calls `Manager::default().create(...)`, line 461).

---

## examples/basic.mdx

**Important:** `src/main.rs` example (line 76) calls `umbra::migrate::make().await.ok()` then `umbra::migrate::run().await?`. Both exist in the codebase. However, the example uses `edition = "2024"` in `Cargo.toml` (line 28), which matches actual framework examples (`examples/hello/Cargo.toml:4`, `examples/derive-demo` implicit). OK.

**Nit:** The `Cargo.toml` example lists `sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }` (no `"chrono"` feature). The model struct has `pub created_at: chrono::DateTime<chrono::Utc>` which requires `sqlx`'s `chrono` feature for `FromRow`. The scaffold in `crates/umbra-cli/src/scaffold.rs:306` emits `sqlx = { version = "0.8", features = ["macros", "sqlite", "postgres", "chrono", "runtime-tokio"] }` (includes `"chrono"`). Fix: add `"chrono"` to the `sqlx` features line in the `basic.mdx` Cargo example, or remove the `DateTime` field from the struct.

---

## examples/batteries-included.mdx

**FYI:** References `umbra_email::EmailPlugin` and `umbra_email::EmailMessage`. Plugin crate `plugins/umbra-email/Cargo.toml` exists. No source-level verification of the specific API surface was done (out of audit scope). No contradictory claim found.

**Nit:** `blog.rs` example (line 95) uses `Post::objects().get(post::ID.eq(id))` — the `get()` method in the codebase takes a filter expression. Matches the ORM query API documented elsewhere. OK.

**Nit:** `main.rs` example (line 278) registers `umbra_tasks::TasksPlugin` with no argument. Matches usage pattern seen in `examples/shop/`. OK.

---

## examples/rest-api-service.mdx

**Nit:** `.env` example (line 53) writes `UMBRA_DATABASE_URL=postgres://...` with a note that "a bare `DATABASE_URL` would be ignored". This matches the `figment` loader in `crates/umbra-core/src/settings.rs:341` which uses `Env::prefixed("UMBRA_").split("__")`. OK.

**Nit:** `src/auth.rs` uses `api_token::TOKEN` and `api_token::REVOKED` column constants (lines 114–115). These are auto-generated by `#[derive(Model)]` for the `ApiToken` model. Pattern is consistent with how every other model in the codebase uses column constants. OK.

**Nit:** `Identity::user_id` is described as a `String` (line 198: `"Identity::user_id is a String (PK-type-agnostic)"`). Cannot verify against `plugins/umbra-rest/src/` without reading that crate but the comment is self-consistent with the example code. Unverified-but-plausible.

---

## getting-started/your-first-app.mdx

OK — `Settings::from_env()`, `umbra::db::connect()`, `App::builder()`, `.settings()`, `.database()`, `.routes()`, `.build()`, `app.serve()` all exist. The `sqlite::memory:` default warning matches `crates/umbra-core/src/settings.rs` (`fn default_database_url` returns `"sqlite::memory:"`). The five `App::build()` phases description (collect, detect backend, publish ambient state, run system checks, merge plugin routers) is narrative and not directly verifiable from a public API; no contradiction found.

---

## getting-started/settings-and-env.mdx

**Nit:** `Settings` struct code block (lines 22–35) lists `extra: HashMap<String, toml::Value>`. The actual field in `crates/umbra-core/src/settings.rs:303` is `pub extra: std::collections::HashMap<String, toml::Value>`. The doc shortens to `HashMap<String, toml::Value>` (omitting `std::collections::`). This is a cosmetic difference, not a correctness issue. OK as-is.

**FYI:** `UMBRA_DATABASES__<NAME>` (double underscore) env var syntax (line 42) is verified: `settings.rs:341` uses `Env::prefixed("UMBRA_").split("__")` which maps `UMBRA_DATABASES__REPLICA` to `databases.replica`. OK.

**FYI:** `umbra::settings::get()` and `umbra::settings::get_opt()` are both documented and both exist in `crates/umbra-core/src/settings.rs:30,36`. The `&'static Settings` return type claim is accurate (the `OnceLock` stores `Settings` and returns `&'static`). OK.

**FYI:** `extra_str` accessor (line 144) matches `settings.rs:323`. OK.

**Important:** Precedence summary (line 180) lists step 5 as `"The --addr flag on umbra serve (one-off override)"`. The `umbra serve` command with `--addr` is NOT part of the project-binary dispatch path documented in the scaffolded `main.rs`. The `umbra` binary (the global CLI from `umbra-cli/src/main.rs`) handles `startproject`/`startapp`/`startplugin`; the per-project binary (`cargo run -- serve`) uses `umbra_cli::dispatch(app)`. Checking that `serve --addr` actually exists as a flag in `dispatch` is needed. Cross-reference: `crates/umbra-cli/src/lib.rs` — not read in this pass. Mark as **unverified**. If `--addr` is not a real flag on `cargo run -- serve`, step 5 in the precedence list is a phantom feature.

---

## about.mdx

OK — The `Callout` block (line 13) accurately enumerates shipped features: models, migrations, inspectdb, auth, sessions, admin, REST, OpenAPI, tasks, permissions, security, static, templates, CLI (`cargo run -- <command>` + `umbra` binary). The list matches the plugin crates present in `plugins/`. The `Results::from_env()` / `ok?` / `app.serve()` code snippet (lines 67–75) is accurate for the current API (verified against `crates/umbra-core/src/`).

**Nit:** `about.mdx` Roadmap section (lines 90–121) lists "Notifications plugin (SSE)" as a backlog item (line 98). But `plugins/umbra-realtime/` is already shipped and includes SSE push. If the "Notifications" backlog item refers specifically to a *higher-level notification abstraction* (as opposed to the raw SSE/WS transport), this is fine. If it's meant to describe SSE itself, it should be moved out of the roadmap. Low severity — depends on interpretation of "Notifications plugin" vs. `umbra-realtime`. Mark as **FYI**.

---

## Summary

| Severity | Count |
|---|---|
| Critical | 2 |
| Important | 3 |
| Nit | 7 |
| FYI | 7 |

**Worst 3 findings:**

1. **Critical — `realtime/sse.mdx` and `realtime/scaling.mdx` both end with stray XML artifacts** (`</content>` / `</invoke>`). These will render as literal garbage in MDX or break the parser entirely. Both files need the trailing artifact lines deleted.

2. **Important — `templates/helpers.mdx` built-in table is missing three registered globals** (`static()`, `media_url()`, `highlight_styles()`). A plugin author or template author reading the table will not know these exist. The `custom-tags.mdx` page already names `static` and `media_url` in its built-in list, creating an inconsistency across the two pages.

3. **Important — `examples/basic.mdx` `Cargo.toml` example is missing `"chrono"` in sqlx features** while the model struct uses `chrono::DateTime<chrono::Utc>` with `sqlx::FromRow`. Copying the example verbatim produces a compile error. The scaffolder (`crates/umbra-cli/src/scaffold.rs:306`) already gets this right.
