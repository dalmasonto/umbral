# review_4 · umbral-graphql

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 5,091 LOC (`plugins/umbral-graphql`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 3 medium · 1 low · 0 info — 6 new, 1 known |

## Summary

- Derives a full GraphQL schema — queries, mutations, subscriptions — from the same `ModelMeta` registry REST/OpenAPI read (`async-graphql` dynamic schema), with DataLoader batching for FK / reverse-FK edges, Relay cursor connections, a CSRF content-type guard on the POST endpoint, and depth/complexity budgets ON by default. 9 test suites, 120 test attributes.
- Internally one of the most carefully documented plugins in the tree: guards fail closed, the ownership scope's anonymous arm deliberately leaks nothing, and the child loader's docstring names the exact bug it replaced.
- Two HIGH findings, both in the codebase's signature drift class (the rule was fixed in one copy and left in its sibling):
  - GRAPHQL-1 — the CSRF guard is a `route_layer` over the POST route alone; `/graphql/sse` (also a POST) and `/graphql/ws` are registered after it, ride the same prefix-based CSRF exemption, and accept cookie-bearing cross-site operations (a mutation over WS runs through `execute_stream` → `execute_once`).
  - GRAPHQL-2 — `RowLoader` keeps the flat `.limit(MAX_LIMIT)` its sibling's docstring calls "exactly the gaps4 #13 bug": any resolution pass batching more than 200 distinct PKs silently gets `null` relations.
- Mediums: NULL sort values break keyset pagination (silent row omission on text columns; a hard "cursor does not match this column's type" error on numeric/date columns); `create` mutations bypass the owner scope that `update`/`delete` enforce and REST's create path enforces; raw sqlx error text reaches clients where REST sanitizes. One LOW: a subscription silently drops events when its re-read errors.
- Fix first: 1) close the SSE/WS transports (one `route_layer` after all three routes are registered, plus an `Origin` check on the WS handshake), 2) delete the flat loader cap.

## Findings

### GRAPHQL-1 · HIGH · security · self-verified
**The CSRF content-type guard is a `route_layer` applied before the subscription transports exist, so `/graphql/sse` (a POST) and `/graphql/ws` accept cross-site cookie-bearing operations — a mutation sent over WebSocket executes with the victim's session.**

- **Where**: `plugins/umbral-graphql/src/lib.rs:784` (the layer), `:758-777` (the only route it covers), `:810-839` (WS), `:844-869` (SSE); `plugins/umbral-security/src/lib.rs:558-565` (prefix exemption); vendored `axum-0.8.9/src/routing/path_router.rs:311-340`.
- **Evidence**:
  ```rust
  // lib.rs:784 — applied when `router` holds exactly one route (the POST at :758)
  router = router.route_layer(axum::middleware::from_fn(csrf_content_type_guard));
  // lib.rs:844-846 — registered AFTER the layer; never wrapped by it
  router = router.route(
      &format!("{path}/sse"),
      axum::routing::post(
  ```
  ```rust
  // vendored axum-0.8.9/src/routing/path_router.rs:326-333 — route_layer wraps the endpoints
  // present at call time, and only those. The panic at :319-323 states the rule:
  // "Adding a route_layer before any routes is a no-op. Add the routes you want the layer to apply to first."
  let routes = self
      .routes
      .into_iter()
      .map(|(id, endpoint)| {
          let route = endpoint.layer(layer.clone());
  ```
  The guard itself (`lib.rs:146-162`) rejects non-JSON only when `req.method() == POST`; its own docstring (`:153-157`) says the content type "is what stands between a hostile page's `<form>` and a mutation carrying your user's session cookie". The module design note (`lib.rs:113-118`) records why the endpoint is exempt at all: "every umbral app puts `/graphql` in `csrf_exempt_paths` … Classic CSRF, with the token check turned off by the very exemption that made queries work."
