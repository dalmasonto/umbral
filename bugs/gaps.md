## Status

**5 of 10 closed.** Last updated 2026-06-01.

| # | Gap | Status |
|---|-----|--------|
| 1 | Email plugin attachments | done — pending commit |
| 2 | Django-style 404 / 500 pages | done — `e42f408` |
| 3 | CLI: `umbra startproject` / `startapp` + dispatch shape | done — `95f0709` + `e5645a4` |
| 4 | Plugin docs + REST custom field rendering | done — `1cdbc18` (docs) + `dc84dbf` (code) |
| 5 | OpenAPI customisation params (`.description()`) | done — `e42f408` |
| 6 | Security audit across system + plugins | open |
| 7 | No `.create()` method on `Model` objects | open |
| 8 | No `.update()` method on `Model` objects | open |
| 9 | No `.delete()` method on `Model` objects | open |
| 10 | No bulk create/update/delete on `Model` objects | open |

Gaps 7–10 are one architectural shape (a write-side complement to the existing read-side QuerySet terminals); they're best tackled as a single Model-writes session. Gap 6 benefits from its own dedicated security-focused session.

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
6. [ ] Security audit across the system and the plugins
7. [ ] There is no `.create()` method on `Model` objects
8. [ ] There is no `.update()` method on `Model` objects
9. [ ] There is no `.delete()` method on `Model` objects
10. [ ] There are no bulk create/update/delete methods on `Model` objects
