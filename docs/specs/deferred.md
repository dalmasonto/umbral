# Deferred backlog

| | |
|---|---|
| **Status** | Living backlog. Items here are deliberately out of scope for M0–M13. |
| **Audience** | Whoever picks up the next iteration after the M0–M13 spine ships. |
| **Companions** | `docs/decisions/2026-05-30-spec-set-design.md §7.10` (the audit that surfaces these), `umbral-PRD.md §14` |

## What this file is

A pickup-ready backlog of Django capabilities umbral deliberately defers. Most are real Django features real teams use; they're deferred because they're niche (humanize, sitemaps), specialty domains worth their own focus (GIS, i18n), refinements of an existing surface that don't make sense until that surface stabilises (browsable API), or cross-cutting concerns that haven't accreted enough surface to deserve their own file yet (error model, logging).

Items can be reordered freely. The order below is grouped by category, not by priority. When you take one on:

1. Pick the item.
2. Promote its entry to a half-page outline at `docs/specs/outlines/<name>.md`, using the same skeleton the existing outlines follow. The metadata in this file becomes the seed.
3. Promote the outline to a deep spec at `docs/specs/<NN>-<name>.md` when its milestone is approached.
4. Implement. Ship a user-facing doc page per the CLAUDE.md "Documentation" rule.

To add a new deferred item: copy the entry shape from any item below, fill it in, slot it under the right category. Every entry below has the same fields.

| Field | Meaning |
|---|---|
| Django term | What porters look for when reading the Django docs. |
| Purpose | One sentence: what it does in Django. |
| Why deferred | One sentence: why it isn't in M0–M13. |
| Complexity | Small / Medium / Large, with the reason. |
| Suggested umbral shape | 1–2 bullets on what it would look like as a plugin. |
| Revisit signal | A concrete need or stakeholder ask that should trigger picking it up. |

---

## 1. Django `contrib` niceties

### `umbral-contenttypes`

| | |
|---|---|
| **Django term** | `django.contrib.contenttypes` |
| **Purpose** | Generic foreign keys — a single FK column that can reference rows across any model. |
| **Why deferred** | Niche outside Django's internals. Most ORM use cases prefer per-target FKs; generic relations cost type safety. |
| **Complexity** | Medium. Needs a `content_type` table, a `(content_type_id, object_id)` field-pair convention, a generic-relation type in the QuerySet API, and admin / REST integration. |
| **Suggested umbral shape** | Plugin owning a `content_type` table that mirrors Django's. Models opt in via a `#[umbral(generic_relation)]` attribute on the two-column pair. |
| **Revisit signal** | A real workload (admin audit log, tag-anything system, polymorphic comments) needs to FK into multiple model classes from one column. |

### `umbral-messages`

| | |
|---|---|
| **Django term** | `django.contrib.messages` |
| **Purpose** | Flash-message framework — queue a short note in one request, render it in the next response. |
| **Why deferred** | Rarely used standalone outside the Django admin. Modern apps either use frontend toast libraries or pass messages via direct response data. |
| **Complexity** | Small. Storage is a session-bound queue; the API is `messages.add(level, text)` plus a template tag. |
| **Suggested umbral shape** | Plugin with a `MessagesMiddleware` that reads from / writes to the session, an `umbral::messages::push(...)` helper, and a `{% messages %}` template tag. |
| **Revisit signal** | The admin or a user-facing app pattern (login flow showing "Welcome back, …") needs the queue UX. |

### `umbral-sites`

| | |
|---|---|
| **Django term** | `django.contrib.sites` |
| **Purpose** | Multi-tenant scaffolding — bind content to a `Site` (domain), let one install serve multiple branded sites. |
| **Why deferred** | A pattern from Django's CMS heritage that few greenfield projects need. Modern multi-tenant systems prefer row-level tenancy or per-tenant schemas. |
| **Complexity** | Small in isolation; the cost is that auth, admin, and content models all gain a `site_id` foreign key. |
| **Suggested umbral shape** | Plugin with a `Site` model, a `current_site()` ambient accessor populated per request, and an `#[umbral(per_site)]` model attribute that injects the FK. |
| **Revisit signal** | A user actually needs one umbral install to serve content scoped per domain. |

