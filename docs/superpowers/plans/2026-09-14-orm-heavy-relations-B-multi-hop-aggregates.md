# ORM Heavy Relations — Plan B: Multi-hop aggregates (Phase 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Aggregate across relation paths — `annotate_count("posts__comments")`, `annotate_sum/avg/min/max(alias, "posts__price")`, ordering and filtering on the annotated result — each aggregate computed independently so multiple annotations can never inflate each other.

**Architecture:** Every annotation compiles to one correlated subquery — `SELECT <AGG>(leaf.col) FROM <path…> WHERE <innermost link> = <outer row PK>` — added to the main SELECT as `expr_as(subquery, alias)`. The subquery's JOIN chain is built by Plan A's `walk_joins`, correlated to the outer row via the base PK. Because each aggregate lives in its own subquery, there is no shared JOIN and no GROUP BY on the base, so the classic multi-aggregate JOIN-multiplication bug is impossible by construction. Filtering on an aggregate wraps the annotated statement and filters the alias (a portable HAVING stand-in).

**Tech Stack:** Rust, `sea-query` / `sea-query-binder`, `sqlx` (SQLite + Postgres).

**Spec:** `docs/specs/orm-heavy-relations-epic.md` (sub-project B).

## Global Constraints

- ORM-only: aggregates emitted through sea-query subqueries; no string SQL.
- Multi-tenant: subquery tables via `schema_qualified_table(...)`.
- Backends: SQLite required for tests; PG parity by symmetry, PG-only assertions gated behind `UMBRAL_TEST_POSTGRES_URL`.
- Ambient pool preserved.
- Before each commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` (whole workspace).
- Behavioral tests: real rows, public `annotate_*` API, read the annotated value back — the anti-inflation test is mandatory.
- Depends on Plan A being merged (needs `RelPath::from_path`, `walk_joins`, `PathBase::TableRoot`).

## File Structure

- `crates/umbral-core/src/orm/queryset/aggregate_path.rs` (new) — the correlated-subquery builder: given `T`, a relation path, an aggregate kind, and an optional aggregated column, emit the correlated `SelectStatement`. One responsibility: path+agg → subquery.
- `crates/umbral-core/src/orm/queryset/mod.rs` — the public `annotate_*` builders and `filter_annotation`; wire them to `aggregate_path`. (`order_by_annotation` already exists — reused unchanged.)
- `crates/umbral-core/tests/annotate_relation_path.rs` (new) — behavioral tests.
- `documentation/docs/v0.0.1/orm/aggregates.mdx` (new) — doc page.

## Interfaces produced by Plan A (consumed here)

- `RelPath::from_path::<T>(path: &str) -> Result<RelPath, sqlx::Error>` — path → hops.
- `walk_joins(select, root_alias, hops, policy, prefix, registered) -> Result<Alias>` — appends JOINs, returns leaf alias. Use `NullJoinPolicy::Inner` (an aggregate over a null branch simply contributes nothing).
- `PathBase::TableRoot { table }`.

---

### Task 1: The aggregate kind enum + correlated-subquery builder

**Files:**
- Create: `crates/umbral-core/src/orm/queryset/aggregate_path.rs`
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (add `mod aggregate_path;`)

**Interfaces:**
- Consumes: `RelPath::from_path`, `walk_joins`, `NullJoinPolicy`, `schema_qualified_table`, `registered_models`, `pk_of` (make `pk_of` in `relation_resolve.rs` `pub(crate)` or duplicate the 6-line registry lookup — prefer exposing it).
- Produces:
  - `pub(crate) enum AggKind { Count, Sum, Avg, Min, Max }`
  - `pub(crate) fn build_aggregate_subquery<T: Model>(path: &str, agg: AggKind, column: Option<&str>, outer_pk_alias: sea_query::Alias, outer_pk_col: &str) -> Result<sea_query::SelectStatement, sqlx::Error>` — returns the correlated subquery. `Count` ignores `column` and counts DISTINCT leaf PK; the others require `column` (error if `None`).

- [ ] **Step 1: Write the failing test** (`crates/umbral-core/tests/annotate_relation_path.rs`)

Use models with a reverse-FK depth-2 path. Copy the App/registry preamble from `relation_traversal_integration.rs`. Define `User { id }`, `Post { id, author: ForeignKey<User>, price: i64 }`, `Comment { id, post: ForeignKey<Post> }`. Seed: 1 user, 2 posts (prices 10, 40), 3 comments on post 1, 0 on post 2.

```rust
#[tokio::test]
async fn annotate_count_over_two_hop_path() {
    // … App setup + seed …
    let u = User::objects()
        .annotate_count("posts__comments")            // count comments across the user's posts
        .fetch_annotated::<i64>("posts__comments_count").await
        .expect("annotated");
    // one user, 3 comments total across their posts
    assert_eq!(u, vec![3]);
}
```

Note: the exact terminal that reads an annotation value back — `annotate_*` sets a column on the SELECT; the existing `values(&[...])` / a small `fetch_annotated` helper reads it. If no such helper exists, add `pub async fn fetch_annotated<V>(&self, alias: &str) -> Result<Vec<V>, sqlx::Error>` in Task 3; for Task 1 assert through `to_sql()` first (see Step 4) then convert to the round-trip once the terminal lands.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test annotate_relation_path annotate_count_over_two_hop_path`
Expected: FAIL (`annotate_count` deep path / builder undefined).

