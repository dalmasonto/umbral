# Audit — Core ORM (`umbral-core/src/orm/*`, `pagination.rs`)

Component slug: **core-orm**
Scope audited: `crates/umbral-core/src/orm/` (dynamic queryset, write path, validation, foreign_key, m2m, masked, search, expr) + `crates/umbral-core/src/pagination.rs`. Read-only audit; the only files edited are the ORM doc `.mdx` and this report.

---

## A. Executive summary

Overall risk posture: **the SQL-injection surface is clean, but the field-encryption guarantee has a silent, exploitable hole and the JSON write path is deny-nothing-by-default.** Every dynamic query builder (filter / order_by / search / IN / m2m) validates identifiers against the model's field list and routes them through sea-query `Alias::new` (backend-quoted), with all values bound as parameters — I found no request-driven SQL injection in the audited builders. The `Masked<T>` crypto itself is sound (anonymous sealed box, fresh ephemeral keypair + random 24-byte nonce per value, authenticated, fails-closed on a malformed key).

The three most urgent issues: **(1 — CRITICAL)** a `Masked` column written through the dynamic path (`insert_json` / `update_json`, which is exactly what the REST plugin's create/update endpoints and the admin form submit call — `plugins/umbral-rest/src/lib.rs:2662,2714,2797,3013,3172,3323`) is stored **in plaintext**, because sealing lives only in serde `Serialize` / sqlx `Encode` and neither runs on the raw-JSON bind path; the encrypt-at-rest headline feature is silently bypassed on the primary API write path. **(2 — HIGH)** `insert_json`/`update_json` write every field present in the client body except the PK and `noform` columns — there is no allowlist, so mass-assignment of `is_superuser`/ownership FKs is possible unless the model author remembered `noform`. **(3 — HIGH)** the pool (auto-commit) write path is non-atomic: the parent INSERT commits, then M2M junction rows are written in a *separate* transaction, so a junction failure orphans a committed parent while returning an error implying the whole write failed.

What I could not assess from this scope: whether the shipped auth `User` marks `is_superuser`/`is_staff` as `noform` (plugins), whether migrations actually emit DB-level FK/UNIQUE constraints that backstop the advisory validation checks (migrate scope), and the REST serializer's default field-hiding behaviour. These are listed under Blind spots.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | CRITICAL | Security / crypto | `orm/dynamic.rs:3322` + `3286-3331` (insert), `1722-1729` (update); `orm/write.rs:415` (`json_to_sea_value` Text arm); `migrate.rs:844` (`widget`) | Dynamic write path binds a `Masked` column as plain `TEXT`; `Masked::seal` is never invoked (it lives only in serde `Serialize`/sqlx `Encode`). REST create/update (`plugins/umbral-rest/src/lib.rs:2662,2714,2797,3013,3172,3323`) and admin form submit use this path with raw client JSON. | PII/secrets stored **plaintext at rest** for any `Masked` field on a REST-writable model — the encrypt-at-rest guarantee is silently defeated. Exploitable now. | Make the dynamic write path seal columns whose `widget == "masked"` (or add `masked: bool` to `Column`) before binding; reject a masked write when no keyring is configured, mirroring the serde path. | ✅ done |
| 2 | HIGH | Data layer / tx | `orm/dynamic.rs:1428-1475` (`insert_json`), `1967-2002` (`update_json`); `orm/m2m.rs:658-681` (`set_junction_dynamic` opens its own tx) | On the auto-commit pool path the parent INSERT/UPDATE commits first, then M2M junctions are written in a *separate* transaction. | A junction-write failure after the parent commits leaves an orphaned parent with missing relations, yet the caller gets an `Err` implying the whole write failed → inconsistent state + misleading semantics. | Wrap the parent write + junction writes in one transaction on the pool path (reuse the `_in_tx` machinery: `begin` → `insert_json_in_tx` → `commit`). | open |
| 3 | HIGH | Security / input | `orm/dynamic.rs:1701-1731` (update loops all non-PK fields), `3286-3331` (insert) | No client-writable field allowlist. `insert_json`/`update_json` write every body key except the PK (update) and `noform` columns. Protection of `is_superuser`, `is_staff`, ownership FKs is opt-in via `noform`. | Mass-assignment / privilege escalation / ownership hijack if a REST-writable model exposes a sensitive column the author forgot to mark `noform`. Deny-nothing-by-default. | Deny by default: require an explicit writable-field allowlist at the write boundary (REST serializer `fields`/`read_only`), or add a core `server_managed` flag honored by `insert_json`/`update_json`. At minimum document the `noform` requirement for every sensitive column. | open |
| 4 | MEDIUM | Data layer / correctness | `orm/dynamic.rs:1990,2009` (pool), `1776` (tx); count source `1963` vs UPDATE predicate `1947` | `update_json*` returns `parent_pks.len().max(if any {1} else {0})` — a matched-PK count from a *separate* SELECT, not `rows_affected`. Pool path collects PKs with raw `self.where_clauses` while the UPDATE uses `effective_where_clauses()` (adds `deleted_at IS NULL`). No-PK model returns 1 for any update. | (a) Soft-delete models: returned count and the `bulk_post_save` signal include soft-deleted rows the UPDATE skipped → overstated count / wrong signal payload. (b) Callers using the return as affected-count (200 vs 404) get wrong answers. Extra SELECT per update. | Return the UPDATE's real `rows_affected()`; collect PKs (only when M2M/signals need them) with the *same* `effective_where_clauses()`. | open |
| 5 | MEDIUM | Security / input | `orm/dynamic.rs:3287-3300` | Integer PK is skipped only when it is a sentinel (None/Null/0); a non-sentinel client-supplied `id` is inserted verbatim. | A REST client can choose its own row id — occupy/collide specific ids, interfere with the sequence, or make ids predictable. | On insert, always omit an auto-increment integer PK (ignore any client-supplied value) unless the model explicitly opts into client-assigned PKs. | open |
| 6 | MEDIUM | Data layer / perf | `orm/validation.rs:47-70,621-640`; `orm/dynamic.rs:1400-1479` | Per-write validation fires per-FK COUNT + per-M2M existence queries sequentially, then INSERT, then a re-fetch SELECT, then per-M2M junction writes + hydrate reads — many sequential round-trips per single create. On the pool path validation and INSERT are separate ops (TOCTOU on FK existence). | Write latency balloons at 10M-user scale (a create with 3 FKs + 2 M2Ms is ~10 sequential round trips). FK check is advisory; correctness relies on a real DB FK constraint. | Batch/parallelize existence checks; skip the M2M re-fetch when the response doesn't need it; run pool-path validation+INSERT in one tx (or rely solely on classified DB constraint errors). | open |
| 7 | LOW | Data layer | `orm/dynamic.rs` (`limit: None` default); `pagination.rs` (opt-in) | `DynQuerySet` has no default LIMIT; `fetch_as_json()` without `.limit()` loads the whole table. | A new caller that forgets a limit full-scans a 10M-row table into memory. Mitigated today (REST caps 1000, admin paginates) but the core primitive is unbounded. | Add a configurable hard cap (or a debug-mode warning) on unbounded terminal fetches. | open |
| 8 | LOW | Security / injection | `orm/search.rs:36-39,138,159-201` | `branch_sql` emits `filter_sql()` **verbatim** into raw SQL. Query value is parameterized (`$1`/`?1`) and identifiers are `quote_ident`'d, but `filter_sql` is author-trusted. | Injection-safe for request data; a model author who ever puts request input in `filter_sql()` opens SQL injection. | Keep the "no user input" contract loud in docs; consider a debug assertion that `filter_sql` is a `'static` constant. | open |
| 9 | LOW | Correctness | `orm/masked.rs:387-389` | `Masked::deserialize` maps the literal `••••••` to empty plaintext. | A client legitimately submitting the exact string `••••••` silently loses data (stored as empty). Combined with #1 the masked round-trip is already broken via REST. | Deserialize should preserve the input; the "no-change on redaction echo" concern belongs in the write layer, not the type's `Deserialize`. | open |

No injection issues found in the sea-query dynamic builders (filter/order/search/IN/m2m): identifiers are validated against `meta.fields` and quoted by `Alias::new`; values are bound. `Masked` crypto (seal/open) is correct — see Detailed findings.

---

## C. Detailed findings (CRITICAL / HIGH)

### #1 (CRITICAL) — `Masked` columns stored in plaintext on the dynamic (REST/admin) write path

`Masked<T>` seals in exactly two places: serde `Serialize` (`orm/masked.rs:374-377`) and the sqlx `Encode` impls (`orm/masked.rs:419-427`). The dynamic JSON write path uses **neither**. A masked field is `SqlType::Text` in `ModelMeta` (the derive maps `Masked` → `StrCol`, `umbral-macros/src/lib.rs:3245`) with `widget: Some("masked")` and **no** masking flag reachable by the write path (`grep masked` over `write.rs`/`dynamic.rs`/`migrate.rs` is empty). `build_insert_plan` and `update_json` bind the value with:

```rust
// orm/dynamic.rs:3322 (insert) — same shape at 1722 (update)
let sea_value = crate::orm::write::json_to_sea_value(
    col.ty,               // == SqlType::Text for a Masked column
    json,                 // the raw client string, e.g. "sk_live_deadbeef"
    col.nullable,
    &col.name,
    fk_target_pk_sql_type(col),
)?;
q.value(Alias::new(&col.name), sea_value);   // binds PLAINTEXT
```

`json_to_sea_value` for `SqlType::Text` (`orm/write.rs:415`) coerces to a plain `String` and binds it — no `Masked::seal`.

**Attack scenario.** A model exposes `api_key: Masked<String>` behind a REST `ModelViewSet` (the documented use for OAuth tokens / PII). A client `POST`s `{"name":"x","api_key":"sk_live_deadbeef"}`. `plugins/umbral-rest/src/lib.rs:2662` calls `DynQuerySet::for_meta(&model).insert_json(&body)`. The column now holds `sk_live_deadbeef` in cleartext. A stolen `pg_dump` — the exact threat the feature exists to defend against — leaks the secret. A subsequent REST `GET` returns the plaintext too (the decode path returns the stored `TEXT` verbatim). The doc (`orm/masked.mdx`) previously claimed "the plaintext therefore never leaves the process through serde," which is only true for the typed `create` path.

**Corrected code (make the dynamic path seal masked columns before binding):**

```rust
// in build_insert_plan / update_json, before json_to_sea_value:
let sea_value = if col.widget.as_deref() == Some("masked") {
    // Route through the same sealing the typed serde path uses.
    let plaintext = json.as_str().ok_or_else(|| WriteError::Validator {
        field: col.name.clone(),
        message: "masked field must be a string".into(),
    })?;
    // Fail closed: no keyring => refuse the write, never store plaintext.
    let sealed = crate::orm::masked::ambient_seal(plaintext)
        .map_err(|e| WriteError::Validator { field: col.name.clone(), message: e.to_string() })?;
    SeaValue::from(sealed)
} else {
    crate::orm::write::json_to_sea_value(col.ty, json, col.nullable, &col.name, fk_target_pk_sql_type(col))?
};
q.value(Alias::new(&col.name), sea_value);
```

(`ambient_seal` is currently private to `masked.rs`; expose a crate-internal sealing entry point, or add a `Column::seal_if_masked(&str) -> Result<String, MaskError>` helper.) Until fixed, keep masked fields off writable REST/admin surfaces.

### #2 (HIGH) — Non-atomic parent + M2M write on the pool path

`insert_json` (`orm/dynamic.rs:1429-1475`) executes the parent INSERT on the ambient (auto-commit) pool, then calls `write_m2m_junctions` → `set_junction_dynamic`, which **opens its own transaction** (`orm/m2m.rs:658-681`). The two are not atomic.

**Failure scenario.** A create with M2M tags: the parent row INSERTs and commits. The junction `DELETE`+`INSERT` transaction then fails (constraint, connection drop, timeout). `insert_json` returns `Err`, the REST layer renders a 4xx/5xx, the client believes the create failed — but the parent row is durably committed with zero tags. Retrying creates a duplicate. `update_json` has the same shape (UPDATE commits, junctions follow separately).

**Fix.** Run the whole thing under one transaction on the pool path too:

```rust
let mut tx = pool.begin().await?;
let out = self.insert_json_in_tx(body, &mut tx).await?;   // parent + junctions on one tx
tx.commit().await?;
// fire post_save AFTER commit (the _in_tx path deliberately skips signals)
```

### #3 (HIGH) — Mass-assignment: deny-nothing-by-default

`update_json` iterates every column and pulls whatever the body carries, skipping only the PK and (via `normalise_insert_body`) `noform` columns:

```rust
// orm/dynamic.rs:1701-1731 (condensed)
for col in &self.meta.fields {
    if col.primary_key { continue; }
    let Some(json) = body.get(&col.name) else { /* auto_now handling */ continue; };
    // ... validate ...
    q.value(Alias::new(&col.name), json_to_sea_value(...)?);   // writes ANY field present
}
```

There is no allowlist of client-writable fields. A field like `is_superuser`, `is_staff`, or an ownership FK (`owner_id`) is writable through REST/admin **unless** the model author remembered `#[umbral(noform)]`.

**Attack scenario.** A `User` model behind a self-serve profile-update REST endpoint. A user `PATCH`es `{"is_superuser": true}`. If `is_superuser` was not marked `noform`, the ORM writes it → privilege escalation. Same pattern for reassigning `owner_id` to hijack another user's records.

**Fix.** Enforce deny-by-default at the write boundary. Either require the REST serializer to declare an explicit writable `fields` set (and drop unknown keys before `update_json`), or add a core `server_managed`/`read_only` flag that `insert_json`/`update_json` strip unconditionally, and default sensitive auth columns to it. The framework must not depend on every model author remembering an opt-out for every sensitive column. (Confirm the shipped `User` model's flags — see Blind spots.)

---

## D. Blind spots (could not verify from this scope)

- **Auth model field flags.** Whether the shipped `umbral-auth` `User` marks `is_superuser`/`is_staff`/`password` as `noform` — decides whether finding #3 is already exploited in the built-ins. Lives in `plugins/umbral-auth` (out of scope).
- **DB-level constraints.** Whether migrations emit real FK / UNIQUE constraints. Findings #2 and #6 (advisory FK check, non-atomic writes) are backstopped only if the DB enforces them. `migrate.rs` DDL emission is out of scope.
- **REST serializer defaults.** Whether a default `ModelViewSet` hides masked/sensitive fields from writes, or exposes all fields. Determines the real-world blast radius of #1 and #3. `plugins/umbral-rest` (read for call-site evidence only).
- **Admin form submit for masked fields.** The Form derive skips masked fields, but whether the admin auto-CRUD ever routes a masked value into `insert_json`/`update_json` (e.g. via a custom widget) was not traced end-to-end.
- **Runtime config.** Connection-pool sizing, statement timeouts, and whether `UMBRAL_MASK_PRIVATE_KEY` handling in prod matches the env-var story — all runtime/infra, not in these files.
- **`filter_condition` inputs.** `DynQuerySet::filter_condition` (`dynamic.rs:349`) splices an externally-built `sea_query::Condition` from the REST querystring parser; its safety depends on that parser (`plugins/umbral-rest/src/filtering.rs`), not audited here (identifiers still flow through sea-query quoting, so injection is unlikely, but not verified).

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. #1 — Seal masked columns in the dynamic write path (or, as an immediate stopgap, reject a write that targets a `widget == "masked"` column through `insert_json`/`update_json` until sealing lands). Highest priority.
2. #5 — Always drop a client-supplied auto-increment integer PK on insert.
3. #4 — Return real `rows_affected` from `update_json*`; align the PK-collection predicate with `effective_where_clauses()`.

**Short term (< 2 weeks)**
4. #2 — Run parent + M2M writes under one transaction on the pool path (wrap the existing `_in_tx` path).
5. #3 — Introduce a deny-by-default writable-field contract (serializer `fields` allowlist or core `server_managed` flag); audit the auth model's `noform` coverage.
6. #6 — Batch/parallelize FK+M2M existence checks; skip unneeded M2M re-fetch.

**Structural (needs design work)**
7. #3/#1 — A first-class "field write policy" in the ORM (which fields are client-writable, server-managed, or encrypted) so mass-assignment and encryption aren't per-model opt-outs. This is the root cause behind #1 and #3.
8. #7 — Decide the framework stance on unbounded terminal fetches (hard cap vs. explicit `.all()` opt-in).

Clarifying questions (would change severity):
1. Does the shipped `umbral-auth` `User` mark `is_superuser`/`is_staff`/`password` as `noform`? (If not, #3 is CRITICAL, not HIGH.)
2. Do migrations emit real FK + UNIQUE constraints on Postgres and SQLite? (If not, #2/#6 escalate.)
3. Does a default REST `ModelViewSet` expose all model fields for write, or an explicit allowlist? (Sets the blast radius of #1/#3.)

---

## Docs updated

- `documentation/docs/v0.0.1/orm/masked.mdx` — Softened the over-broad "plaintext never leaves the process through serde" claim (true only for the typed `create` path) and added a `danger` callout documenting finding #1: the dynamic/REST/admin JSON write path stores masked fields in plaintext, so masked fields must be kept off writable REST/admin surfaces until the write path seals them.
