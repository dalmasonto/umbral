# review_4 · umbral-rest

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 23,848 LOC (`plugins/umbral-rest`) |
| **Reviewer** | claude (W2 agent) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 3 high · 2 medium · 2 low · 0 info — 7 new, 0 known |

## Summary

- umbral-rest is the DRF-style auto-API plugin: `ResourceConfig`s fold into a `RestPlugin` that registers models, permissions, object scopes, filters, pagination, throttles, nested writes, bulk endpoints, and custom actions, all mounted as an axum router under `/api`.
- The crate's hardening posture is genuinely strong: safe-by-default `ReadOnly`, `DEFAULT_BLOCKED_TABLES`, boot-time system checks and warnings, a single `fetch_rows` seam that carries the object-scope filter into list/detail/read-back alike, per-item bulk guards, and a WEB-5 error policy that never leaks driver text.
- The three HIGH findings are all instances of the tree's signature drift class — a rule enforced on the sibling surfaces and forgotten on one: `.action_by`'s GET resolves a row with no object scope, no `under()` parent check, and no response overrides (REST-1, REST-4), and the nested-update walk never runs the child resource's own Update permission (REST-2).
- The medium/low tail is bounded: bulk endpoints silently drop declared nested child arrays (REST-3), a handler's DB failure is reported as the client's 400 "malformed request" (REST-5), `NoPagination`'s `count` is rows-returned and its 1000-row truncation is unmarked (REST-6), and three stale "AllowAny is the default" comments survive the earlier docs sweep (REST-7).
- Fix order: wire `action_by_dispatch` through the same scope/parent/override pipeline as `retrieve_impl`; implement `check_update` on `RestNestedGate`; then the bulk-nested guard. The two fixes to `RestNestedGate` and `action_by_dispatch` are small and well-seamed.

## Findings

### REST-1 · HIGH · security/drift · self-verified
**`.action_by` GET serves out-of-scope rows: it resolves the row with no object-scope filter and never applies `resolve_parent`, so a scoped or `.under()`-nested resource's natural-key URL bypasses both row scoping and the "flat route must not work" guarantee.**
- **Where**: `plugins/umbral-rest/src/lib.rs:5287-5297` (fetch), `:5308-5316` (gate, after the fetch), `:3546-3553` (`resolve_parent`'s flat-refusal arm), `:716-721` (`effective_scope_hook`'s `Action::Custom` arm).
- **Evidence**:
  ```rust
  // lib.rs:5287-5297 — the entire row-resolution for the action_by GET
  let mut rows = fetch_rows(
      &model,
      Some((&lookup_field, &value)),
      None,
      &FilterClause::default(),
      &[],
      &[],
      &[],
      &[],
  )
  .await?;
  ```
  (The third arg is the page request — `None` on the single-row path; the fourth is the filter, empty here — and the fetch runs before `authenticate` at `:5308`.) `action_by_dispatch` never calls `resolve_parent` or `cfg.object_scope`; a grep of every `object_scope(` call site proves it: `:3753` (List), `:4014` (Retrieve), `:4415` (bulk Update), `:4530` (bulk Delete), `:4902` (Update), `:5055` (Delete) — `Action::Custom`, the arm the hook was written for, is never passed. Meanwhile `routes()` force-mounts the literal flat `/api/<table>/{id}` route for any `.action_by` table regardless of `.under()` (`:2871-2879`), and on that same URL PUT/PATCH/DELETE route through `update_impl`/`destroy_impl` with `parent: None` → `resolve_parent`'s arm at `:3550` 404s ("Declared nested, reached FLAT. The flat route must not work"), so GET is the one verb that serves. The shipped write-up `planning/archive/gaps4-done.md:148` claims this exact path runs "under the resource's `Permission::check` + object scope (Action::Custom)".