### `umbral-humanize`

| | |
|---|---|
| **Django term** | `django.contrib.humanize` |
| **Purpose** | Template filters for human-friendly rendering — time-ago, `intcomma` (1,234,567), `apnumber` (one / two / 99), ordinal. |
| **Why deferred** | Pure convenience. No load-bearing functionality blocks on it. |
| **Complexity** | Small. A handful of pure functions registered as template filters. |
| **Suggested umbral shape** | Plugin that registers filters on the `umbral::templates` engine: `{{ ts | time_ago }}`, `{{ n | intcomma }}`, etc. |
| **Revisit signal** | The admin or a built-in template wants nicer human-readable output, or a user asks. |

### `umbral-redirects`

| | |
|---|---|
| **Django term** | `django.contrib.redirects` |
| **Purpose** | DB-backed URL redirects — editors set up "old-path → new-path" rules without a code change. |
| **Why deferred** | Most teams handle redirects in nginx or a CDN, not at framework level. |
| **Complexity** | Small. A `Redirect(old_path, new_path, status_code)` model plus a middleware that consults it on 404. |
| **Suggested umbral shape** | Plugin with a `Redirect` model, a `RedirectFallbackMiddleware` that checks on 404, admin registration. |
| **Revisit signal** | A CMS-shape app, blog with mutable post slugs, or marketing site needs editor-controlled redirects. |

### `umbral-sitemaps`

| | |
|---|---|
| **Django term** | `django.contrib.sitemaps` |
| **Purpose** | Generate `sitemap.xml` from registered model sets so search engines crawl efficiently. |
| **Why deferred** | SEO concern. Many teams generate sitemaps in CI or via a build step rather than at request time. |
| **Complexity** | Small. A `Sitemap` trait; the plugin registers a `/sitemap.xml` route; the route walks every registered `Sitemap` and renders XML. |
| **Suggested umbral shape** | Plugin with a `Sitemap` trait (yields URLs + lastmod), a mount-point setting, per-plugin sitemap registration in `on_ready`. |
| **Revisit signal** | A public-facing umbral app needs SEO. |

### `umbral-syndication`

| | |
|---|---|
| **Django term** | `django.contrib.syndication` |
| **Purpose** | RSS / Atom feed generation from a model set. |
| **Why deferred** | RSS is a fading concern outside specific verticals (blogs, podcasts, news). |
| **Complexity** | Small. Shape mirrors sitemaps: a `Feed` trait, a renderer per format. |
| **Suggested umbral shape** | Plugin with a `Feed` trait, an `AtomRenderer` and `RssRenderer`, route registration via the plugin. |
| **Revisit signal** | A blog or news-shape app actually needs to publish a feed. |

### `umbral-flatpages`

| | |
|---|---|
| **Django term** | `django.contrib.flatpages` |
| **Purpose** | Lightweight CMS — DB rows hold path + title + body; a fallback view renders them. |
| **Why deferred** | Modern apps either reach for a full CMS or hard-code marketing pages. The flatpages middle ground rarely wins. |
| **Complexity** | Small. A `FlatPage` model, a fallback middleware that handles 404 by looking up the requested path. |
| **Suggested umbral shape** | Plugin with a `FlatPage` model, a `FlatPageFallbackMiddleware`, admin registration. |
| **Revisit signal** | A user needs editor-managed static pages alongside a real app. |

---

## 2. Specialty domains

### `umbral-gis`

| | |
|---|---|
| **Django term** | `django.contrib.gis` (GeoDjango) |
| **Purpose** | Geospatial field types (`Point`, `Polygon`, etc.), spatial queries (`within`, `intersects`), geographic backends. |
| **Why deferred** | Large standalone domain. Worth being its own crate with focused leadership rather than half-shipped in core. |
| **Complexity** | Large. Needs PostGIS integration, new field types with backend-specific declarations (Postgres + PostGIS only), a spatial query DSL on `QuerySet`, admin map widgets. |
| **Suggested umbral shape** | A separate crate `umbral-gis` that depends on the umbral facade. Field types `Point`, `Polygon`. Spatial predicate methods on field columns (`location.within(area)`). |
| **Revisit signal** | A user explicitly needs geo. Until then, leave it for someone with PostGIS expertise to lead. |

