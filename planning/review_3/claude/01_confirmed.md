# Confirmed findings — hand-verified

Each of these was traced end to end against the real code, adversarially (default assumption: the finder is wrong). The exploit chain and the specific lines are given so you can re-check without re-deriving.

---

## 1. CRITICAL — admin inline cell-edit: staff → superuser

**`plugins/umbral-admin/src/handlers/inline_edit.rs:184` → `crates/umbral-core/src/orm/dynamic.rs:1215`**
Also raised independently as inline_edit.rs:185, inline_edit.rs:187, dynamic.rs:1215 — one root cause.

**What happens.** A user who is `is_staff` and holds the admin `Change` permission on the `auth_user` table sends:

```
POST /admin/auth_user/<their-own-id>/cell/is_superuser
Content-Type: application/x-www-form-urlencoded

is_superuser=true
```

and their own row is updated to `is_superuser = true`. They are now a superuser — full authority over the admin, every model, every user. This is account takeover from the lowest privilege that can reach the admin at all.

**Evidence.**

- `is_staff` and `is_superuser` on `AuthUser` are `#[umbral(privileged)]` (`plugins/umbral-auth/src/lib.rs:291,295`), with a doc comment stating the untrusted write path "refuses to set it unless the caller authorizes it via `DynQuerySet::allow_privileged`". So the guard is real and these fields are exactly what it protects.
- The route is mounted: `plugins/umbral-admin/src/lib.rs:871` → `POST /{table}/{id}/cell/{field}` → `cell_edit_post`.
- `cell_edit_post` (`inline_edit.rs:128`) authorizes with `require_staff` + `permcheck::require(Change)` — table-level only. Its field-level checks are: `cfg.readonly_fields.contains(&field)` (a per-config list, **empty by default**) and a file/image-kind refusal. **It never checks `col.privileged`, `col.noform`, or `col.noedit`.** It then calls:
  ```rust
  DynQuerySet::for_meta(&model)
      .filter_eq_string(&pk.name, &id)
      .update_one(&field, &new_value)   // inline_edit.rs:184-186
  ```
- `DynQuerySet::update_one` (`dynamic.rs:1215`) looks the column up, validates the value, and builds `UPDATE <table> SET <col> = <value> WHERE <pk> = <id>` — with **no** `is_unauthorized_privileged` call. Compare `update_form` (`dynamic.rs:1305`) and `update_json` (`dynamic.rs:2377`), which both run `is_unauthorized_privileged(col, &self.allow_privileged)` / `normalise_update_body` and strip the column. `update_one` is the *only* dynamic write terminal that skips the guard.

**Why it's the right altitude to fix in the ORM, not the handler.** `update_one` is a public write terminal that silently trusts its caller to have vetted the column. Every *other* terminal vets it. The contract is inconsistent, and the admin handler is just the first caller to get burned — any future caller of `update_one` inherits the hole. Two fixes, both needed:

1. **In the ORM:** `update_one` should refuse an unauthorized `privileged` column the same way `update_form` does (and honor `noform`/`noedit`), or be renamed/documented as an explicitly-unguarded primitive that callers must gate — but given its name, guarding it is the least-surprising fix.
2. **In the handler:** `cell_edit_post` must reject a field that is `privileged` (without authorization), `noform`, or `noedit` — mirroring the form path — not only `readonly_fields`.

**Note:** this is present in 0.0.9 and earlier, not introduced by 0.0.10.

---

## 2. HIGH — secret / private / Masked columns leak in the Postgres write response

**`crates/umbral-core/src/orm/dynamic.rs:1822, 1966, 2130`**
Raised as dynamic.rs:1966 (twice, from two lenses).

**What happens.** On **Postgres** (the production-first backend), a REST create/update that echoes the written row back to the client includes columns that should never leave the server: `#[umbral(secret)]` columns, unauthorized `private` columns, and `Masked<T>` ciphertext. On SQLite the same call strips them. So the framework's own test backend hides the bug and production exposes it.