- **Impact**: App with subscriptions enabled (`if exposed.iter().any(|e| e.subscribable)`, lib.rs:803) and session cookie `SameSite=None` (the documented cross-origin setting — `plugins/umbral-sessions/src/lib.rs:315-324`). A hostile page opens `new WebSocket("https://app.example/graphql/ws", "graphql-transport-ws")`: browsers attach cookies to the handshake and CORS does not gate WebSocket, then sends a `subscribe`/`start` message whose document is `mutation { deletePost(id: "7") { id } }`. Vendored async-graphql runs any non-subscription operation from that path — `ClientMessage::Start` → `execute_stream` (`async-graphql-7.2.1/src/http/websocket.rs:367-383`), and `execute_stream` dispatches `if env.operation.node.ty != OperationType::Subscription { … execute_once … }` (`src/schema.rs:606-626`). No `Origin` check exists anywhere in the plugin (every "origin" hit in `src/` is a comment). The SSE variant needs no socket: POST to `/graphql/sse` with a `text/plain` form body crafted so the browser-inserted `=` falls inside a GraphQL string literal (e.g. an unused variable default `($x: String = "=")`) — async-graphql parses every non-multipart content type as JSON (`src/http/mod.rs:97-113`; "default to json" at `:131-132`), so the form body is accepted and the mutation runs; the attacker cannot read the SSE response cross-origin but does not need to. The app-wide CSRF layer does not help: `is_exempt` is segment-prefix (`umbral-security/src/lib.rs:558-565`), so exempting `/graphql` also exempts both siblings. Under the DEFAULT `SameSite=Lax` no cookie rides either request — that is why this is HIGH and not CRITICAL; the documented `SameSite=None` configuration is the precondition. Static trace; browser cookie/WS behaviour as described, no runtime repro.
- **Fix**: register the WS and SSE routes before a single `route_layer` (or attach it per route), and validate the `Origin` header on the WS handshake — HTTP-level guards cannot see WS frames, and a naive "move the layer" fix leaves `/graphql/ws` open because the guard only inspects POST. Add a regression test mirroring `a_form_encoded_post_is_rejected_because_that_is_what_csrf_looks_like` (`tests/mutations.rs:255-282`) against `/graphql/sse` and the WS handshake — today that test covers only `/graphql`.

### GRAPHQL-2 · HIGH · correctness · self-verified
**`RowLoader`'s batched read still applies one flat `.limit(MAX_LIMIT)` to the coalesced key set — the pattern the sibling loader's own docstring names as the gaps4 #13 bug — so any DataLoader batch above 200 distinct PKs silently returns `null` for the truncated relations.**

