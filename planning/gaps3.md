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
11. [x] Auth JSON routes slash-inconsistent with REST → `/api/auth/login/` 404s — archived (fixed, commit 4f30cc4)
12. [x] `GET /oauth/{provider}/login` 500s for an unregistered provider key — archived (fixed, commit e6efb7a)
13. [x] SQLite `AlterColumn` fails with FK-constraint on a table with inbound FKs — archived (fixed, commit a60405a)
14. [x] `update_or_create` UPDATE branch emits bulk_post_save not per-row post_save — archived (fixed, commit fe200c1)

   **Options:** (a) have `update_or_create`'s update branch fetch-and-`save()` the single row (per-row `post_save`) instead of `update_values`, so the whole API is per-row-signal-consistent; or (b) at minimum document loudly that `on_model`/`post_save` won't see `update_or_create` updates and point consumers at `save()` or an explicit push. **Workaround used:** pushed the payment notification explicitly from the handler with `Realtime::to_user(...)` rather than relying on the `on_model` bridge.

---

_Entries #15–#25 harvested from the web3clubs_fc backend (a live consumer; see [[project_web3clubs_fc_backend]]). Findings verified against umbral 0.0.5's actual surface — the app is on 0.0.4, so a few of its workarounds are already resolved (SQLite alter-with-inbound-FK #13, object-scope reads via `ResourceConfig::owned_by`/`.scope`, and `umbral_auth::validate_password` all now exist)._

15. [x] No `IntoResponse` for ORM errors → every handler re-declares `err500` and sprinkles `.map_err(err500)?` — shipped (commit 0763d0c3)

    In the consumer, all 5 plugins open with an identical `fn err500<E: Display>(e: E) -> (StatusCode, String)` and every ORM terminal is `.map_err(err500)?`. The highest-volume boilerplate in the app. REST already has `impl IntoResponse for ApiError` + `From<WriteError>` (`plugins/umbral-rest/src/lib.rs:2222,2254`), but it's REST-internal — plain axum handlers can't reach it.

    **Proposal:** lift an `ApiError` (with `From<WriteError>`/`From<sqlx::Error>` + `IntoResponse`, safe-by-default opaque 500 like WEB-5) to `umbral-core` and re-export from the facade, so a plain handler returns `Result<Json<T>, umbral::ApiError>` and uses bare `?` on ORM calls.

16. [x] REST has read scoping (`owned_by`) but no owner-*injection* on create (`perform_create`) — shipped (commit 4746e946)

    `ResourceConfig::owned_by("col")` / `.scope(...)` filter reads/updates to the caller's rows (audit_2 H1/P2), which the consumer didn't have on 0.0.4 (it hand-rolled `GET /api/me/*`). But there is still no way to *fill* an owner FK from the authenticated identity on **create** and reject a body-supplied value — so every "the member comes from the token, never the body" write (RSVP, chat post, payment record) bypasses REST for a bespoke handler.

    **Proposal:** `ResourceConfig::owner_field("member")` — on create, set the FK from the identity; ignore/reject a client-supplied value. Collapses most of the app's bespoke write handlers back into declarative REST.

17. [x] No lightweight typed current-user extractor — handlers parse `identity.user_id: String → i64` (~8×) — shipped (commit d84e91e2)

    `LoggedIn<AuthUser>` exists but does a DB fetch; the token-only `Identity` gives `user_id: Option<String>` (the PK-LCD), so every scoped handler repeats `let uid: i64 = identity.user_id.parse().map_err(|_| (UNAUTHORIZED, ...))?`.

    **Proposal:** `Identity::user_pk::<T: FromStr>() -> Result<T, _>` and/or a `CurrentUserId<T>(pub T)` extractor (no fetch, 401 on parse failure) generic over the app's PK type.

18. [x] No permission-gated extractor for plain handlers — `require_staff` copy-pasted across plugins — shipped (commit c44c8a0c)

    REST `Permission` types (`IsStaff`, etc.) can only gate viewsets, so the app re-declares an identical `require_staff(&Identity) -> Result<i64, ApiErr>` in `fc-teams` and `fc-payments`.

    **Proposal:** a `Require<P: Permission>` extractor (403s on failure) usable on any axum handler, plus a `RequireStaff(pub i64)` convenience that returns the parsed uid.

