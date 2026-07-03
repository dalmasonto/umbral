# Seen/Known gaps - Continued from @gaps2.md

1. [x] REST `views([...])` means read-only everywhere (routes, OPTIONS Allow, OpenAPI spec, 405 vs 404) — archived
2. [ ] Push notifications implementations
3. [ ] Can one stream a video
4. [x] Flash messages no-op without a pre-existing session — resolved (works with SessionsPlugin) — archived
5. [ ] We need to offer auto SEO ie if a link lacks something like title, we inject it, if an image lacks alt, we use the image link as title, like how can we auto-magically help in terms of SEO
6. [x] Admin dashboard widget catalog filters by `widget.permission` — archived
7. [x] Custom-view paths validated at build (no router panic on reserved/duplicate paths) — archived
8. [x] Per-widget permission checks batched (concurrent, deduped) — archived
9. [x] REST nested writes are create-only; PATCH/PUT ignores nested child arrays — shipped

   `RestPlugin` supported writable nested children only on `POST`; the `update` handler was flat and ignored `cfg.nested`, so a PATCH carrying `{ "items": [...] }` handed the array to the ORM as an unknown column instead of upserting children.

   **Shipped:** `update` now splits declared nested arrays out of the body and upserts children on ONE `umbral::db::begin()` tx (parent update + child writes commit/roll-back together). **Reconciliation: upsert, no implicit deletes** — item WITH the child pk → update (scoped to this parent via the FK; a cross-parent pk is a 404); WITHOUT a pk → create. Rows absent from the payload are untouched. Full replace-set (delete-the-missing) stays a future opt-in (`ResourceConfig::nested_sync(...)`). Test: `plugins/umbral-rest/tests/nested_updates.rs`. Superseded/extended by #10 (recursion). The `update_json_in_tx`-return-is-not-affected-count footgun found here is captured in `.claude/skills/dynqueryset-update-return-semantics.md`.
10. [x] Nested writes only went one level deep; grandchildren were silently dropped — shipped

   `create_nested`/`update_nested` iterated only the parent's `.nested()` specs and inserted each child flat, so a level-3 array (e.g. `order.items[].components[]`) rode along inside the child object and — because the dynamic insert path iterates the child table's columns and validation doesn't flag unknown keys (`crates/umbral-core/src/orm/validation.rs:83`) — was **silently dropped**: no error, no rows. Silent data loss, the exact anti-pattern CLAUDE.md's "fix, don't patch" calls out.

   **Shipped:** both writers are now recursive (`insert_nested_tree` / `upsert_nested_child` in `plugins/umbral-rest/src/lib.rs`). Nesting is driven per table from `cfg.nested`, so a subtree is written iff its parent's table *also* declared `.nested(...)` — one level per declaration, arbitrary depth. Each level: FK auto-set from the parent's just-inserted pk (create) or ownership-scoped upsert (update); `MAX_NEST_DEPTH = 16` guards cyclic/self-referential declarations with a 400. Test: `plugins/umbral-rest/tests/nested_deep.rs` (3-level create + deep upsert + depth-3 cross-parent 404 rollback).

   **Follow-up (deferred):** declaring `.nested()` on a mid-level table also exposes it as a routed REST resource. If a caller wants deep nesting *without* exposing the intermediate table, we need a declaration that registers nesting without mounting routes (e.g. `ResourceConfig::for_::<T>().nested_only(...)` or a plugin-level nested-map). Log a new gaps3 entry if/when that's needed.
11. [ ] Auth JSON routes are slash-inconsistent with REST resources → `/api/auth/login/` 404s under the default `SlashRedirect::Append`

   Found building a real consumer backend (web3clubs_fc). `AuthPlugin::with_default_routes()` registers the JSON auth routes WITHOUT a trailing slash — `POST /api/auth/login`, `/register`, `/me`, `/logout` (`plugins/umbral-auth/src/auth_routes.rs:201-214`, `.route(&format!("{prefix}/login"), post(login))`). But `RestPlugin` resources use a TRAILING slash (`/api/fixture/`), and the `startproject` scaffold's `main.rs` turns on `.slash_redirect(SlashRedirect::Append)` by default. Net effect for a consumer: hitting `POST /api/auth/login/` — the natural thing to try, since every REST endpoint ends in `/` — returns **404** (the Append policy would redirect a no-slash request *to* the slash form, but the route registered is the no-slash form, so the slash form matches nothing). The no-slash form works, but the inconsistency is a silent footgun: it cost real debugging time (login appeared broken until the exact path was reverse-engineered), and a 307/308 redirect on POST wouldn't preserve the body anyway.

   **Impact:** every consumer that follows the REST plugin's trailing-slash convention (or whose HTTP client auto-appends) gets a confusing 404 on login/register — the first thing they wire up.

   **Fix options (pick one, be consistent):** (a) register the auth routes WITH a trailing slash to match REST resources (and let Append handle the no-slash form), or (b) register BOTH forms (like REST's collection routes already do — `{base}/{table}/` and `{base}/{table}`), or (c) have `with_default_routes()` read the app's `SlashRedirect` policy and register the matching shape. Option (b) is the most forgiving and mirrors what `RestPlugin::routes()` already does for collection paths. At minimum, document the exact (no-slash) auth paths prominently in `documentation/docs/v0.0.1/auth/` and the scaffold README, since the scaffold ships `SlashRedirect::Append` on by default.