- [ ] **Step 3: Write minimal implementation** (`aggregate_path.rs`)

```rust
use sea_query::{Alias, Expr, Func, Query, SelectStatement, SimpleExpr};
use crate::migrate::registered_models;
use crate::orm::relation::{RelPath, PathBase, NullJoinPolicy};
use crate::orm::queryset::relation_resolve::{walk_joins, pk_of};
use crate::orm::Model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggKind { Count, Sum, Avg, Min, Max }

pub(crate) fn build_aggregate_subquery<T: Model>(
    path: &str, agg: AggKind, column: Option<&str>,
    outer_pk_alias: Alias, outer_pk_col: &str,
) -> Result<SelectStatement, sqlx::Error> {
    let rel = RelPath::from_path::<T>(path)?;
    let registered = registered_models();
    let root_alias = Alias::new("__agg_0");
    let mut q = Query::select();
    // The subquery FROM is T's own table (aliased), joined out along the path.
    q.from_as(crate::db::router::schema_qualified_table(T::TABLE), root_alias.clone());
    let leaf = walk_joins(&mut q, root_alias.clone(), &rel.hops, NullJoinPolicy::Inner, "__agg_", &registered)
        .map_err(|e| e)?;
    // Correlate: subquery root PK == outer row PK.
    let root_pk = pk_of(&registered, T::TABLE)
        .ok_or_else(|| sqlx::Error::Protocol(format!("no pk for {}", T::TABLE)))?;
    q.and_where(
        Expr::col((root_alias, Alias::new(root_pk)))
            .equals((outer_pk_alias, Alias::new(outer_pk_col))),
    );
    // Aggregate expression on the leaf.
    let expr: SimpleExpr = match agg {
        AggKind::Count => {
            // COUNT(DISTINCT leaf.pk) — dedup a leaf reachable via multiple paths.
            let leaf_pk = /* pk of the leaf table via registered + rel.hops.last().to_table */;
            Func::count_distinct(Expr::col((leaf.clone(), Alias::new(leaf_pk)))).into()
        }
        _ => {
            let col = column.ok_or_else(|| sqlx::Error::Protocol(
                "sum/avg/min/max require a column path (e.g. posts__price)".into()))?;
            let c = Expr::col((leaf.clone(), Alias::new(col)));
            match agg {
                AggKind::Sum => Func::sum(c).into(),
                AggKind::Avg => Func::avg(c).into(),
                AggKind::Min => Func::min(c).into(),
                AggKind::Max => Func::max(c).into(),
                AggKind::Count => unreachable!(),
            }
        }
    };
    q.expr(expr);
    Ok(q)
}
```

Resolve during implementation: for `Sum/Avg/Min/Max` the aggregated column is the LAST `__` segment and is a *column*, not a relation — so the path passed to `from_path` is the relation prefix and the final segment is the column. Split the incoming `"posts__price"` into relation-path `"posts"` + column `"price"` for the non-Count kinds (Count takes the whole thing as a relation path). Make `pk_of` and `walk_joins` `pub(crate)` in `relation_resolve.rs`.

- [ ] **Step 4: Verify via `to_sql` first, then round-trip** — add a `#[test]` asserting the built subquery SQL contains `COUNT(DISTINCT` and one `INNER JOIN` per hop, then make the Step-1 round-trip test pass once Task 3's terminal exists.

Run: `cargo test -p umbral-core --test annotate_relation_path`
Expected: SQL-shape test PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/aggregate_path.rs crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/src/orm/queryset/relation_resolve.rs
git commit -m "feat(orm): correlated-subquery aggregate builder over relation paths"
```

---

### Task 2: Public `annotate_*` builders (deep count + sum/avg/min/max)

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`annotate_count` ~3170; add the new methods; mirror on the `&self` impl ~4255)

**Interfaces:**
- Consumes: `build_aggregate_subquery`, `AggKind` (Task 1).
- Produces (both on `QuerySet<T>` and its `&self` mirror):
  - `pub fn annotate_count(self, path: &str) -> Self` — now accepts a deep `__` path; alias = `"<path>_count"` (with `__` kept, e.g. `posts__comments_count`).
  - `pub fn annotate_sum(self, alias: &str, path: &str) -> Self`, and `annotate_avg` / `annotate_min` / `annotate_max` with the same signature.
  - Each stores `(alias, SelectStatement subquery)` on the QuerySet's annotation list, correlated to the base table's PK alias, so the terminal emits `expr_as(subquery, alias)`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn annotate_sum_over_path_and_no_cross_inflation() {
    // … seed: user with posts prices [10,40] and 3 comments on post 1 …
    let rows = User::objects()
        .annotate_sum("price_total", "posts__price")
        .annotate_count("posts__comments")
        .fetch_values(&["price_total", "posts__comments_count"]).await
        .expect("annotated");
    // sum is 50 and count is 3 — NEITHER inflated by the other
    // (a shared-JOIN impl would give price_total = 10+40 multiplied by comment rows).
    assert_eq!(rows[0].get("price_total"), Some(&json!(50)));
    assert_eq!(rows[0].get("posts__comments_count"), Some(&json!(3)));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p umbral-core --test annotate_relation_path annotate_sum_over_path_and_no_cross_inflation`
