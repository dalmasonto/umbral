# Audit — `plugins/umbral-rest/`

Scope: the auto-REST plugin only. Every finding cites code I read. Files reviewed: `src/lib.rs` (4258 lines), `src/permission.rs`, `src/throttle.rs`, `src/auth.rs`, `src/resource.rs`, `src/filtering.rs` (partial), plus the `documentation/docs/v0.0.1/rest/` pages I own.

## Claims verified against code

The four "recently changed" claims all hold:

- **Fallback permission is `ReadOnly`.** `new()` sets `default_permission: Arc::new(ReadOnly)` (`lib.rs:573`); `permission_for` falls back to it (`lib.rs:333-338`); `ReadOnly` allows only `List`/`Retrieve` (`permission.rs:178-186`). Writes without an explicit opt-in get 403. ✔
- **List capped at 1000 rows.** `MAX_LIST_ROWS = 1000` (`lib.rs:94`), enforced as `req.limit.min(MAX_LIST_ROWS)` in `fetch_rows` (`lib.rs:3708`), and the CSV path uses the same ceiling (`lib.rs:2418-2425`). ✔
- **Bulk is opt-in via `.bulk()`.** `create` rejects a JSON array unless `cfg.bulk.contains(&table)` (`lib.rs:2631-2639`); `bulk_update`/`bulk_delete` self-gate with a 404 when not opted in (`lib.rs:2748-2752`). Batch capped at 1000 (`MAX_BULK_ITEMS`, `lib.rs:2676`, `check_bulk_size`). ✔
- **Recursive nested create + upsert landed.** `insert_nested_tree` (`lib.rs:2972`) and `upsert_nested_child` (`lib.rs:3250`) recurse to `MAX_NEST_DEPTH = 16` on one transaction. ✔

Also confirmed clean: SQL injection surface (filters/ordering/search build `sea_query` predicates with bound values and column names validated against `model.fields` — `filtering.rs:144-146`, `190-202`, `parse_search` escapes LIKE wildcards `filtering.rs:282-287`); raw DB errors are never echoed (`ApiError::Sqlx` → opaque 500, logged server-side, `lib.rs:2160-2171`); `password_hash` is stripped from every response un-overridably (`HARD_DENIED_FIELDS`, applied last in `apply_overrides_depth`, `lib.rs:1237-1239`); every built-in CRUD route and the custom-action dispatch pass through `gate()` (`lib.rs:2362,2559,2613,2753,3375,3448,3523`).

---

## A. Executive summary

The plugin's *table-level* authorization is solid and the safe-by-default rework is real: writes are 403 without opt-in, blocked tables are denied, `password_hash` never leaves in a response, lists and bulk batches are capped, and SQL is parameterized. The exposure that remains is **object-level**, and it is the single most important REST risk for a 10M-user multi-tenant system.

The three most urgent issues: **(1)** the built-in CRUD path has *no* object-scoping hook — `retrieve`/`update`/`destroy` look a row up by primary key alone, and the `Permission` trait is handed only `(action, identity)`, never the row or a queryset, so any caller who clears the table-level permission can read or mutate **any** row by id (classic IDOR / cross-tenant access). **(2)** Nested child writes run under only the *parent* resource's permission gate and skip `strip_hidden_for_write`, so a nested payload bypasses both a child resource's stricter permission class and the hidden-field write denylist (mass assignment, up to writing `password_hash` on a nested `auth_user`). **(3)** Nested fan-out is unbounded — depth is capped at 16 but the *number of items per level* is not, so one authenticated write can expand into an arbitrarily large single-transaction write (the bulk path is capped at 1000 for exactly this reason; nested is the uncapped bypass).

Secondary: throttling is entirely opt-in, so writes/bulk/nested/CSV/search carry no rate limit out of the box; and the boot-time "wide open" warning only fires for `AllowAny`, so the default `ReadOnly` silently exposes every business model's rows to anonymous reads with no warning.

What I could not assess: the ORM internals (`DynQuerySet`, `insert_json`/`update_json`, `noform` handling, transaction semantics) live in `umbral-core` and were out of scope; whether an outer tower layer imposes a request-body size limit (which would partially bound finding H-3) is not visible here; and the real permission classes apps actually configure.

## B. Findings table

