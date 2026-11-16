# review_4 · umbral-openapi

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 5,101 LOC (`plugins/umbral-openapi`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 2 medium · 1 low · 0 info — 3 new, 4 known |

## Summary

- The plugin walks the model registry (through umbral-rest's policy readers) and emits an OpenAPI 3.0.3 document plus Swagger UI; `client_gen.rs` is the other half — the `gen-client` TypeScript generator. The spec side is in good shape after the earlier sweeps: base path and pagination style are now read from the live REST config (hardening F1/F2 held), every column-listing site guards `is_hidden` (review_3's drift fixed and held), and the hidden / write-only / conditionally-visible trichotomy is handled deliberately and documented.
- Both MEDIUMs live in the generated-client contract, not the spec: the emitted TypeScript row interfaces type conditionally-visible private columns as required — contradicting umbral-rest's own documented promise that the generator emits `cost?: string` — and the Create/Update DTOs are built with no write-denial check at all, so the hard-denied/secret branch can make a create impossible through the generated types (the hide()-list half of the same sites is tracked as gaps6 #20, open).
- One LOW: the spec's `required` array omits the `col.default` exemption that both the server's required-check and the crate's own client apply.
- Routed leads resolved: **RUN-1 refuted for this crate** — spec property keys come from the SQL column name (`lib.rs:562`), which is exactly the dynamic REST wire contract, so the SQL-name-vs-serde-name concern does not apply here; `client_gen.rs:1007` is a custom paginator's declared envelope field name, not a column. **BUG-20 verified from the openapi side** — the merge is deliberately last-write-wins; core's `app.rs` comment claiming "picks the first" is stale (out-of-scope note below).
- Fix first: mirror the write-denied predicate into `in_create`/`in_update` (lands gaps6 #20 and OPENAPI-2 together; needs a public predicate from umbral-rest), then give the TS emitter an optionality notion so conditionally-visible columns become `?`.

## Findings

### OPENAPI-1 · MEDIUM · correctness/drift · self-verified
**Generated row interfaces type conditionally-visible private columns as required, contradicting the REST layer's documented `cost?: string` contract and the spec's own optionality — a consumer's code compiles and then reads `undefined` at runtime.**

- **Where**: `plugins/umbral-openapi/src/client_gen.rs:601-606` (the emit path), `:829-832` (`visible`), `crates/umbral-core/src/typegen.rs:260-266` (the emitter); contract promise at `plugins/umbral-rest/src/lib.rs:2206-2208`; spec's own handling at `plugins/umbral-openapi/src/lib.rs:573-580`.
- **Evidence**:
  ```rust
  // client_gen.rs:606 — row interfaces are filtered only by is_hidden
  out.push_str(&umbral::typegen::typescript_for(&strip_hidden(exposed)));
  // client_gen.rs:830-832 — and `visible` knows exactly one policy
  fn visible(table: &str, col: &Column) -> bool {
      !umbral_rest::is_hidden(table, &col.name)
  }
  ```
  ```rust
  // crates/umbral-core/src/typegen.rs:261-266 — the emitter's only optionality rule is `nullable`
  let ty = if column.nullable {
      format!("{base} | null")
  } else {
      base
  };
  let _ = writeln!(out, "  {}: {ty};", column.name);
  ```
  ```rust
  // plugins/umbral-rest/src/lib.rs:2206-2208 — what is_conditionally_visible documents
  /// the staff one. The truth is **optional** — the field may or may not be present — and that
  /// is what the spec says, so a generated TypeScript client emits `cost?: string` and makes
  /// the consumer check. Which is correct, because they do have to.
  ```
  A `#[umbral(private)]` column with an `allow_private_if` unlock is *not* hidden (`plugins/umbral-rest/src/lib.rs:1995-1999`: "calling it 'hidden' here would strip it straight back out of the response"), so `strip_hidden`/`visible` keep it, and with no `nullable` on the column the interface member comes out `cost: string;` — never `cost?:`. The spec side gets this right: its `required` list skips conditionally-visible columns (`lib.rs:578-580`).
- **Impact**: model `Post { cost: String, … }` with `.allow_private_if(cost, IsStaff)`. Staff callers receive `cost`; anonymous callers receive the same JSON without it. The generated `Post` row interface declares `cost: string`, so an anonymous consumer's `post.cost.toUpperCase()` (or any downstream use) type-checks and then throws `TypeError: Cannot read properties of undefined` in production — the exact failure the REST doc says the generator prevents. Nullable conditionally-visible columns fare no better: `cost: string | null` still promises the key exists.
- **Fix**: give the emitter an optionality rule for this class — either extend core `typegen` with a per-column "optional in output" hook (core has to learn the concept anyway; see out-of-scope) and pass `umbral_rest::is_conditionally_visible(table, &col.name)` from `emit_dts`, or post-process the emitted interfaces. Add a client-gen test with a private+unlock column asserting `cost?:`.

### OPENAPI-2 · MEDIUM · correctness/security-surface · self-verified
**`in_create`/`in_update` consult no write-denial policy, so Create/Update DTOs offer columns the REST write path strips — and for a hard-denied or `secret` non-nullable column the create is impossible through the generated types (the DTO demands it, the server strips it, the required-check then 400s on the same field).**

- **Where**: `plugins/umbral-openapi/src/client_gen.rs:895-903` (the whole filter), `:930-941` (requiredness), `:601-605` (stale premise comment); server side `plugins/umbral-rest/src/lib.rs:2079-2096` (`is_field_write_denied`, `pub(crate)`), `:2111-2117` (`strip_hidden_for_write`), call sites `:4246, :4333, :4443, :4650, :4892`; ORM required check `crates/umbral-core/src/orm/validation.rs:650-657`.
- **Evidence**:
  ```rust
  // client_gen.rs:895-903 — nothing here consults hidden / secret / privileged write-denial
  fn in_create(col: &Column) -> bool {
      !(col.primary_key || col.noform || col.privileged || col.auto_now || col.auto_now_add)
  }
  ...
  fn in_update(col: &Column) -> bool {
      in_create(col) && !col.noedit
  }
  ```
  ```rust
  // client_gen.rs:934 — required unless the client can omit
  let optional = col.nullable || !col.default.is_empty();
  ```
  ```rust
  // plugins/umbral-rest/src/lib.rs:2117 — the server's write guard (WEB-2), run on every write path
  body.retain(|k, _| !self.is_field_write_denied(table, k));
  // plugins/umbral-rest/src/lib.rs:2072-2073 — what that predicate covers
  /// - `#[umbral(secret)]` / a hard-denied name (`password_hash`, …) — never readable, never
  ///   writable through the API.
  ```
  ```rust
  // crates/umbral-core/src/orm/validation.rs:650-657 — the required check the stripped body then hits
  fn column_is_required(col: &Column) -> bool {
      !col.primary_key
          && !col.noform
          && !col.nullable
          && col.default.is_empty()
          && !col.auto_now
          && !col.auto_now_add
  }
  ```
  The spec itself is right: a hard-denied/`secret` column is skipped (`lib.rs:552-555`) because `is_write_only` returns false for it (`plugins/umbral-rest/src/lib.rs:2237-2239`). Only the TS generator disagrees.
- **Impact**: model `Account { email: String, password_hash: String }` served by auto-CRUD. Spec: `password_hash` absent, correctly. Generated TS: `AccountCreate.password_hash: string` — required (in_create true; non-nullable, no default). A caller following the types sends it; the REST create strips it (`:4246`) via `is_field_write_denied` (hard-denied name, `:2080-2082`); `validate_on_create` then fails the required check with `password_hash: This field is required.` — a 400 on the field the client was told to send, with no sendable value that succeeds. The milder variant (nullable or defaulted column) is the gaps6 #20 shape: the field is silently ignored. `in_update` has the same omission, so PATCH types advertise `password_hash?: string` that the server always drops. Note the fix cannot be done from client_gen alone today: `is_field_write_denied` is `pub(crate)` — the crate needs a public predicate (same `OnceLock` reader pattern as `is_hidden` at `plugins/umbral-rest/src/lib.rs:2216`).
- **Fix**: exclude `is_field_write_denied` columns from `in_create`/`in_update` (this is also gaps6 #20's prescribed fix and clears the stale comment at `client_gen.rs:601-605`), and either keep the column out of the spec identically (already true) or, for the write-only class, keep the `writeOnly` treatment exactly as `model_schema` does. Add a client-gen test asserting a hidden column is absent from both DTOs and a hard-denied column is absent from Create/Update.

### OPENAPI-3 · LOW · docs/spec-fidelity · self-verified
**The spec's `required` array omits the `col.default` exemption the server's required-check honours and the crate's own client applies — columns with a model default are declared required.**

- **Where**: `plugins/umbral-openapi/src/lib.rs:563-581`; contrast `crates/umbral-core/src/orm/validation.rs:650-657` and `plugins/umbral-openapi/src/client_gen.rs:934`.
- **Evidence**:
  ```rust
  // lib.rs:581 — the gate as shipped
  if !col.nullable && !col.primary_key && !col.auto_now && !col.auto_now_add && !col.noform {
      required.push(Value::String(col.name.clone()));
  }
  ```
  The comment directly above (`lib.rs:563-571`) frames the rule as "Non-nullable non-PK columns are what the client MUST supply — except when the framework supplies a default itself", but the implementation checks only the attribute-driven exemptions. The server also exempts model-declared defaults (`col.default.is_empty()` in `column_is_required`), the DDL emits `DEFAULT` for them (`crates/umbral-core/src/migrate.rs:6490-6499`), and the crate's own `push_create_type` already marks such fields optional (`let optional = col.nullable || !col.default.is_empty();`).
- **Impact**: a `status: String` with `#[umbral(default = "draft")]` is listed in the schema's `required`, so Swagger UI's "Try it" and third-party codegen from the published spec demand a value on POST that the API happily fills. Doc-fidelity, not breakage: the server accepts both shapes.
- **Fix**: add `&& col.default.is_empty()` to the gate (one clause), and extend the existing `model_schema` unit tests with a defaulted column asserting it is absent from `required`.

## Strengths

- **The hidden / write-only / conditionally-visible trichotomy is handled deliberately on the spec side**, each branch with its rationale in a comment: `hide()`/secret columns skipped entirely (`lib.rs:541-555`), `private`-without-unlock described via `writeOnly: true` because "private is a read policy — gaps3 #75" (`:548-561`), and conditionally-visible columns never required "whatever the model says" (`:573-580`). This agrees 1:1 with umbral-rest's `is_field_hidden`/`is_write_only` semantics, which were re-checked and match.
- **Live-config fidelity**: CRUD paths are built from `umbral_rest::registered_base_path()` (`lib.rs:332`, used `:386`, `:400`), pagination params from `umbral_rest::registered_pagination_style()` (`:362-363`, `:986+`), filter params from `filters_enabled_for` (`:837`) — the spec follows the booted REST plugin instead of hard-coded defaults. Hardening F1/F2 verified held.
- **Four-for-four column-walk guard parity**: every site that lists columns consults `is_hidden` — properties (`:553`), search fields (`:928`), the FK-expansion/`fields` picker (`:960`), and `filter_parameters` (`:1050`). review_3's one-missed-site drift is closed and stayed closed.
- **Swagger UI posture**: SRI hashes pinned only when serving the pinned default asset base, with the re-versioning hazard explained (`lib.rs:282-302`, audit_2 #9); the plugin mounts nothing at all in Prod unless opted in (`tests/prod_gating.rs` asserts it).
- **Client-gen is actually tested**: 7 integration suites — `client_gen.rs`, `client_auth.rs`, `client_pagination_auth.rs`, `gen_client_command.rs` (end-to-end through the real CLI dispatcher), `action_by.rs`, `integration.rs`, `prod_gating.rs` — plus 27 unit tests in `lib.rs`, including a per-model Uuid-PK id-typing case.
- **Debt hygiene**: the `pascal_case` duplication (hardening F5) was closed properly via the shared `umbral-casing` crate (gaps2 #77, shipped), and this crate now depends on it rather than carrying a copy.

## Clean areas checked

- Raw SQL / DB access: none — `[dependencies]` has no sqlx (dev-only) and the spec build is a pure registry walk over `ModelMeta` / REST policy readers; nothing to bind or inject.
- Dependency direction: `umbral` facade + `umbral-rest` (a declared, legitimate plugin pairing — the plugin's `dependencies()` names it) + `umbral-casing`; no `umbral-core` internals, no sibling-plugin surprises.
- Hidden-column leakage into the spec: properties, required, `?fields=`, `?include=`, `?search=` and filter params all scrub hidden columns (the four sites above) — a hidden `password_hash` never appears anywhere in the document.
- `noform` / `auto_now` / `auto_now_add` / PK never land in `required`; `noform` columns are documented as server-managed rather than as request fields.
- Plugin has no write endpoints of its own: `GET spec_url`, `GET ui_route`, plus the slash-less UI mirror (`lib.rs:248-260`) — no state-changing surface, so no CSRF/rate-limit concerns beyond docs-serving.
- The `gen-client` command writes files from a fixed template; no network, no process spawn, deterministic output for a given registry.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #20` — Generated TS client includes `hide()`-ed columns in Create/Update DTOs `[open]` — verified as stated at this commit (`in_create`/`in_update` at `client_gen.rs:895-903` check none of hidden/secret; the stale "a hidden field can be write-only" comment sits at `:601-605`). OPENAPI-2 is the residual of the same sites that #20 does not record: the hard-denied/`secret` branch and its impossible-create consequence (the hide()-list half is #20's).
- `gaps5 #35` (tf#248) — OpenAPI coverage misses handwritten app routes `[open]` — plugin `openapi_paths()` contributions DO merge now (`lib.rs:405-414`); routes registered directly on the app builder still are not described, and the module doc still declares the limitation (`lib.rs:16-19`).
- `hardening/plugins-review/umbra-openapi.md` F3 — per-resource `.permission(...)` not reflected in per-operation `security` `[open]` — only the global auth chain is emitted; a staff-only resource is documented identically to an open one.
- `hardening/plugins-review/umbra-openapi.md` F4 — spec rebuilt from the registry on every `/openapi.json` request `[open]` — `spec_handler` still calls `build_spec(cfg)` per request (`lib.rs:269-280`); no memoization for a document that is static after `App::build()`.
- `hardening/plugins-review/umbra-openapi.md` F1/F2/F5 — hard-coded `/api/` paths; hard-coded `page`/`page_size`; `pascal_case` duplication `[fixed/verified held]` — base path via `registered_base_path()`, pagination via `registered_pagination_style()`, casing via `umbral-casing` (gaps2 #77).
- `review_3` — `filter_parameters()` advertised hidden columns `[fixed/verified held]` — guard present at `lib.rs:1050`; the fourth column-walk site now matches its three siblings.
- `hardening/plugins-review/umbra-openapi.md` — stale module doc (claimed no securitySchemes, deferred pagination) `[fixed/verified held]` — the doc now documents securitySchemes and per-backend pagination (`lib.rs:20-26`).

## Out-of-scope notes

- RUN-1 (core slice 5, `planning/review_4/_scratch/core-5-runtime.md`) **refuted for this crate**: spec property keys are emitted from `col.name` (`lib.rs:562`), and the dynamic REST wire (responses, filters, ordering, `?fields=`, `?include=`, write bodies) is keyed by column name too, so there is no serde-name assumption here; `client_gen.rs:1007` compiles a custom paginator's *declared* `PaginationSchema` envelope field name, not a SQL column. The sqlx(rename)-vs-serde-name divergence is confined to core's typed typegen path (`crates/umbral-core/src/typegen.rs:266`).
- BUG-20 verified from this side: the plugin merge at `lib.rs:405-414` is deliberately last-write-wins (`paths.insert(path.clone(), item.clone())`, comment `:405-409`); core's note at `crates/umbral-core/src/app.rs:1513-1517` ("umbral-openapi's merge step picks the first") is stale — route registration and spec generation agree; no openapi-side defect.
- OPENAPI-1's cleanest fix is an emitter capability in core's `typegen.rs` (a per-column optional/predicate hook at `:261-266`); openapi can only post-process today — flag for the core reviewer.
- `is_field_write_denied` is `pub(crate)` (`plugins/umbral-rest/src/lib.rs:2079`), so gaps6 #20's and OPENAPI-2's fix need `umbral-rest` to expose a public predicate (or the facade to re-export one) — flag for the umbral-rest reviewer.