- **Impact**: Two concrete traces. (A) `.resource(ResourceConfig::for_::<Community>().owned_by("owner_id"))` + `.action_by("slug", Method::GET, h)`: `GET /api/community/<someone-elses-slug>/` returns that owner's full row to anyone the read permission admits, while `GET /api/community/<their-pk>/` → 404 (retrieve_impl scopes). (B) `.under("fixture", "fixture_id")` + `.action_by("slug", …)`: `GET /api/selection/<slug>` serves a row from ANY fixture; the same URL's PUT/PATCH/DELETE are 404 and the nested URL `/api/fixture/9/selection/<slug>` is scoped — GET is the documented-guarantee exception.
- **Fix**: in `action_by_dispatch`, move `authenticate` above the fetch, run `resolve_parent(cfg, &table, None).await?` (this 404s the flat URL for `.under()` resources, matching every sibling verb), resolve `cfg.object_scope(&table, identity, None, &Action::Custom(lookup_field))` and AND the `Filter` into the fetch's `FilterClause` (`DenyAll` → fall through to the standard not-found); mirror the `unlocked_private`/`revealed_cols` args the other handlers pass. Add a scope + `.under()` test to `tests/action_by*.rs` (current suites only cover 401/404-shape, not scope).

### REST-2 · HIGH · security/drift · self-verified
**Nested update never enforces the child resource's own Update permission: core calls `gate.check_update` on the upsert UPDATE branch, but `RestNestedGate` does not implement it, so the trait default allows — while the wrapper's own comment claims it is enforced.**
- **Where**: `crates/umbral-core/src/orm/nested.rs:163-169` (hook + default), `:905-909` (the only call site), `plugins/umbral-rest/src/lib.rs:4631-4677` (`RestNestedGate` impl), `:4815-4820` (claim), `:4947-4963` (REST entry).
- **Evidence**:
  ```rust
  // crates/umbral-core/src/orm/nested.rs:163-169
  /// Enforce this child's OWN update permission — the upsert path's UPDATE
  /// branch (an item that carries its primary key). Runs before the
  /// ownership read. Default: allow.
  fn check_update(&self, table: &str) -> Result<(), Self::Error> {
      let _ = table;
      Ok(())
  }
  ```
  ```rust
  // crates/umbral-core/src/orm/nested.rs:905-909
  // UPDATE — enforce this child's own update permission before any read.
  let pk_str = json_pk_to_string(&pk_json)...;
  gate.check_update(&meta.table)?;
  ```
  `RestNestedGate` implements `max_nodes`, `allow_table`, `strip_hidden`, `check_create` (`:4654`, "H2: enforce the child resource's OWN create permission"), `scope_create`, `apply_overrides` — and nothing else; it inherits the default-allow `check_update`. The REST wrapper comment nevertheless says the gate supplies "per-child security (create/update permission, hidden-strip, object-scope, revealed overrides)" (`:4817-4819`). `update_impl` gates only the PARENT (`:4877-4882`, `&Action::Update` on `table`) before routing into the walk at `:4947-4963`. `tests/nested_child_permission.rs` covers only the create branch (a POST whose child has no pk); no test sends a pk-bearing child on PATCH.