| # | Severity | Area | Location | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------|---------|--------|-----------------|--------|
| H-1 | HIGH | Authz / IDOR | `lib.rs:2578-2586`, `2394,2421-2424`, `3461-3463`; `permission.rs:113-114` | Built-in `retrieve`/`update`/`destroy` scope the lookup to the pk only; `Permission::check` receives no row/queryset, and no `get_queryset`/`get_object` hook exists on `ResourceConfig` | Any caller past the table-level gate reads or mutates any row by id. Under the default `ReadOnly`, anonymous can retrieve/list every row; under `IsAuthenticated`, any user can read/update/delete another tenant's rows | Add a per-resource queryset-scoping hook (`fn scope(identity) -> Condition`) applied to *every* CRUD lookup, or an object-permission callback receiving the loaded row; until then, document that CRUD cannot be object-scoped | deferred: real fix is a framework primitive (per-resource queryset-scoping / object-permission hook on the CRUD path) — a design decision out of scope for a contained plugin fix. Doc requirement DONE: `rest/permissions.mdx` states plainly that built-in CRUD cannot be object-scoped and how to enforce per-owner access via `.views(...)` + custom `.action(...)`. |
| H-2 | HIGH | Authz + Mass assignment | `insert_nested_tree` `lib.rs:2972-3045`, `upsert_nested_child` `lib.rs:3250-3324` (no `strip_hidden_for_write`, no child `permission_for` check) | Nested child rows are written under only the *parent* handler's `gate(...)`; the child's own permission class is never consulted and hidden/HARD_DENIED fields are never stripped from child bodies | A user with parent-write access writes child tables they couldn't write directly (bypasses a child's `IsStaff`), and sets hidden fields (`is_admin`, `balance`, or `password_hash` on a nested `auth_user`) | Run `strip_hidden_for_write(child_table, …)` on every nested body, and enforce the child resource's `permission_for(child_table).check(Create/Update, identity)` before each nested write | ✅ done |
| H-3 | HIGH | DoS / unbounded | `insert_nested_tree` `lib.rs:3023-3044`, `update_nested` `lib.rs:3187-3211` (no per-level or total item cap) | Nested arrays have no size limit; only depth is capped (16). `check_bulk_size` (1000) guards the bulk path but is never applied to nested arrays | One authenticated request with a large nested payload expands to an unbounded number of INSERT/UPDATE statements on one long-lived transaction — lock contention, memory, and rollback cost; a DoS primitive | Apply a total-node cap across the whole nested tree (reuse `MAX_BULK_ITEMS`), rejecting oversize payloads with 400 before opening the tx | ✅ done |
| M-4 | MEDIUM | Authz / exposure | `lib.rs:1676-1692` (warn only when `is_open()`), `permission.rs:120-122,138-140` | The boot-time "anonymous read AND write" warning fires only for `AllowAny` (`is_open()==true`); the default `ReadOnly` triggers no warning | Adding `RestPlugin::default()` silently serves anonymous **reads** of every non-blocked business model (`customer`, `payment`, …) with no startup signal | Also warn when `authentication.is_anonymous()` and the effective permission allows anonymous *reads* on non-blocked app tables | ✅ done |
| M-5 | MEDIUM | Rate limiting | `throttle.rs:19-25`, `lib.rs:571` (empty `default_throttles`), `gate_throttle` `lib.rs:497-501` | Throttling is entirely opt-in; a default plugin imposes no limit on any action, including create/bulk/nested/CSV-export/search | Expensive and write endpoints have no abuse protection by default; combined with H-3 an attacker can hammer unbounded nested writes | Ship a conservative default throttle (or at least a boot warning when writes are enabled with no throttle); document that expensive actions need one | ✅ done |
| L-6 | LOW | Authz / exposure | `meta_for_table` `lib.rs:3051-3062` (no `allow()` gate) | Nested-child resolution resolves any registered table, ignoring `DEFAULT_BLOCKED_TABLES` / `exclude` | A developer can (accidentally) make a blocked table writable via a `.nested(...)` declaration even though its own `/api/` endpoint 404s | Reject a nested `child_table` that `!cfg.allow(child_table)` unless explicitly `expose`d | ✅ done |
| L-7 | LOW | Authz | `custom_action_dispatch` `lib.rs:3498-3510` (no `allowed_model`) | Custom-action dispatch looks the action up by `(table,name)` without calling `allowed_model`, so it never re-checks the block-list | An action registered on a blocked table stays reachable; low risk since actions are developer-declared | Call `allowed_model(&table)?` at the top of the dispatch for consistency with the CRUD handlers | ✅ done |
| L-8 | LOW | Error handling | `lib.rs:2037-2039`, `2172` | `sqlx::Error::Protocol(_)` and JSON parse errors are surfaced to the client as `BadInput`/`invalid_json` with `e.to_string()` | Minor internal-string leakage (protocol/parse detail); not SQL schema, but still framework internals in a 4xx body | Map Protocol errors to a generic 400 message; keep the detail in the server log only | ✅ done |

## C. Detailed findings (HIGH)