- **Where**: `plugins/umbral-graphql/src/loader.rs:258-261` (`fetch_where_in`), `:77-79` (RowLoader routes through it), `:271-272` (the sibling's docstring), `plugins/umbral-graphql/src/lib.rs:71` (`MAX_LIMIT = 200`).
- **Evidence**:
  ```rust
  // loader.rs:258-261
  let rows = DynQuerySet::for_meta(meta)
      .allow_private(&refs)
      .filter_condition(any.expect("set above"))
      .limit(crate::MAX_LIMIT)
  ```
  ```rust
  // loader.rs:271-272 — the sibling's own account of the class
  /// Unlike [`fetch_where_in`], this puts NO `LIMIT MAX_LIMIT` on the whole query —
  /// that single global cap is exactly the gaps4 #13 bug, where one prolific parent
  /// consumed the entire limit and starved its siblings.
  ```
  async-graphql coalesces pending keys into one `load` call, `max_batch_size: 1000` by default (vendored `async-graphql-7.2.1/src/dataloader/mod.rs:215`).
- **Impact**: query `{ a: posts(first: 200) { author { id } } b: posts(first: 200, orderBy: "-id") { author { id } } }` over 400 posts with >200 distinct authors between the two pages. Both `author` resolvers call `loaders.row.load(PkKey { table: "user", id })`; one coalesced batch reaches `fetch_where_in` with e.g. 240 ids; the query becomes `… WHERE id IN (240 ids) LIMIT 200` → 200 rows → the loader's map lacks the other 40 keys → those `author` fields resolve `null`, silently, with no error. The connection cap (`first` clamped to `MAX_LIMIT`, `connection.rs:233`) means a single list can never trip the cap alone, but any query composing two lists (or a list plus a same-window relation batch) can exceed 200 distinct keys — and for distinct PKs the result is inherently ≤ `ids.len()`, so the limit can only truncate; it protects nothing. Static trace: the exact coalescing is async-graphql's run-loop behaviour (needs runtime confirmation of batch composition), but the cap is unconditional.
- **Fix**: drop `.limit(crate::MAX_LIMIT)` from `fetch_where_in` and bound the batch explicitly (set the DataLoader's `max_batch_size`); or window it the way `fetch_children_batch` already does. Extend the `child_window.rs` two-authors pattern to the FK loader with more than `MAX_LIMIT` distinct targets.

### GRAPHQL-3 · MEDIUM · correctness
**A NULL sort value produces an empty-string cursor cell the keyset predicate can never match — paginating a nullable sort column silently skips the rest of the NULL block (text) or errors with "cursor does not match this column's type" (numeric/date).**

- **Where**: `connection.rs:79-85` (`cell_to_string`), `:99-112` (`keyset`), `:309-314` (cursor built from the page's last row).
- **Evidence**:
  ```rust
  // connection.rs:79-85
  fn cell_to_string(v: Option<&Json>) -> String {
      match v {
          Some(Json::String(s)) => s.clone(),
          Some(Json::Null) | None => String::new(),
          Some(other) => other.to_string(),
      }
  }
  // connection.rs:107-108 — a "" cursor fails coercion on a typed column
  let after_sort = typed_cmp_condition(meta, sort, &cur.v, cmp)
      .ok_or_else(|| async_graphql::Error::new("cursor does not match this column's type"))?;
  ```
- **Impact**: `postsConnection(first: 50, orderBy: "published_at")` where `published_at` is nullable and 80 rows are NULL. SQLite sorts NULLs first on ASC: page 1 ends inside the NULL block with cursor `v = ""`; page 2's predicate `published_at > '' OR (published_at = '' AND pk > p)` evaluates to NULL for every NULL row → the remaining 30 NULL rows are unreachable, silently (the caller jumps to non-NULL rows). Postgres sorts NULLs last: once the cursor is the last non-NULL row, the whole NULL block is unreachable the same way. On an integer/date sort column the `""` instead fails `typed_cmp_condition` and the next page returns `cursor does not match this column's type` — pagination dies mid-list. The `orderBy` guard (`connection.rs:274-278`) checks only that the column exists and is visible; nullable columns are orderable. Meets MEDIUM (bounded correctness with a workaround — order by a non-nullable column) rather than HIGH: the numeric case fails loudly, and the text case needs a nullable sort column whose NULL block crosses a page boundary.
- **Fix**: carry NULL explicitly in the cursor (a flag alongside the value) and make `keyset` NULL-aware (`sort IS NULL AND pk > p` while inside the NULL block, plus a transition arm), or emit an explicit `NULLS LAST` on both backends and anchor the NULL block deterministically.

### GRAPHQL-4 · MEDIUM · security/drift
**`create<Model>` mutations never apply the owner scope that `update`/`delete` — and REST's create path — enforce, so a caller can create rows owned by another user.**

- **Where**: `mutation.rs:163-194` (create: no `owner_scope`), `:196-245` (update: `owner_scope` at `:216`), `:247-282` (delete: `:264`), `:290-319` (the resolver); contrast `plugins/umbral-rest/src/lib.rs:4248-4253`.
- **Evidence**:
  ```rust
  // mutation.rs:172-175 + 181-183 — create resolves args and inserts; nothing consults owner_field
  crate::guard_write(&ctx, &e)?;
  let data = ctx.args.try_get("data")?;
  let unlocks = crate::privacy::from_ctx(&ctx);
  let body = body_from_args(&data.object()?, &e)?;
  ...
  let row = DynQuerySet::for_meta(&e.meta)
      .allow_private(&refs)
      .insert_json(&body)
  ```
  ```rust
  // plugins/umbral-rest/src/lib.rs:4248-4253 — the REST sibling blocks exactly this
  // gaps3 #16: owner-field injection — fill the declared owner column from the
  // authenticated identity and reject a body-supplied value (a client can't
  // create a row owned by someone else).
  inject_owner_field(cfg, &model, identity.as_ref(), &mut body)?;
  ```
- **Impact**: model declared `.owned_by("post", "author")` (and REST `.owner_field("author")`): authenticated user B sends `mutation { createPost(data: { title: "hi", author: "<A's pk>" }) { id } }`. The row is created with `author = A`. It appears under A's read scope (spam/phishing/forged attribution), is absent from B's own listing, and any per-owner quota or visibility invariant is broken. REST has refused precisely this since gaps3 #16; the plugin's IDOR boot check (`tests/object_scope_check.rs`, gaps5 #101 / tf#322) stops warning once `owned_by` is set — so an operator gets a green check while the create surface stays forgeable. The `owned_by` doc (`lib.rs:375-403`) scopes "an `update`/`delete` mutation", and the two-user suite (`tests/mutation_owner_scope.rs`) covers update/delete/anonymous only — this is an omission, not a decision.
- **Fix**: apply the same decision on create — inject the caller's pk into the owner column and reject a body-supplied different value (the `inject_owner_field` shape), and honour `DenyAnonymous` for owner-scoped models. Extend `mutation_owner_scope.rs` with the create case.

### GRAPHQL-5 · MEDIUM · security
**Mutation and connection errors reach GraphQL clients as the ORM/sqlx Display text, leaking driver messages where the REST sibling sanitizes them (WEB-5).**

- **Where**: `mutation.rs:185, 226, 276`, `connection.rs:301` (same pattern on the list/query resolvers); Display impls `crates/umbral-core/src/orm/write.rs:515` and `orm/dynamic.rs:94`; REST's sanitized arm `plugins/umbral-rest/src/lib.rs:3310-3319`.
- **Evidence**:
  ```rust
  // mutation.rs:183-185
  .insert_json(&body)
  .await
  .map_err(|err| async_graphql::Error::new(err.to_string()))?;
  ```
  ```rust
  // crates/umbral-core/src/orm/write.rs:515
  WriteError::Sqlx(e) => write!(f, "umbral::orm::write: sqlx: {e}"),
  // crates/umbral-core/src/orm/dynamic.rs:94
  Self::Sqlx(e) => write!(f, "{e}"),
  ```
- **Impact**: a create that trips a unique index returns the raw driver message — `UNIQUE constraint failed: user.email` / `duplicate key value violates unique constraint "user_email_key"` — disclosing table, column and constraint names to any client, while the same failure over REST logs `%e` and answers `"internal server error"` (code `database_error`). Bounded info disclosure; needs a DB error to trigger. Fix: map write/query errors through one sanitizing helper (log `%e`, return a generic message), as REST's WEB-5 arm already does — the parity target exists.

### GRAPHQL-6 · LOW · correctness
**A transient DB error while re-reading a changed row silently drops the subscription event — `.ok()??` collapses "row gone" and "read failed" into "skip", with no log.**

- **Where**: `subscription.rs:200-206` (`load_one_json` is `loader.rs:358`).
- **Evidence**:
  ```rust
  // subscription.rs:200-206
  let row = crate::loader::load_one_json(
      &meta,
      &ev.pk,
      unlocks.for_table(&meta.table),
  )
  .await
  .ok()??;
  ```
- **Impact**: a `postChanged` subscriber misses an update whenever the follow-up read errors (connection reset, pool exhaustion) — the client waits with no signal — because `.ok()` discards the error before anything can log it (the CLAUDE.md `.ok()`-discards-a-secondary-error class). "Row deleted in the race" also lands here, which is legitimate; today the two cases are indistinguishable. Fix: log the error (`tracing::warn!`) before skipping, or split `Ok(None)` (skip silently) from `Err(e)` (log, or surface on the stream).

## Strengths

- `guard`/`guard_write` fail closed and hide existence (`lib.rs:198-244`): a denied request gets the same answer whether or not the row exists.
- `OwnerScope::DenyAnonymous` is deliberately non-leaking: "Denying (not erroring) leaks nothing about whether the target row exists" (`mutation.rs:297-300`).
- Read-back-after-write on every mutation, with the reason in the comment (`mutation.rs:230-233`): defaults, `auto_now`, cleaners and triggers all mean the response is the stored row, not the request echo.
- `resolve_request_ctx` (`lib.rs:164-174`) — ONE context builder for POST/SSE/WS, per-caller loaders keyed to the caller's unlocks, with the pre-fix drift recorded in the docstring.
- Depth/complexity budgets ON by default (`schema.rs:203-204`, applied `:211-212`; overridable via `lib.rs:715-716`).
- `privacy.rs::is_conditional` (`:59-61`) makes private+unlockable columns nullable so a permission decision can never become a broken query — the same case the openapi TS client gets wrong (OPENAPI-1, sibling file).
- `fetch_children_batch` (`loader.rs:268-282`): per-parent window, proportional truncation under a safety ceiling, and a docstring that names the old bug outright — a model for documenting a fix.
- Real behavioural tests through the real router: 120 test attributes across 9 suites — two-user owner scope, per-parent windows, private unlocks on every read path, subscription context, the CSRF content-type test, and boot-check coverage.

## Clean areas checked

- Raw SQL: `grep -rn "sqlx::" plugins/umbral-graphql/src` → no hits; every read/write goes through `DynQuerySet` / the loaders, with `allow_private` threaded per caller.
- `unsafe`: none in the crate.
- Dependency direction: `Cargo.toml` depends on the `umbral` facade plus async-graphql(-axum), axum, serde, sea-query — no `umbral-core` internals, no plugin-to-plugin deps.
- Ordering surface: `orderBy` requires the column to exist and be visible (`connection.rs:274-278`) — hidden columns cannot become sort keys or cursor fields.
- Pagination caps: `first` clamped `.min(crate::MAX_LIMIT)` (`connection.rs:233`); `DEFAULT_LIMIT = 50`, `MAX_LIMIT = 200` (`lib.rs:68-71`); the extra `first + 1` row is fetched only to compute `hasNextPage` and discarded.
- CSRF on the POST route itself: correct where applied — content-type check before extraction (`lib.rs:145-162`, `:779-784`).
- Private-field exposure: reads resolve through per-request unlocks (`privacy.rs::from_ctx` — "Absent context => nothing unlocked, which is the safe direction to be wrong in"); loaders are built per request, so no cross-caller cache.
- Writes: `body_from_args` + `insert_json`, which strips privileged columns and runs cleaners/validators (comment at `mutation.rs:178-180`).

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps4 #9` — GraphQL mutations lack object-level scope `[fixed/verified held]` — `owned_by` scopes `update`/`delete` with two-user regression tests; `create` is NOT covered → GRAPHQL-4.
- `gaps4 #10` — no depth/complexity budget `[fixed/verified held]` — defaults 12/500 applied in `schema::build` (`schema.rs:203-204`, `:211-212`).
- `gaps4 #12` — subscription/SSE context misses auth + unlocks `[fixed/verified held]` — one `resolve_request_ctx` feeds POST/SSE/WS (`lib.rs:174`, used at `:766`, `:823`, `:852`).
- `gaps4 #13` — child loader global relation limit `[fixed/verified held]` for the ChildLoader's per-parent window (`loader.rs:124-160`); its sibling `RowLoader` still carries the flat cap → GRAPHQL-2.
- `gaps5 #101` (tf#322) — IDOR protection has the pieces but no unified contract `[open]` — stamp open; the GraphQL boot-check half ships (`tests/object_scope_check.rs`), the unified guide/contract half does not; the check stops warning once `owned_by` is set, which is exactly where GRAPHQL-4 lives.

## Out-of-scope notes

- `umbral-security` `is_exempt` is segment-prefix (`plugins/umbral-security/src/lib.rs:558-565`): exempting `/graphql` also exempts `/graphql/sse` and `/graphql/ws` — the security reviewer should confirm the intended breadth (relevant to GRAPHQL-1).
- `umbral-sessions` documents `SameSite::None` as the cross-origin option (`plugins/umbral-sessions/src/lib.rs:315-324`); the docs could note it removes the cookie-level CSRF barrier for every transport — the GRAPHQL-1 precondition (sessions reviewer).