- **Impact**: Parent `Order` with `.nested("items", "order_item")` and a permission that admits writes, child `OrderItem` with a stricter class (the crate's own test config: `.permission(ReadOnly)` on the child, rows pre-existing via seeder/admin — or more generally a custom permission that allows Create but denies Update): `PATCH /api/order/1/ {"items":[{"id":7,"qty":99}]}` → 200, child row 7 updated; the direct `PATCH /api/order_item/7/` → 403. Same for grandchildren via `upsert_child`'s recursion. The FK-ownership read (`nested.rs:915-927`) bounds reach to the parent's own tree, but "who may update this class of row" is exactly the per-child rule the hook exists for — and the create branch's 403 test shows the intended policy.
- **Fix**: implement `check_update` on `RestNestedGate` mirroring `check_create` (`permission_for(table).check(&Action::Update, self.identity)`); extend `nested_child_permission.rs` with the pk-supplied PATCH case. Secondary (core, see Out-of-scope): the trait has no `scope_update` hook, so the child's own write scope cannot run on nested updates either.

### REST-3 · MEDIUM · correctness/drift
**Bulk create/update silently drop a declared nested child array in an item body: the array branch dispatches to `bulk_create` before nested handling, and the bulk loops never route through the nested writers nor run `reject_undeclared_nested` — the client gets 201/200 with children missing.**
- **Where**: `plugins/umbral-rest/src/lib.rs:4218-4235` (dispatch), `:4243` (`nested_specs` computed after), `:4326-4353` (`bulk_create` item loop), `:4427-4466` (`bulk_update` item loop), `:4274`/`:4967` (the guard's only call sites).
- **Evidence**:
  ```rust
  // lib.rs:4218-4235 — array body leaves for bulk BEFORE nested handling
  if let Value::Array(items) = raw {
      ...
      return bulk_create(cfg, &table, model, items, identity.as_ref(), parent_scope.as_ref()).await;
  }
  ```
  ```rust
  // lib.rs:4345-4350 — bulk item write: no nested routing, no undeclared-array guard
  let mut row = as_user(
      identity,
      umbral::orm::DynQuerySet::for_meta(&model).insert_json_in_tx(&body, &mut tx),
  )
  ```
  The single-object path guards the same shape (`:4271-4274`, "an array-of-objects is an undeclared nested relation, not an ignorable extra (gaps3 #10)") and routes declared nesting to `create_nested` (`:4284-4289`). Core's `insert_json` plan iterates `meta.fields` only, so a non-column array key is ignored, not errored. The crate's docs assert parity: `resource.rs:773-777` ("Every bulk item runs the SAME validation … bulk opens no bypass") and `bulk_create`'s doc at `:4308-4313` ("Each item runs the SAME field denylist and validation as the single create").
- **Impact**: Resource with `.nested("items", "order_item")` + `.bulk()`: `POST /api/order/ [{"customer":"A","items":[{"product":"W"}]}, …]` → 201 with parent rows created and ZERO children; the identical body as a single object creates the children. Same on `PATCH` via `bulk_update` (the array key is dropped by `update_json_in_tx`). A client seeding an object graph in bulk gets a success and a half-written graph.
- **Fix**: either run `reject_undeclared_nested` per item (400 naming the index — matching the single path's stance) or route items carrying declared nested arrays through `create_nested`/`update_nested` on the same tx; the 400 is the minimal honest fix. Extend `bulk_security.rs`/`bulk.rs` with a nested+bulk case.

### REST-4 · HIGH · security/drift · self-verified
**`action_by`'s `resolved_row` is the raw fetch: `.hide()`/`transform`/`computed` overrides and per-identity `reveal_if`/`allow_private_if` entitlements are not applied, so the documented echo pattern returns columns the docs say are stripped from every response.**
- **Where**: `plugins/umbral-rest/src/lib.rs:5329`, `:5287-5297` (empty `unlocked_private`/`revealed`), `:1698-1702` (`apply_overrides`), `plugins/umbral-rest/src/resource.rs:1008`/`:1083`/`:1095` (the "every REST response" claims), `:1189-1192` (the documented handler).
- **Evidence**:
  ```rust
  // lib.rs:5329 — handed to the handler exactly as fetched
  resolved_row: Some(Value::Object(row)),
  ```
  ```rust
  // resource.rs:1008 — the contract
  /// Strip one or more fields from every REST response for this table.
  ```
  ```rust
  // resource.rs:1189-1192 — the documented action_by pattern
  .action_by("slug", Method::GET, |ctx| async move {
      let row = ctx.resolved_row.expect("action_by always resolves a row");
      Ok(json!({ "community": row }))
  })
  ```
  Every sibling response path calls `apply_overrides` (list `:3821-3823`, retrieve `:4059`, create `:4280`, bulk `:4351`, update `:4994`, bulk_update `:4465`); `action_by`'s row does not, and the dispatch also passes empty `unlocked_private`/`revealed_cols` (`:5294-5295`) where `retrieve_impl` passes `cfg.unlocked_private(...)`/`cfg.revealed_cols(...)`. Nothing downstream shapes the handler's `Json` output (the router's only layer is `cache_control_layer`).
- **Impact**: `.hide("ssn")` (a plain column — `.hide()` is not the ORM's secret/private tier) + `.action_by("slug", …)` with the documented handler: `GET /api/community/<slug>/` returns `ssn`, while every CRUD endpoint for the same resource strips it. Symmetrically, a caller entitled via `.reveal_if`/`.allow_private_if` gets the un-revealed row, and `transform`/`computed` results are absent — the same URL serves a differently-shaped row than the rest of the API.
- **Fix**: after resolving the row (post-gate), call `cfg.apply_overrides(&table, &mut row)` and fetch with `cfg.unlocked_private(...)`/`cfg.revealed_cols(...)` for the authenticated identity, so `resolved_row` matches the detail endpoint's shape. Note `.action()`'s `resolved_row: None` (`:5170`) is unaffected.

### REST-5 · MEDIUM · correctness/api
**A custom action handler's database error is reported to the client as `400 bad_input "malformed request"` even though `ActionError::Internal` is documented as 500.**
- **Where**: `plugins/umbral-rest/src/resource.rs:163-165` (doc), `:190-194` (`From<sqlx::Error>`), `plugins/umbral-rest/src/lib.rs:5192`, `:5339` (both dispatchers), `:3161-3169` (Protocol → 400).
- **Evidence**:
  ```rust
  // resource.rs:163-165
  /// 500 — internal failure, with a short message. Database
  /// errors and other unexpected failures.
  Internal(String),
  ```
  ```rust
  // resource.rs:190-194
  impl From<sqlx::Error> for ActionError {
      fn from(e: sqlx::Error) -> Self {
          ActionError::Internal(e.to_string())
      }
  }
  ```
  ```rust
  // lib.rs:5192 and :5339
  Err(ActionError::Internal(m)) => Err(ApiError::Sqlx(sqlx::Error::Protocol(m))),
  ```
  `From<sqlx::Error> for ApiError` then rewrites Protocol into `BadInput("malformed request")` (`:3161-3168`) → HTTP 400.
- **Impact**: A handler that `?`s a `sqlx` error (the `ActionError::internal` shortcut exists for exactly this, `resource.rs:169-173`) answers 400 "malformed request" — clients retry/debug the request instead of treating it as a server fault, and 5xx dashboards miss it. The correct arm `ApiError::Internal` exists (`:3150-3153`, opaque 500 + server-side log) and is what the `ActionError` doc promises.
- **Fix**: map `ActionError::Internal(m)` to `ApiError::Internal(m)` in both dispatchers; keep the `Protocol` special-case for genuinely driver-level faults on the non-write paths.

### REST-6 · LOW · api/docs
**Under the default `NoPagination`, the envelope's `count` is rows-returned (not the filtered total other paginators report), and the 1000-row safety truncation is invisible.**
- **Where**: `plugins/umbral-rest/src/pagination.rs:234-245` and `:15-17`, `plugins/umbral-rest/src/lib.rs:5491-5500` (clamp), `:3859-3863` (`total = rows.len()`).
- **Evidence**:
  ```rust
  // pagination.rs:234-245
  fn paginate(&self, rows: ..., _total_rows: i64, _req: &PageRequest) -> Value {
      let count = rows.len();
      json!({ "results": rows, "count": count })
  }
  ```
  ```rust
  // lib.rs:5499
  let effective_limit = req.limit.min(MAX_LIST_ROWS);
  ```
- **Impact**: The same key `count` means "total matching rows" under `PageNumberPagination`/`LimitOffsetPagination` and "rows in this response" under the default; a client on the default cannot distinguish a 1000-row table from a larger one truncated at the cap (no `next`, no marker). The cap itself is documented for users (`documentation/docs/v0.0.1/rest/index.mdx:41`), so this is a shape ambiguity, not a silent leak.
- **Fix**: when `NoPagination` returns exactly `MAX_LIST_ROWS` rows, add a marker (`truncated: true` or `has_more`) to the envelope; document `count`'s per-style meaning on the trait.

### REST-7 · LOW · docs/drift
**Three comments still call `AllowAny` the default permission; the code's default is `ReadOnly` (WEB-1).**
- **Where**: `plugins/umbral-rest/src/permission.rs:24`, `:149`; `plugins/umbral-rest/src/resource.rs:324-325`.
- **Evidence**:
  ```rust
  // permission.rs:24
  //! - [`AllowAny`] — default. Every action allowed, anonymous OK.
  // permission.rs:149
  /// Allow every action, anonymous OK. The default.
  // resource.rs:324-325
  /// Permission class for this resource. `None` defaults to
  /// [`crate::permission::AllowAny`] at merge time.
  ```
  vs `lib.rs:1076` (`default_permission: Arc::new(ReadOnly)`) and `lib.rs:563-564` ("The default is [`ReadOnly`] (WEB-1)").
- **Impact**: A reader auditing permissions from the type docs is told the opposite of the shipped posture — the same misreading the earlier docs sweep fixed in the module docs (`gaps4 #20`), left behind in three per-item comments.
- **Fix**: update the three comments to `ReadOnly` (resource.rs's also contradicts the merge logic: a `None` resource permission inherits the plugin default, it does not merge to `AllowAny`).

## Strengths

- **Safe-by-default with a paper trail.** `default_permission: Arc::new(ReadOnly)` (`lib.rs:1076`) plus `DEFAULT_BLOCKED_TABLES`, the `security.object_scope` boot check over auto-exposed models (gaps6 #2), and the WEB-1/M-4/M-5/trusted-proxy boot warnings make the risky configurations loud at startup instead of quiet in prod.
- **One scoping seam.** `fetch_rows` (`lib.rs:5438`) carries the object-scope filter into the single-row path as well as the list path (`:5467-5474`, audit_2 H1/P2), and `resolve_parent` (`:3532-3601`) centralizes parent scoping with the "flat route must not work" arm — the CRUD handlers all funnel through both. REST-1 exists precisely because one newer dispatch skipped the seam.
- **Nested security as gate hooks, not special cases.** The `NestedWriteGate` split (`crates/umbral-core/src/orm/nested.rs`) keeps the tree walk generic while `RestNestedGate` supplies strip/create-permission/scope/overrides, with `allow_table` re-checking exposure (`L-6`) and the cross-parent ownership read structural in core. The create spine is correct and test-proven (`nested_child_permission.rs`'s 403 + whole-tree rollback).
- **Bulk safety rails.** `MAX_BULK_ITEMS` pre-check, per-item `strip_hidden_for_write` + `inject_owner_field` + `inject_parent` + create-scope, all-or-nothing transaction, and errors naming the offending index (`bulk_item_error`) — REST-3 is a missing sibling guard, not a missing discipline.
- **Error-text hygiene.** WEB-5 policy is implemented consistently: driver text never reaches clients (`Protocol` → generic 400 with `tracing::warn`, `ApiError::Sqlx` → opaque 500 with `tracing::error`), `WriteError` keeps structured per-field 400 maps, and the DRF-shaped `{detail, code}` envelope is uniform.
- **ORM discipline.** Zero `sqlx::query`/`query_as` in `src/` (the single grep hit is a comment inside a generated-code template), facade-only imports, no `unsafe`, no raw-SQL escapes.
- **Behavioral tests.** 60 files / 12.3k LOC; the security-relevant suites (`nested_child_permission.rs`, `membership_scope.rs`, `bulk_security.rs`, `object_scope_auto_exposed.rs`) drive real HTTP requests against real rows and read the object graph back.

## Clean areas checked

- **SQL injection**: every value bound through sea-query predicates; identifiers come from validated `ModelMeta` columns; `parse_ordering` only accepts names pre-filtered against `queryable_fields`.
- **Filter/search/order visibility**: queryable set built from non-hidden fields (`list_impl`, `:3709-3776`), matching the response serializer — the old oracle (review_3 claude #3) is fixed and holds.
- **Mass assignment**: `strip_hidden_for_write` on create/bulk/nested plus `inject_owner_field`/`inject_parent` overrides; the nested UPDATE removes pk and FK from the SET body before `update_json_in_tx`; `is_field_write_denied` covers secret/hard-denied/hide (private handled by the ORM tier).
- **Permission default-deny surface**: every dispatch re-checks `allowed_model` (L-6/L-7), custom actions re-find their def and gate `Action::Custom`, `view_exposed`/`exposed_methods` shape 404/405 correctly.
- **Throttle key quality**: `throttle_client_ip` resolves through `umbral::settings::client_ip` under the trusted-proxy policy (audit_2 H9) and buckets to a shared "unknown" without a proxy rather than trusting spoofable headers.
- **Caps**: `MAX_LIST_ROWS` clamp on list and CSV (`:5499`, `:3803-3807`), `MAX_BULK_ITEMS`, `MAX_NEST_NODES` + depth via the gate — no unbounded statement surfaces found.
- **Boot guards**: `action_by` panics with clear messages for a missing model/column or a non-unique lookup; version resolution and mount prefixes are single-sourced.
- **`commands.rs` generators**: the four scaffold templates were read; the emitted permissions/pagination/throttle code is sound (401-vs-403 contract, constant-time advice, ORM-not-raw-SQL note, `Action::Custom` default-deny) and the `expect` calls are on string literals only.
- **CONFIG unwraps**: all `CONFIG.get().expect("RestPlugin::routes was called")` sites are boot-ordering-guaranteed (routes() runs before serving), not request-reachable panics.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps4 #80` — `.action_by` shipped; its write-up claims the path runs under the resource `Permission::check` + object scope (`Action::Custom`) — `[fixed/verified held]` (feature shipped; the object-scope claim is refuted by REST-1, filed numbered, not absorbed here).
- `gaps4 #91` — nested-update reconciliation lifted into `umbral::orm::nested`; the trait "gained ONE hook, `check_update` (default allow), to mirror the create side" — `[fixed/verified held]` (hook exists; REST never implements it — REST-2).
- `gaps4 #20` / `review_3/codex/13` — REST rustdoc described pre-safe-by-default posture — `[fixed/verified held]` (module docs fixed; three per-item comments remain — REST-7).
- `gaps4 #3` / `review_3/codex/21` — PG read-back leaked hidden fields — `[fixed/verified held]`.
- `audit_2 plugin-rest H1/H2/H3` + `gaps4 #37/#79` — object scope on CRUD/bulk, nested child create gating, tree caps — `[fixed/verified held]`.
- `PERF-1` (`planning/review/performance.md`) — unbounded list `SELECT` — `[fixed/verified held]` (clamp in `fetch_rows`).

## Out-of-scope notes

- **core (MD-2 link)**: `FileField`/`ImageField` `validate_storage_key` runs only in serde `Deserialize` (`crates/umbral-core/src/orm/file_field.rs:219-231`); every REST write seam goes through the dynamic JSON path — `insert_json` (`lib.rs:4277`), `insert_json_in_tx` (`:4347`), `update_json_in_tx` (`:4452`), `update_json` (`:4977`), and the nested writer (core `nested.rs:959-961`) — so no REST write ever runs it. Reachability and impact are core's; see `planning/review_4/_scratch/core-2-models-relations.md` MD-2.
- **core**: `NestedWriteGate` has no `scope_update` hook (only `scope_create`, `nested.rs:171-180`), so the child resource's own write scope cannot run on nested updates — the scope half of REST-2's class; the fix is a core trait hook + a `RestNestedGate` impl.
- **umbral-openapi**: `gaps6 #20` `[open]` — client-gen `in_create`/`in_update` include `.hide()`-ed columns the REST server strips.
- **core (validation)**: `gaps6 #22` `[open]` — `max_length` overflow surfaces as a 500 via the bind; umbral-rest already renders `WriteError::Validator` as a 400 map, so the fix is core-side.