### H-1 — No object-level authorization on the built-in CRUD path (IDOR)

`retrieve` builds its lookup from the pk alone and applies no ownership filter:

```rust
// lib.rs:2571-2586
let pk = pk_column(&model)?;
let mut rows = fetch_rows(
    &model,
    Some((&pk.name, &id)),   // WHERE <pk> = <id>  — nothing else
    None, &no_filter, &include, &[],
).await?;
```

`update` (`lib.rs:3421-3424`) and `destroy` (`lib.rs:3461-3463`) do the same (`filter_eq_string(pk, id)`). The only authorization that ran is the table-level `gate` → `permission_for(table).check(action, identity)`, and the trait can't see the object:

```rust
// permission.rs:113-114 — no row, no queryset, ever
fn check(&self, action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError>;
```

`ResourceConfig` (resource.rs) exposes `permission`, `throttle`, `views`, `hide`, `nested`, … but **no queryset/get_object hook**. So there is nowhere in the auto-CRUD path to scope rows to the caller.

**Attack.** App serves `GET/PATCH/DELETE /api/invoice/{id}` behind `IsAuthenticated` (a normal multi-tenant setup). Tenant A logs in and requests `GET /api/invoice/9931` — a tenant-B invoice. The gate sees a valid identity, allows `Retrieve`, and the handler returns the row because the WHERE clause is `id = 9931` with no tenant predicate. `PATCH`/`DELETE` on the same id mutate/destroy B's data. Under the *default* `ReadOnly`, the same read works anonymously. This is textbook IDOR and the framework gives an app author no built-in way to close it on the CRUD path.

**Fix (framework).** Give resources a scoping hook that every CRUD lookup ANDs in:

```rust
// ResourceConfig
pub fn scope<F>(mut self, f: F) -> Self
where F: Fn(Option<&Identity>) -> Option<Condition> + Send + Sync + 'static { /* store */ }

// in retrieve/update/destroy, before fetch:
let mut where_cond = Condition::all().add(pk_eq);
if let Some(extra) = cfg.scope_for(&table, identity.as_ref()) { where_cond = where_cond.add(extra); }
// then filter_condition(where_cond) instead of filter_eq_string(pk, id)
```

Until such a hook ships, the docs must state plainly that built-in CRUD cannot be object-scoped and that per-owner access requires disabling those actions with `.views(...)` and reimplementing them as custom `.action(...)` endpoints (which do get `ctx.pk` + `ctx.identity`). See "Docs updated".

### H-2 — Nested writes bypass the child's permission class and the hidden-field write denylist

The single-object create/update strip hidden fields before writing:

```rust
// lib.rs:2651 (create) and lib.rs:3390 (update)
cfg.strip_hidden_for_write(&table, &mut body);
```

`grep` confirms `strip_hidden_for_write` is called at only four sites — `lib.rs:2651, 2712, 2793, 3390` — the single create, bulk_create, bulk_update, and single update. It is **never** called inside `insert_nested_tree`, `update_nested`, or `upsert_nested_child`. Those functions take the child body, set the FK, and hand it straight to the ORM:

```rust
// lib.rs:3026-3040 (insert_nested_tree) — no strip, no child permission check
let Value::Object(mut child_body) = item else { … };
child_body.insert(fk.clone(), pk_value.clone());
let crow = Box::pin(insert_nested_tree(cfg, &child, &mut child_body, tx, depth + 1)).await?;
```

Nor is the child's own permission consulted: the only `gate(...)` that ran is the parent handler's (`create`/`update`) against the *parent* table.

**Attack A (privilege field).** `Order` resource is writable by any authenticated user and declares `.nested("items", "order_item")`; `order_item` has a hidden `verified` column (`ResourceConfig::for_::<OrderItem>().hide("verified")`). `POST /api/order/ {"items":[{"product":7,"qty":1,"verified":true}]}` writes `verified=true` because the child body is never stripped. The same column set through `PATCH /api/order_item/{id}` would be dropped.

**Attack B (account takeover).** A resource declares `.nested("members", "auth_user")` (auth_user reachable as a nested child because `meta_for_table` ignores the block-list — see L-6) with a parent that permits writes. `password_hash` is in `HARD_DENIED_FIELDS` and stripped at the single-write sinks, but not on the nested path, so `{"members":[{"username":"x","password_hash":"<attacker-chosen argon2>"}]}` sets a known credential.

**Attack C (permission bypass).** `order_item` resource is `.permission(IsStaff)`; `order` is `.permission(IsAuthenticated).nested("items","order_item")`. A non-staff authenticated user writes `order_item` rows through the nested path, never hitting `IsStaff`.