### `umbral-i18n`

| | |
|---|---|
| **Django term** | `django.utils.translation`, `LocaleMiddleware`, `USE_TZ`, `USE_I18N` |
| **Purpose** | Internationalisation — `gettext`-style string translation, locale-aware formatting, time-zone handling beyond UTC default. |
| **Why deferred** | Large concern. PRD §14 already excludes it. UTC-by-default in `DateTime<Utc>` covers most modern apps; full localisation is a focused workstream. |
| **Complexity** | Large. Translation infrastructure (catalogues, lazy translation, `{% trans %}` tag), a locale middleware that reads `Accept-Language`, a time-zone middleware that respects `Settings.time_zone`, locale-aware date and number rendering. |
| **Suggested umbral shape** | Plugin `umbral-i18n` that ships a `t!("string")` macro, a `LocaleMiddleware`, and template filters for localised dates and numbers. Probably rides on `fluent` rather than `gettext` so umbral is honest about not being a `.po` shop. |
| **Revisit signal** | A user explicitly needs multi-language UI. |

### `umbral-channels`

| | |
|---|---|
| **Django term** | `django.channels` (third-party but Django-shape) |
| **Purpose** | WebSockets, long-polling, and other extensions over HTTP for real-time features. |
| **Why deferred** | umbral's web layer needs to settle first. WebSockets in axum are already idiomatic; the value-add is the umbral-shape wrapper, which can't be designed before the deep web-layer spec lands. |
| **Complexity** | Medium. The wire protocol is axum's. What umbral owns is the Plugin-contributed handler shape (`fn ws_routes(&self) -> Vec<WsRoute>`?), auth and session integration, the broadcast-channel pattern. |
| **Suggested umbral shape** | Plugin or core extension that adds an `WsRouter` alongside `Router`, with `WsHandler` extractors that receive `Auth<User>` and `Session` automatically. Broadcast via tokio `broadcast::channel`. |
| **Revisit signal** | An umbral app needs real-time features (chat, notifications, collaborative editing). |

---

## 3. Backend and infrastructure

### MySQL / Oracle backend support

| | |
|---|---|
| **Django term** | `django.db.backends.mysql`, `django.db.backends.oracle` |
| **Purpose** | Run umbral against MySQL or Oracle instead of Postgres / SQLite. |
| **Why deferred** | PRD §14 declares Postgres-first explicitly. The system check (spec 05) catches incompatible field types at boot; the `DatabaseBackend` trait already leaves room for a new backend without restructuring. |
| **Complexity** | Medium per backend. Implement `DatabaseBackend` for the dialect (type mapping, quoting, RETURNING vs OUTPUT, upsert syntax). File backend-specific feature gaps (no `ArrayCol`, no `HStoreCol`). Verify the migration engine emits valid DDL. |
| **Suggested umbral shape** | A `MysqlBackend` (or `OracleBackend`) implementing `DatabaseBackend`. Lives in `umbral-core` next to `PostgresBackend` and `SqliteBackend`, or in a separate `umbral-mysql` crate if isolation matters. |
| **Revisit signal** | A user with a real MySQL or Oracle workload they can't migrate off. |

### Pluggable non-DB task brokers