Expected: FAIL (`annotate_sum` undefined).

- [ ] **Step 3: Implement** the builders. Each calls `build_aggregate_subquery::<T>(path, kind, column, base_pk_alias, base_pk_col)` and pushes `(alias, subquery)` onto the annotation vec (reuse the existing annotation storage that `annotate_count`/`order_by_annotation` already share; if the existing storage holds a raw `SimpleExpr`, wrap the subquery as `SimpleExpr::SubQuery`). For `annotate_count` split so the whole path is the relation; for the others split the last segment as the column.

- [ ] **Step 4: Run to verify it passes** + regression

Run: `cargo test -p umbral-core --test annotate_relation_path` then `cargo test -p umbral-core --test annotate_count` (the pre-existing single-hop count test must still pass — deep path is a superset).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/tests/annotate_relation_path.rs
git commit -m "feat(orm): annotate_count deep path + annotate_sum/avg/min/max over relation paths"
```

---

### Task 3: `filter_annotation` (portable HAVING) + `order_by_annotation` coverage

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs`

**Interfaces:**
- Consumes: the annotation storage from Task 2; `order_by_annotation` (pre-existing).
- Produces: `pub fn filter_annotation(self, alias: &str, op: crate::orm::Op, value: sea_query::Value) -> Self` (and `&self` mirror) — wraps the annotated statement as `SELECT * FROM (<annotated>) AS __anno WHERE <alias> <op> <value>` so the alias is filterable on both backends. (`Op` = the framework's existing comparison enum used by predicates; confirm its name in `orm/mod.rs` and reuse it — do not invent a new one.)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn filter_annotation_cuts_rows_by_aggregate() {
    // … seed: userA with 3 comments across posts, userB with 0 …
    let ids = User::objects()
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Op::Gt, 0.into())
        .order_by_annotation("posts__comments_count", true)
        .values(&["id"]).await.expect("filtered");
    // only userA survives the HAVING-style cut
    assert_eq!(ids.len(), 1);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p umbral-core --test annotate_relation_path filter_annotation_cuts_rows_by_aggregate`
Expected: FAIL (`filter_annotation` undefined).

- [ ] **Step 3: Implement** — at the terminal, if any `filter_annotation` clause is set, build the inner annotated `SelectStatement`, then wrap: `Query::select().expr(Expr::asterisk()).from_subquery(inner, Alias::new("__anno"))` and add `and_where(Expr::col(Alias::new(alias)).<op>(value))`. Reuse the existing `Op`→sea-query mapping the predicate builder already has (extract a shared helper if it's inline).

- [ ] **Step 4: Run to verify it passes** + full workspace

Run: `cargo test -p umbral-core --test annotate_relation_path` then `cargo test`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/tests/annotate_relation_path.rs
git commit -m "feat(orm): filter_annotation — portable HAVING over aggregate annotations"
```

---

### Task 4: Doc page + full-workspace verification

**Files:**
- Create: `documentation/docs/v0.0.1/orm/aggregates.mdx`

- [ ] **Step 1: Write the doc** — purpose (aggregate across relation paths; each annotation is an independent correlated subquery so counts/sums never inflate each other), one example (`annotate_count("posts__comments").filter_annotation(...).order_by_annotation(...)`), and a `<Callout>` explaining the no-inflation guarantee vs. naive JOIN+GROUP BY. Link to the spec. Frontmatter `title`/`description`/`sidebar_position`. Prose not hard-wrapped.

- [ ] **Step 2: Full workspace gate**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add documentation/docs/v0.0.1/orm/aggregates.mdx
git commit -m "docs(orm): multi-hop aggregates over relation paths"
```

---

## Self-Review

- **Spec coverage:** correlated-subquery-per-annotation (Task 1), deep `annotate_count` + `annotate_sum/avg/min/max` (Task 2), `filter_annotation` portable HAVING + `order_by_annotation` reuse (Task 3), doc (Task 4). All of sub-project B's "Design" bullets map to a task. ✓
- **Anti-Django-bug test present:** `annotate_sum_over_path_and_no_cross_inflation` (Task 2) is the mandatory no-inflation assertion. ✓
- **Placeholder scan:** the two `/* … */` markers (leaf-pk lookup in Task 1, `Op` enum name in Task 3) are flagged as in-task decisions to resolve during Step 3, not deferrable TODOs. ✓
- **Type consistency:** `AggKind` / `build_aggregate_subquery` signatures identical across Task 1 (produces) and Task 2 (consumes); `filter_annotation` reuses the existing `Op` enum (Task 3 confirms the name rather than inventing one). ✓
- **Dependency on A:** every JOIN goes through Plan A's `walk_joins`; the plan states A must be merged first. ✓
