# ORM query-API sufficiency

Can the query builder express what a real app needs, so plugin code never has to drop to raw SQL? (CLAUDE.md makes the ORM the single DB interface; any shape it can't express forces a contract violation.) Scope: the full public `QuerySet`/`Manager`/`DynQuerySet`/`Aggregate`/`M2M` surface in `crates/umbral-core/src/orm/`. Cross-checked against `bugs/gaps.md`, `gaps2.md`, `REAL-GAPS.md`, `features.md` — all gaps below are new.

**Headline:** the surface is **substantially more complete than CLAUDE.md's "80%" list** — it already has Q-objects (AND/OR/NOT), F-expressions, date-part extraction, full-text search, JSON/array operators, group-by aggregation, `IN (subquery)`, union/intersect/except, `get_or_create`/`update_or_create`/`upsert`/`bulk_update`, soft-delete, and `__`-traversal projection. The genuine gaps are a focused set led by **`select_for_update` (row locking)**.

---

## Coverage table

### Filtering lookups
| Capability | Status | Method / gap |
|---|---|---|
| eq, ne, gt/ge, lt/le | present | per-type in `column.rs` |
| in_ | present | `IntCol::in_`, `UuidCol::in_`, nullable variants |
| contains/icontains, startswith/istartswith, like/ilike | present | `StrCol` (`column.rs:175-247`) |
| endswith / iendswith | **absent** | no `endswith` anywhere. Workaround `.like("%suffix")` |
| range / between | **absent** | no `range`. Workaround `Q::and(col.gte(a), col.lte(b))` |
| isnull | present | on all nullable cols |
| regex / iregex | **absent** | no `regex` |
| date/year/month/day/hour/minute/second/week_day | present | `DateTimeColExt` → `ColExpr` (`column.rs:2672-2769`) |
| F-expressions (field-to-field) | **partial** | only `eq_f`/`ne_f` (`expr.rs:204-236`), Int/Fk/Str only — no `gt_f`/`lt_f`, not on datetime/float |
| OR / NOT / nested Q | present | `Q::and/or/not` + `&`/`\|` (`expr.rs:276-311`) |

### Relations
| Capability | Status | Method / gap |
|---|---|---|
| select_related (FK join) | present | `select_related`, `select_related_many`, `join_related` |
| nested spanning (`author__manager`) | present | walks `__` chain |
| prefetch_related (reverse FK / M2M batched) | present | `prefetch_related`, `prefetch_related_many` |
| M2M add/remove/set/clear | present | `M2M::add/remove/clear/set/fetch` |
| **reverse-relation filtering** (`.filter(comments__author=…)`) | **absent** | predicates span only the base table |
| **prefetch with a filtered queryset** (prefetch a relation constrained by its own predicate) | **absent** | prefetch takes a field name only |

### Aggregation
| Capability | Status | Method / gap |
|---|---|---|
| count, sum, avg, min, max | present | `Aggregate::{count,sum,avg,max,min}` |
| aggregate over whole set | present | `QuerySet::aggregate(&[(name, Aggregate)])` |
| values().annotate() group-by | present | `QuerySet::annotate(group_cols, aggs)` |
| **per-row annotate** | **absent** | `annotate` is GROUP-BY only |
| **having** | **absent** | no post-group filter |
| **conditional aggregates** (`Sum(Case(When…))`) | **absent** | Aggregate has no Case input |

### Subqueries
| Capability | Status | Method / gap |
|---|---|---|
| `__in = subquery` | present | `into_subquery` + `in_subquery` |
| **EXISTS / NOT EXISTS** | **absent** | `Subquery` only feeds `IN` |
| **OuterRef / correlated** | **absent** | `Subquery` wraps a standalone SELECT |

### Projection
| Capability | Status | Method / gap |
|---|---|---|
| values() (with `__` traversal) | present | `values(&[&str])` |
| values_list() | **partial** | no dedicated method; no `flat=True` |
| only() | present | `QuerySet::only(&[&str])` |
| defer() | **absent** | only the inverse exists |
| distinct() | present | `distinct()` |
| distinct on (cols) — Postgres | **absent** | explicitly deferred (`mod.rs:638`) |

### Write
| Capability | Status | Method / gap |
|---|---|---|
| create, bulk_create | present | |
| get_or_create, update_or_create, upsert | present | `mod.rs:2987/3028/3108` |
| bulk_update | present | one CASE UPDATE |
| queryset-level update / delete | present | `update_values`, `delete` |
| F-expression update (atomic increment) | **partial** | `update_expr` updates **one** column per call |
| cascade-on-delete | present | `FkAction` |

### Transactions / locking
| Capability | Status | Method / gap |
|---|---|---|
| atomic blocks | present | `db::begin()`, `on_tx`, Manager `.atomic()` |
| **savepoints / nested atomic** | **absent** | flat transactions only |
| **select_for_update (row locking)** | **absent** | no `FOR UPDATE` in `orm/` — **highest-severity gap** |
| **on_commit hooks** | **absent** | no post-commit side-effect hook |

### Conditional / ordering
| Capability | Status | Method / gap |
|---|---|---|
| Case / When | **absent** | no builder |
| Coalesce | **absent** | (lower/upper/length + date parts exist) |
| multiple order keys | present | chain `order_by` |
| **order by related field** (`order_by("author__name")`) | **absent** | base-table columns only |
| **nulls first / last** | **absent** | no `NULLS FIRST` control |

---

## Prioritized gaps

### High — forces raw SQL, commonly needed
1. **`select_for_update` (row locking).** Absent. No `.select_for_update()` primitive. Any "decrement stock / claim a job / debit balance" flow needs `SELECT … FOR UPDATE` to avoid lost-update races; today the only path is raw `sqlx`. Real in-tree consumers at risk: **`umbral-tasks`** job-claiming (already buggy - see [BROKEN-1](broken-features.md)) and **`examples/shop` ecommerce stock**. This is the single most important gap and is the same item as MISS-1 from the first review round - both audits converged on it independently.
2. **Reverse-relation filtering** (`Post::objects().filter(comments__author=…)`). Absent — predicates target the base table only. "Posts that have a comment by X" / "orders containing product Y" force a hand-rolled subquery or an N+1 in Rust. `umbral-admin`'s related-filter UI and M2M-faceted listings want this.
3. **EXISTS / correlated subqueries (OuterRef).** Absent — `Subquery` only feeds `IN (…)`. "Rows where a related thing exists" with a correlated condition can't be expressed; `IN` degrades on large id-sets.

### Medium — workaround exists but awkward
4. **`Case`/`When` + `Coalesce`.** Absent. Needed for computed status columns, conditional aggregates (`SUM(CASE WHEN paid THEN amount END)`), `COALESCE(nickname, name)` ordering. Admin list columns and dashboard rollups are the consumers.
5. **`HAVING` on grouped aggregates.** `annotate(group_cols, aggs)` has no post-group filter. "Authors with > 5 posts" needs HAVING; current workaround is fetch-all-groups + filter in Rust.
6. **Per-row annotation** (and therefore order-by-annotation). `annotate` is group-by only; no per-row computed column attached to model rows.
7. **F-expression breadth.** Only `eq_f`/`ne_f` on Int/Fk/Str (`expr.rs:204`). No `gt_f`/`lt_f` (can't express `WHERE start < end` across two columns), not on datetime/float; `update_expr` is one column per call.
8. **`order_by` related field + `NULLS FIRST/LAST`.** Can't `order_by("author__name")`; no null-ordering control.

### Low — rare / cosmetic
9. **`endswith`/`iendswith`, `__range`, `__regex`** — trivial composition workarounds.
10. **`defer()`, `reverse()`, true streaming cursor, `values_list(flat=True)`, slicing sugar, `DISTINCT ON`** — each has a workaround or is explicitly deferred.
11. **Savepoints / nested atomic / `on_commit` hooks** — flat transactions only; `on_commit` would matter for "enqueue task after commit" but no consumer demands it yet.

---

## Contract-violation note (raw SQL already in tree)

`plugins/umbral-cache/src/lib.rs:311-381` does **row-level reads/writes via raw `sqlx::query`/`query_as`** with SQLite `?` placeholders (`SELECT … WHERE key = ?`, `INSERT … ON CONFLICT … DO UPDATE`). This audit flags it as a real CLAUDE.md violation since the ORM has `upsert` + `filter().first()`.

**Reconciliation with [BROKEN-9 / the plugin-contract table](broken-features.md):** the nuance is the *pool*. The cache backend is constructed with an **explicit, possibly non-ambient** pool, while the ORM's `upsert`/`first` resolve the **ambient** pool via `OnceLock`. So the *query shape* is expressible through the ORM, but not against the pool the cache holds. The correct fix is therefore **not** "rewrite inline" but "add the missing ORM escape hatch" — a `Manager::upsert_with(&pool)` / `...first_with(&pool)` family that takes an explicit pool — then route the cache through it. That escape hatch should be a logged gaps2 entry (it currently lives only as a code comment in the cache backend). Either way it's SQLite-only today (`?` placeholders won't run on Postgres), so it needs fixing before the cache backend is Postgres-ready.
