## Status

**10 of 10 closed.** Last updated 2026-06-01.

| # | Gap | Status |
|---|-----|--------|
| 1 | Email plugin attachments | done — `abf502c` |
| 2 | Django-style 404 / 500 pages | done — `e42f408` |
| 3 | CLI: `umbra startproject` / `startapp` + dispatch shape | done — `95f0709` + `e5645a4` |
| 4 | Plugin docs + REST custom field rendering | done — `1cdbc18` (docs) + `dc84dbf` (code) |
| 5 | OpenAPI customisation params (`.description()`) | done — `e42f408` |
| 6 | Security audit across system + plugins | done — `cd656d5` |
| 7 | No `.create()` method on `Model` objects | done — `9ab3c00` |
| 8 | No `.update()` method on `Model` objects | done — `9ab3c00` |
| 9 | No `.delete()` method on `Model` objects | done — `9ab3c00` |
| 10 | No bulk create/update/delete on `Model` objects | done — `9ab3c00` |

All known gaps closed. New gaps land below as they're surfaced.

## Seen/Known gaps

1. [x] Email plugin lacks the ability to do file attachments
       — Shipped: `Attachment { filename, content_type, data: Vec<u8> }` struct + `EmailMessage::attach(filename, content_type, bytes)` builder. Bytes-only by intent (no path-loading, no auto content-type, no inline images) — users with files do `std::fs::read(path)?` themselves; users wanting any of the deferred shapes get them when a real consumer surfaces a need. When attachments are present, `compose()` wraps the existing body in `multipart/mixed` with the body part(s) (text-only / html-only / alternative) nested inside per RFC 2046. 11 new tests in `tests/attachments.rs` pin the on-wire MIME shape (multipart/mixed wrapper, Content-Disposition: attachment, base64 transfer encoding for binary payloads, alternative-inside-mixed for text+html+attachment). See `documentation/docs/v0.0.1/plugins/email.mdx`.
2. [x] Django has special way of doing pages ie 404, 500 (Internal server error) which is quite symbol and direct
       — Shipped via `App::builder().not_found_template("404.html")` and `.server_error_template("500.html")`. Composes with `slash_redirect`. See `documentation/docs/v0.0.1/web/error-pages.mdx`.
3. [x] The current version does not make use of the cli. Ie instead of `cargo run`, you must use `umbra serve` to run the server. Or even better, I need something like umbra startproject, startapp commands to set up a new project like django. We need to make it easy to setup the app that way with `apps`. We talked about this, it might be implemented but docs are out of sync by far.
       — Shipped via commits 95f0709 (code) + e5645a4 (docs). `cargo install umbra-cli` installs a global `umbra` binary with `startproject` + `startapp`. User binaries call `umbra_cli::dispatch(app).await` from `main.rs` to host the management subcommands (serve / migrate / makemigrations / inspectdb / dumpdata / loaddata). `cargo run -- serve` is the project-local way to invoke them. See `documentation/docs/v0.0.1/cli/startproject.mdx` + the rewritten `management-commands.mdx`.
4. [x] Docs are out of sync with the current version ie how to use plugins. There is no plugin section on how to use `rest` plugin for example. So the rest plugin should make it easy to create rest views with minimal effort and with the ability to use custom fields like in django we define the view and then the fields, currently it returns the whole struct/model. DRF classes make it easy to even redefine fields ie you can hide emails ie using a method like get_email(obj) and it returns "***@mail.com" instead of the actual email by spliting at the `@` symbol and replacing the username with `***`.
       — Docs portion: rest.mdx + openapi.mdx + rls.mdx + sessions.mdx + tasks.mdx + admin.mdx + web/ section all landed (commit 1cdbc18).
       — Custom field rendering shipped: `RestPlugin::hide(table, field)`, `.transform(table, field, |v| ...)`, `.computed(table, name, |row| ...)`. Per-row, per-table, applied in hide→transform→computed order on every outbound response. The example `transform("user", "email", mask_email)` in rest.mdx now matches exactly the DRF `get_email(obj)` pattern called out in the gap. 8 new tests in `tests/field_overrides.rs`.
5. [x] The openapi plugin should allow the user to pass in params to customize the openapi schema. ie the base path, title, version, description, etc.
       — `.description(s)` added in commit e42f408; `.at()`, `.title()`, `.version()`, `.exclude()` already existed.
6. [x] Security audit across the system and the plugins
       — Shipped via `cd656d5`. Four parallel audit agents (ORM/SQL, Auth+Sessions+RLS, Templates+XSS+Headers, CLI+Scaffolding+IO) produced triaged reports. Combined findings: 2 HIGH (both fixed), 6 MEDIUM (all fixed or documented), 8 LOW (key ones fixed, rest acknowledged in canary comments). HIGH fixes: session tokens SHA-256 hashed before DB storage so a DB leak doesn't surrender live sessions; CSRF compare uses `subtle::ConstantTimeEq`. MEDIUM fixes: defense-in-depth for dev secret_key when bind_addr isn't loopback; `model.table` quote-doubled in REST/admin/backup SQL; RLS using/with_check raw-SQL trust boundary documented with `# SQL injection warning` sections; admin sanitises sqlx errors to a fixed string (logs full error server-side); Content-Type in error pages derived from render result (not template-was-set). LOW: CRLF redirect canary comment, autoescape extension-whitelist sync warning, ConsoleBackend warns in non-dev environments. Major verified-clean surfaces: argon2id w/ per-password CSPRNG salt, account-enumeration defense, HttpOnly+Secure+SameSite cookie flags, autoescape on by default for HTML, no open redirects, scaffold name validation blocks path traversal, no secret logging, lettre's typed APIs for email headers (no manual construction → no CRLF injection in From/To/Subject/filename).
7. [x] There is no `.create()` method on `Model` objects
       — Shipped via `Manager::create(instance)` in commit 9ab3c00. INSERT + RETURNING * for the populated row. Autoincrement sentinel: PK == 0 (ints), nil UUID, or empty String are omitted from the INSERT so the DB assigns them; explicit non-default PKs are bound as supplied. Requires `T: serde::Serialize` (which models already derive for REST).
8. [x] There is no `.update()` method on `Model` objects
       — Shipped via `QuerySet::update_values(map)` in commit 9ab3c00. Takes a `serde_json::Map<String, Value>` of column → new value. PATCH semantics (absent keys keep current values). PK in the map is silently skipped to prevent identity rewrite while filtering on the old PK. Unknown columns raise `WriteError::UnknownColumn` early.
9. [x] There is no `.delete()` method on `Model` objects
       — Shipped via `QuerySet::delete()` in commit 9ab3c00. Applies accumulated filter predicates as WHERE; returns affected-rows count. Without filter calls, deletes every row — same semantics as raw SQL DELETE FROM.
10. [x] There are no bulk create/update/delete methods on `Model` objects
       — All covered by gaps 7-9's primitives: `Manager::bulk_create(Vec<T>)` produces one multi-VALUES INSERT (cheap, batched); `QuerySet::filter(...).update_values(map)` and `QuerySet::filter(...).delete()` are naturally bulk via the WHERE clause. 10 new tests in `tests/model_writes.rs` + 5 unit tests in the `crate::orm::write` module pin the JSON→sea_query::Value dispatch. See the new "Writing rows" section in `documentation/docs/v0.0.1/orm/models.mdx`.
