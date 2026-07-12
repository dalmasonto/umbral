---
name: adding-a-migration-operation
description: Use when adding a new variant to migrate::Operation (a new kind of DDL — views, triggers, extensions, partitions) or a new field to ModelMeta. Lists every seam the compiler will and will NOT catch.
---

# Adding a migration Operation (or a ModelMeta field)

## Context

`migrate::Operation` is the enum every schema change lowers to. Adding a variant (features #73 added `CreateView` / `DropView`) touches more places than it looks, and **the compiler finds most of them for you — but not all.** This is the list, in the order the compiler surfaces them.

## Approach

### 1. Add the variant, then let `cargo build --workspace --all-targets` do the work

Five exhaustive `match`es will fail to compile. That is the good case — take the errors as a checklist:

| Site | What it wants |
|---|---|
| `migrate.rs` `Operation::table_name()` | the relation this op targets — used to route the op to the right pool in a multi-DB app |
| `migrate.rs` `classify_operation()` | `OpSafety::Safe` / `Warning` / `Unsafe` for the zero-downtime gate |
| `migrate.rs` `render_operation_sqlite()` | the SQLite DDL |
| `migrate.rs` `render_operation_postgres()` | the Postgres DDL |
| `umbral-cli/src/lib.rs` `op_kind()` | the short uppercase tag in the `checkmigrations` report |
| `inspect.rs` | the table/column counter for the `inspectdb` summary |

Use `--all-targets`: a couple of the matches live in **tests**, and a plain `cargo build` will not see them.

### 2. Also do the things the compiler will NOT catch

- **`suffix_for(ops)`** — the migration *filename*. Falls through to a generic name silently. Add a slice pattern; match the pair the user's edit actually produces (an SQL edit to a view is `[DropView, CreateView]` → `update_view_x`, not two separate migrations).
- **Ordering inside `diff()`.** `diff` returns a `Vec<Operation>` that is applied in order. If your op must run before or after the table passes, splice it there explicitly — nothing sorts it for you.
- **`is_false` / `#[serde(default)]`** on new fields, or you break every migration JSON file already on disk.

### 3. A new `ModelMeta` field breaks ~15 struct literals

`ModelMeta` has a `Default` impl but the test fixtures do **not** use `..Default::default()` — they list every field. Adding one field breaks all of them at once.

A mechanical fix works, but the naive version of it is a trap: `grep 'ModelMeta {'` also matches **`fn foo() -> ModelMeta {`**, and inserting fields after a function signature produces `expected identifier, found ':'`. Filter out lines containing `->` or `fn `, and remember `inspect.rs` and `validation.rs` build metas too.

## Why

The enum is the choke point on purpose: everything that mutates the schema goes through `Operation`, so the exhaustive matches are the framework's way of forcing you to answer *"is this safe to deploy?"* and *"how does this render on each backend?"* before your change can compile. Resist adding a `_ => {}` wildcard to any of those six matches — it would let the next variant silently render as nothing on SQLite, or silently classify as safe.

## Pitfalls

- **Never add a SQLite fallback that quietly diverges.** A Postgres-only op (materialized views, RLS, citext) must fail a **boot-time system check** on SQLite (`check.rs::framework_checks()`, `Severity::Error`), not degrade into a lookalike. `#[umbral(materialized_view)]` rendering as a plain `CREATE VIEW` on SQLite would give correct answers with an inverted performance contract — the worst kind of bug, because nothing errors.
- **Test the Postgres path against real Postgres.** The SQLite suite proves nothing about `MATERIALIZED`. See `throwaway-postgres-verification`. Assert against `pg_class.relkind`, not against your own renderer's output — otherwise you are marking your own homework.
- Backend type surprises live in the SELECT list, not the DDL: PG's `SUM(bigint)` is `NUMERIC` and will not decode into `i64`. `CAST(x AS BIGINT)` is standard SQL and works on both backends.

## See also

- `crates/umbral-core/src/migrate.rs` — `Operation`, `diff`, `diff_views`, the renderers
- `.claude/skills/throwaway-postgres-verification.md`
- `crates/umbral-core/tests/database_views.rs` — the end-to-end shape (real diff → real render → real rows)