**Evidence.** Three dynamic JSON write-response builders each have a SQLite arm and a Postgres arm. Every SQLite arm filters; every Postgres arm does not:

| Builder | SQLite arm (filtered) | Postgres arm (unfiltered) |
|---|---|---|
| `insert_json` (non-tx) | `:1805` `.filter(\|c\| self.may_serialize(c))` | `:1822` `for col in &self.meta.fields` |
| `insert_json_in_tx` | `:1946` filtered | `:1966` unfiltered |
| fetch-one-json twin | `:2110` filtered | `:2130` unfiltered |

`may_serialize` (`dynamic.rs:368`) is the response gate: it returns `false` for `is_secret_column(col)` and for `private` columns the reader hasn't been granted. The Postgres arms bypass it entirely and serialize `col.name → decode_pg_to_json(...)` for every field.

**Fix.** Apply `.filter(|c| self.may_serialize(c))` on the three Postgres arms, identical to their SQLite twins. Better altitude: the read-back is duplicated six times across two backends and has now drifted once — extract "serialize this row through the field-visibility policy" into one helper both backends call, so a future column-visibility rule can't be added to one arm and forgotten on the other. (This is the same class as finding #3.)

---

## 3. HIGH — REST list filter/search is a blind extraction oracle over hidden columns

**`plugins/umbral-rest/src/lib.rs:3061,3070` → `plugins/umbral-rest/src/filtering.rs:164`**

**What happens.** A caller with list-read access — which under the safe-by-default `ReadOnly` permission is **anonymous** on any opted-in resource — can read the value of a column that is stripped from every response body, one predicate at a time:

```
GET /api/auth_user/?password_hash__startswith=$2b$12$a   → 200, rows present  (guess correct so far)
GET /api/auth_user/?password_hash__startswith=$2b$12$b   → 200, empty         (wrong)
```

The row count is the oracle. The same applies to any `#[umbral(secret)]` column, `Masked` ciphertext, `.hide()`d or `private` column — every value that the response layer is careful never to emit is fully recoverable through the filter.

**Evidence.**

- `list` handler builds the filter over the full field list: `parse_filters(&params, &model.fields, filters_on)` (`lib.rs:3061`) and `parse_search(term, &model.fields, restrict)` (`lib.rs:3070`). `restrict` is `None` unless the app configured `search_fields`.
- `parse_filters` (`filtering.rs:164`) validates a key only by *existence* in `columns` — `columns.iter().find(|c| c.name == field_name)` — with no `secret` / `private` / `hidden` / `may_serialize` check. It then builds the predicate. There is no visibility filter anywhere in the function.
- The "unknown field" error (`filtering.rs`, in the same block) enumerates `columns.iter().map(|c| c.name)` — so it also hands the attacker the full column list, secret names included, for free.

**Fix.** The list filter/search/order surface must be built over the reader-*visible* columns, not `model.fields` — the same policy (`is_secret_column` / `private` unless granted / `.hide()`) that governs the response. This is a contract, not a patch: "a column the client may not read, it may not filter, search, or order by either." The cleanest form is a single "columns this identity may reference" function that both the response serializer and the query-input parser consult, so the two can't drift — again the same root shape as #2.

---

## The pattern worth naming

All three are one mistake wearing three hats: **a visibility rule is enforced where the framework's author was looking, and skipped on a sibling surface they weren't.**

- #1: the mass-assignment guard is on `update_form`/`update_json`, skipped on `update_one`.
- #2: the `may_serialize` filter is on the SQLite read-back, skipped on the Postgres read-back.
- #3: the field-visibility policy governs responses, skipped on filter/search input.

A framework whose security depends on remembering to apply the same filter at N call sites will keep leaking at the N+1th. The durable fix, beyond the three point-fixes, is to make each of these rules reachable from exactly one place that every surface is forced through — and then a follow-up sweep specifically for "where else is `model.fields` / an unfiltered column loop used near a client boundary."