| | |
|---|---|
| **Django term** | (Celery has `kombu`'s pluggable brokers: Redis, RabbitMQ, SQS, etc.) |
| **Purpose** | Let `umbral-tasks` run against Redis or AMQP instead of (or alongside) the default DB-backed broker. |
| **Why deferred** | The DB-backed broker is umbral's default for a reason — no extra infra. PRD §14 lists this as a future. |
| **Complexity** | Medium. The tasks outline already plans a `Broker` trait; this is "ship two more impls." The hard parts are at-most-once semantics across brokers, dead-letter handling, and broker interaction with retries. |
| **Suggested umbral shape** | `RedisBroker` and `AmqpBroker` implementing the same `Broker` trait. Opt-in via cargo feature. Backend-specific guardrails (Redis can't do strict ordering; AMQP can). |
| **Revisit signal** | A user needs higher throughput than DB-polling sustains, or already runs Redis / RabbitMQ. |

---

## 4. Tooling and UX

### DRF browsable API

| | |
|---|---|
| **Django term** | DRF's browsable API renderer |
| **Purpose** | Auto-generate an HTML browser interface for any REST endpoint, sitting next to the JSON renderer. |
| **Why deferred** | Lower-value once `umbral-openapi` (Swagger UI) ships. Swagger covers most of the same UX with more polish. |
| **Complexity** | Medium. A `BrowsableApiRenderer` that walks the serializer schema, renders an HTML form for input, pretty-prints the response. Cross-link to templates outline. |
| **Suggested umbral shape** | A renderer in `umbral-rest` that activates when `Accept: text/html` is requested. Reuses form rendering from `forms.md`. Off by default in Prod. |
| **Revisit signal** | A user wants Django-DRF-shape browsable APIs over Swagger. |

### Hosted deploy / runtime tooling

| | |
|---|---|
| **Django term** | (None native; community uses Heroku buildpacks, Render, Fly, etc.) |
| **Purpose** | A first-party way to deploy an umbral app to a managed runtime. |
| **Why deferred** | PRD §14 explicitly out-of-scope. Hosting is the user's choice; umbral ships a binary, not a deploy product. |
| **Complexity** | Large. Cuts across CI, container builds, secrets management, log shipping, scaling rules. |
| **Suggested umbral shape** | Probably never as part of the framework itself. The most likely deliverable is a small `umbral-cli deploy <target>` plugin pattern that emits Dockerfiles, `fly.toml`, k8s manifests as templates. |
| **Revisit signal** | A clear user pain that other Rust frameworks don't solve well. |

---

## 5. Cross-cutting concerns

These two aren't really Django features so much as umbral surfaces that haven't grown big enough to deserve their own spec yet. They live here so they don't get lost.

### Error model

| | |
|---|---|
| **Django term** | (Django doesn't have an explicit error-type taxonomy; uses Python exceptions.) |
| **Purpose** | A unified `umbral::Error` enum plus `From` impls so handler `?` flows, with a default `IntoResponse` that produces the right HTTP status per variant. |
| **Why deferred** | Currently cross-cutting: spec 03 says `umbral::Error` exists; spec 04 says validators emit `Error::Validation`; the web-layer outline open question #4 says "generic error → response mapping needs a home." No spec owns it end-to-end. |
| **Complexity** | Medium. Pin the enum (catalogue of variants), the `From` chain (sqlx, serde, validator, …), the default `IntoResponse` mapping. |
| **Suggested umbral shape** | Probably a deep spec `08-error-model.md` with the enum, the `From` impls, the `IntoResponse` mapping, and an extension hook for plugin-defined variants. |
| **Revisit signal** | Two consumers disagree on what an error variant should carry (web-layer outline open Q #4 names this trigger). |

### Logging

| | |
|---|---|
| **Django term** | `LOGGING` settings dict; `django.utils.log` |
| **Purpose** | A coherent story for what umbral logs by default, what spans handlers create, and what fields go on every log line. |
| **Why deferred** | `tracing` is the obvious answer (`arch.md §2.1` marks it visible to user code) and works fine ad-hoc. Becomes a spec only when umbral needs to mandate a span shape (every request span carrying `request_id`, `user_id`, `plugin`, etc.). |
| **Complexity** | Small. A convention doc plus a small `umbral::tracing` module with span helpers; not a new subsystem. |
| **Suggested umbral shape** | A short outline `logging.md` or a section in `arch.md`. Document the request-span schema, the conventions plugins should follow, the suggested `tracing-subscriber` setup for Dev vs Prod. |
| **Revisit signal** | A built-in plugin needs to log structured data and the team needs to agree on the schema. |