**Fix.** Strip and gate every nested body against the *child's* config:

```rust
// at the top of insert_nested_tree / before each child update in upsert_nested_child
cfg.permission_for(&meta.table)
   .check(&Action::Create /* or Update */, identity)
   .map_err(|e| /* 401/403 */)?;
cfg.strip_hidden_for_write(&meta.table, body);   // incl. HARD_DENIED_FIELDS
```

(`identity` must be threaded down the nested recursion — it currently is not passed in.)

### H-3 — Unbounded nested fan-out

`insert_nested_tree`/`update_nested` iterate every item at every level with no count limit; the only guard is `MAX_NEST_DEPTH = 16` (`lib.rs:2939`):

```rust
// lib.rs:3023-3025 — items is the client array, unbounded
for (field, child, fk, items) in pending {
    for item in items { … Box::pin(insert_nested_tree(…)).await?; }
```

The bulk path caps at `MAX_BULK_ITEMS = 1000` (`check_bulk_size`, `lib.rs:2680-2685`), and the comment there says a bulk request "must never translate into an unbounded number of statements" — yet nested writes, which produce the same statement fan-out, apply no such cap.

**Attack.** A resource with nested children and any write permission accepts `POST /api/order/ {"items":[ {…}, … 200000 objects … ]}`. Each becomes an INSERT on one open transaction; nesting multiplies it (`items × components × …`). The transaction holds locks for the whole run, buffers rows in memory, and — if any item fails near the end — rolls the entire batch back. A handful of such requests exhausts DB connections/locks on a 10M-user deployment.

**Fix.** Count nodes across the whole tree and reject before opening the tx:

```rust
fn count_nested_nodes(cfg: &RestPlugin, meta: &ModelMeta, body: &Map<String,Value>) -> usize { /* recurse */ }
if count_nested_nodes(cfg, &model, &body) > MAX_BULK_ITEMS {
    return Err(ApiError::BadInput("nested payload exceeds the maximum node count".into()));
}
```

## D. Blind spots

- **ORM core (`umbral-core`).** `DynQuerySet::insert_json`/`update_json`/`*_in_tx`, `noform` stripping, `filter_eq_string`, transaction rollback-on-drop, and whether `insert_json` itself rejects unknown/pk/`noform` columns — all out of scope. H-2's "password_hash writable via nesting" assumes `insert_json` does not itself block `password_hash`; the REST-layer `HARD_DENIED` strip existing (gaps2 #75) implies it does not, but I could not confirm in the ORM.
- **Request body size limit.** Whether an outer axum/tower layer caps request bytes (which would partially bound H-3) is not visible in this crate.
- **`FilterClause`/`build_predicate` tail** (`filtering.rs` beyond line ~350) and the pagination module internals were only spot-read; I verified the injection-relevant paths (identifier validation, value binding, LIKE escaping) but not every lookup's coercion.
- **Auth backends.** `Authentication`/`Identity` live in `umbral-core::auth_contract` (re-exported here, `auth.rs:43-46`); token/session generation, expiry, and `is_anonymous()` semantics were not audited.
- **Real app configuration.** Which permission classes, throttles, and `expose`/`hide` calls production apps actually use — the defaults are what I assessed.

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. H-2: add `strip_hidden_for_write(child_table, …)` to the three nested write functions (thread `identity` + call `permission_for(child).check(...)` in the same edit).
2. H-3: add a total-node cap (`MAX_BULK_ITEMS`) checked before the nested tx opens.
3. M-4: extend the boot warning to fire when anonymous reads are allowed on non-blocked app tables.
4. L-6/L-7: gate `meta_for_table` and `custom_action_dispatch` through `allow()`.
5. L-8: map `sqlx::Error::Protocol` to a generic 400 body.

**Short term (< 2 weeks)**
6. M-5: ship a conservative default throttle or a boot warning when writes are enabled with none; document the expensive-action guidance.
7. H-1 (interim): documentation correction (done here) so authors don't assume CRUD is object-scoped.

**Structural (needs design work)**
8. H-1 (real fix): design and add a per-resource queryset-scoping / object-permission hook that every CRUD lookup (list/retrieve/update/destroy, and nested) enforces. This is the framework's biggest missing authorization primitive for multi-tenant use.

## Clarifying questions

1. Is object-level / per-tenant scoping intended to be a framework responsibility, or is the expectation that apps disable CRUD and hand-roll custom actions? (Changes H-1 from HIGH-bug to documented-limitation.)
2. Does the ORM's `insert_json` independently reject `password_hash` / pk / unknown columns on write? (Changes H-2 attack B severity.)
3. Is there an outer request-body-size cap in the standard `App` build? (Bounds H-3.)
