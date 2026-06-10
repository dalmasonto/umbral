# Security — ORM / SQL layer

Scope: `crates/umbra-core/src` (queryset/query building, `DynQuerySet`, F-expressions, ordering, migrations DDL, inspectdb, backup) and `crates/umbra-cli`. **This answers the two open questions in `bugs/security.md`.**

## Verdict on `bugs/security.md`

1. **"Is the ORM safe from SQL injection attacks?"** → **Yes**, for the audited surface. No injection vulnerability found.
2. **"Do we use proper prepared statements in the ORM's query generation?"** → **Yes.** Values are consistently bound through `sea_query_binder`/sqlx (`$N`/`?` placeholders); identifiers go through sea-query `Alias::new` quoting or explicit `"`-escaping.

The ORM is built on **sea-query** (`Expr::col(Alias::new(...))` for identifiers, `build_sqlx` value binding) and **sqlx parameter binding**. Every `format!()` that touches an identifier either escapes embedded `"` (`.replace('"', "\"\"")` / `quote_pg_ident`) or sources the identifier from compile-time model definitions (Rust identifiers, which cannot contain `"`). Runtime user input always reaches the DB as a bound parameter, never concatenated into SQL.

---

## ORM-1 — Unescaped LIKE wildcards in `contains`/`startswith`/`search` (LIKE-injection)
**Severity: low–medium** — functional bug + minor DB-side DoS. **Not** SQL injection: values are bound parameters.

- **File:**
  - `plugins/umbra-rest/src/filtering.rs:461, 468, 471` (`build_predicate`) — attacker-reachable via `?title__contains=`, `?title__icontains=`, `?title__startswith=`
  - `plugins/umbra-rest/src/filtering.rs:245` (`parse_search`) — attacker-reachable via `?search=`
  - `crates/umbra-core/src/orm/dynamic.rs:251` (`DynQuerySet::search`) — admin/REST search term
  - `crates/umbra-core/src/orm/column.rs:206, 226, 242, 248` (typed `contains`/`icontains`/`startswith`/`istartswith`)
- **Evidence:**
  ```rust
  // filtering.rs build_predicate — value flows straight from the query string
  "contains"   => Ok(expr.like(format!("%{value}%"))),
  "startswith" => Ok(expr.like(format!("{value}%"))),
  // dynamic.rs search
  let like_pat = format!("%{term}%").to_uppercase();
  ```
  No `ESCAPE` clause and no escaping of `%`/`_`/`\` anywhere (grep for `ESCAPE`/`escape_like` returns nothing).
- **Attack path:** `GET /api/posts/?title__contains=%25` (or `_`) treats the metacharacter as a wildcard, matching far more rows than intended (discloses rows the caller meant to filter past, silent semantic breakage). A pathological pattern (`%a%a%...` against a large text column) can force expensive scans (DB DoS). The value is bound, so this cannot escape the string literal — hence not SQLi.
- **Fix:** Escape `\`, `%`, `_` in the user substring before wrapping with framework wildcards and emit `ESCAPE '\'`. Centralize in one `escape_like_literal` helper called from all four sites. (Django does exactly this in its `contains`/`startswith` lookups.)

---

## Defense-in-depth notes (not currently reachable)
- **`CreateM2MTable` DDL interpolates names without `"`-escaping** (`crates/umbra-core/src/migrate.rs:2963-2976` Postgres, `:2840-2853` SQLite): `junction_table`/`parent_table`/`child_table`/`parent_col`/`child_col` are dropped into `CREATE TABLE "{...}"(...)` via plain `format!`. These derive from compile-time model identifiers and M2M is not produced by inspectdb, so no untrusted name reaches here today. Route them through `quote_pg_ident` for consistency so a future M2M-introspection path is safe by construction.
- **Postgres constraint-name interpolation** in `render_alter_column_postgres` (`migrate.rs:3137-3146, 3177, 3205`) wraps `{table}_{column}_key`-style names in `"..."` without escaping embedded quotes. Same reasoning — model identifiers, not runtime input. Tidy alongside the M2M fix if touched.

## Verified safe (positives)
1. **Filter values consistently bound, never concatenated** — typed `Predicate`s, `DynQuerySet::filter_eq_string`/`filter_in_strings`/`filter_condition`, and REST `coerce_value`/`build_in_predicate` all produce `SimpleExpr`/`Value` that bind via `sea_query_binder`. `coerce_value` also type-validates (parse-or-400) before binding.
2. **Identifiers go through `Alias::new` everywhere** — `column.rs`, `expr.rs` (F-expressions), `aggregate.rs` (columns validated against `Model::FIELDS` first), `m2m.rs`, `dynamic.rs`. User-supplied column names in `DynQuerySet` are validated against `meta.fields` and **silently dropped if unknown** (`order_by_col`, `select_cols`, `filter_eq_string`), so an attacker can't even name an arbitrary column.
3. **REST field-name validation strict** — `parse_filters` rejects unknown fields with 400 before SQL is built; `?include=`/`?fields=` validated upstream.
4. **`update_json`/`insert_json` never use JSON body keys as identifiers** — they iterate the known `meta.fields` schema and pull values by key; unknown keys ignored, column names from schema via `Alias::new`, values bound (`dynamic.rs:1312-1330`, `:1159-1163`).
5. **`Expr::cust`/`cust_with_values` sites** (`column.rs` JSON path, array, inet/cidr/mac, has-key) interpolate only the escaped column identifier and emit `$N`/`?` for all values. `json_has_key_predicate` (`column.rs:1584`) inlines a developer-supplied `&'static str` key, not user input.
6. **Migration DDL safe** — `render_operation` uses sea-query `Table::create/alter/drop/rename`; hand-written DDL escapes identifiers (`quote_pg_ident` `migrate.rs:3247`, `.replace('"',"\"\"")` in RenameColumn and SQLite alter-column, `'` escaping for default/choice literals). DDL takes no runtime input — migration files are developer-authored JSON; tracking-table inserts fully bound.
7. **inspectdb safe against a hostile DB** — SQLite `PRAGMA table_info("...")` escapes the table name (from `sqlite_master`); Postgres binds the table name. Generated code only produces `CreateTable` ops and Rust structs that must pass `#[derive(Model)]` — a column named `id"; DROP …` isn't a valid Rust identifier and won't compile.
8. **`backup.rs`** uses `quoted_ident` for table/column names (all from registry `ModelMeta`) and `?`/`$N` for values; rejects unknown columns in a dump.
9. **`explain()`** prepends `EXPLAIN` to a sea-query-built SQL string whose values still bind separately via `query_with`.