19. [x] `AuthUser` isn't extensible — confirmed already solved by the swappable `UserModel` / `AuthPlugin<U>` mechanism — archived

20. [x] Auth ships no authenticated change-password route + `set_password` strength policy — archived

21. [x] `DecimalField` / money type — already shipped for Postgres (`rust_decimal::Decimal` → `NUMERIC(19,4)`); SQLite deferred — archived

22. [x] No permission combinators / common preset — the app's main gate is 7 lines of `Box::new(..) as Box<dyn Permission>` — shipped (commit 55ca0cdc)

    `And(IsAuthenticated, Or(ReadOnly, IsStaff))` is the app's most-used gate (fixtures, attendance, announcements, chat, teams) and reads as verbose dyn-boxing. **Proposal:** ship a named `IsAuthenticatedOrReadOnly` (DRF-style) and/or `.and()`/`.or()` combinators on `Permission` so consumers stop hand-boxing.

23. [x] No `serve`-only migrate/seed lifecycle (auto_migrate_on_serve) — archived

24. [x] Adding a `Choices` variant forces a full `AlterColumn` table rebuild — archived

25. [x] ORM SQLite write transactions used `BEGIN DEFERRED` → SQLITE_BUSY under concurrent writes — shipped `BEGIN IMMEDIATE` (commit 7a03c196)

    Root-caused while fixing the test-suite flake: `m2m.rs` (and `db::begin*`) use `pool.begin()`, i.e. sqlx `BEGIN DEFERRED`. Under concurrent writes on a file DB with >1 connection, a deferred read→write lock upgrade returns SQLITE_BUSY *immediately* (deadlock-avoidance path the `busy_timeout` handler is never consulted for). The test suite worked around it with `max_connections(1)` (commit cbbd1571), but real SQLite apps with concurrent writers can hit it.

    **Proposal:** issue `BEGIN IMMEDIATE` for SQLite write transactions (acquire the write lock at BEGIN, so `busy_timeout` applies and writers wait instead of erroring). Postgres unaffected. SQLite is test-first here, so lower priority — but it's the correct fix.

    **Minor (same source):** roster/payment endpoints do `AuthUser::objects().fetch()` into an in-memory id→username map (a manual join) because there's no `.values()`/annotate-join to pull just `(id, username)` — a scale trap the ORM could close.
26. [x] Admin sheet read flake — product bug fixed; residual is a test-only read-lock artifact, production unaffected — archived

27. [x] audit_2 residual low-severity hardening backlog — all 9 items shipped (2026-07-06) — archived

28. [ ] audit_2 deferred findings — big-design or live-Postgres-gated (verified open 2026-07-06)

    Genuinely-open findings that need a design decision or infra the local env can't provide. Recorded so they're tracked, not lost. See `planning/audit_2/findings/` for full write-ups.

    - **[authz P1]** Authorization is default-allow — no default-deny/gated-by-construction router primitive or boot-time audit of ungated mutating routes. Framework-posture design.
    - **[authz P2]** Model-level perms only — no object/row-level permission primitive (per-row grants / IDOR-by-design). Design.
    - **[authz R4 / R5]** RLS: no non-ignored two-tenant *enforcement* test, and policies are append-only across boots (no drop-undeclared diff). Both need a containerized Postgres.
    - **[admin #5]** Admin handlers have no in-handler CSRF verify (only `login_post` self-verifies); depends on either a boot-breaking `"security"` dep or a broad multi-handler sweep + hinges on the session cookie `SameSite`. Design call.
    - **[orm #3 / macros #2]** Mass-assignment: no field allowlist on the dynamic write path / no `#[form(fields=[...])]` allowlist API. Framework-wide write-policy design.
    - **[realtime #2 / #5]** No `authorize_publish` seam for inbound WS messages; presence re-broadcast is O(N²) and changing it alters the shipped wire protocol.
    - **[oauth OAU-4]** create-user + create-social aren't transactional (threading a txn through the username-retry loop).
    - **[supply-chain SC-3 / SC-5]** `umbral-core` has no `[features]` table (feature-gating touches every consumer); `notify` pinned at 6 (7/8 are API-breaking, dev-only plugin).

29. [ ] We need to start thinking about optimization ie what else can we move to the orm layer that is fully reimplemented everywhere, how can we improve the boilerplate.
