# ORM Docs — Restructure Plan

| | |
|---|---|
| **Date** | 2026-06-06 |
| **Status** | Plan approved — Tier 1 ready to execute; Tier 2 deferred |
| **Authors** | Dalmas Ogembo + Claude |
| **Scope** | The `documentation/docs/v0.0.1/orm/` page set: ordering, scoping, and the rule that prevents the regression from recurring. |

---

## 1. Context

Four ORM-implementation waves (gaps #13–#38.1 from `bugs/features.md`) shipped during the 2026-06-05 → 2026-06-06 session. Each wave added user-facing doc content. The content was correct but the placement was incremental: new sections appended to the nearest existing page rather than landing on a page whose scope matched the feature.

The result is the current state of `documentation/docs/v0.0.1/orm/`:

| pos | page | line count | what it actually covers |
|-----|------|------------|--------------------------|
| 1 | `models.mdx` | 469 | setup |
| 2 | `column-types.mdx` | 564 | setup |
| 3 | `relationships.mdx` | 354 | setup + reverse-FK + prefetch_related |
| 4 | `expressions.mdx` | 300 | **8 distinct topic clusters** (see below) |
| 5 | `transactions.mdx` | 199 | atomic + on_tx + builder default |
| 6 | `signals.mdx` | 145 | lifecycle hooks |
| 7 | `aggregates.mdx` | 112 | aggregate + annotate |

### 1.1 The `expressions.mdx` junk-drawer problem

The page is titled "Expressions" but its eight H2 sections cover:

1. F-expressions
2. Q-objects + `.exclude()`
3. `.values()` column projection
4. DB-function helpers (`.lower()`, `.upper()`, `.length()`, `.year()`, `.month()`, `.day()`)
5. QuerySet sugar (`.earliest()`, `.latest()`, `.distinct()`, `.explain()`)
6. Mutate-side terminals (`update_or_create`, `bulk_update`, `raw`)
7. Composition (`in_subquery`, `union`/`intersect`/`except`, `in_bulk`)
8. JSON column ops

Sections 3, 5, 6, 7 do not belong under "Expressions" by any reasonable reading. The reason they ended up there: when a Wave shipped, the nearest existing page was `expressions.mdx`, so the new section got appended rather than triggering a new page.

### 1.2 The ordering problem (smaller, separate)

`aggregates.mdx` sits at position 7 (after `signals.mdx`). A user wants to learn to *read and aggregate* data before they learn to *react to writes*. The current order has the read-side documentation behind the write-side reaction surface.

---

## 2. Decisions

### 2.1 Two-tier restructure, deferred shipping

**Tier 1 (30-minute fix — execute now):** rename + reorder only. No content moves.

| pos | page (renamed) | filename | rationale |
|-----|----------------|----------|-----------|
| 1 | Declaring models | `models.mdx` | unchanged |
| 2 | Column types | `column-types.mdx` | unchanged |
| 3 | Relationships | `relationships.mdx` | unchanged |
| 4 | **Querying** | `querying.mdx` (renamed from `expressions.mdx`) | retitle so the title matches the contents that are actually there |
| 5 | Aggregates and annotate | `aggregates.mdx` | move from pos 7 → 5 |
| 6 | Transactions | `transactions.mdx` | move from pos 5 → 6 |
| 7 | Signals | `signals.mdx` | unchanged |

Add an H1 intro paragraph to the renamed `querying.mdx` acknowledging that it currently covers more than just expressions and pointing forward to the Tier 2 split.

Update cross-links in the See-also lists of `relationships.mdx`, `transactions.mdx`, `aggregates.mdx`, `signals.mdx` to reflect the new positions and filename.

**Why Tier 1 first:** mechanical, no regression risk, ships the ordering fix immediately. The retitle alone removes the "Expressions covers writes" cognitive dissonance.

### 2.2 Tier 2 — content split (deferred)

The proper structure once split:

```
1. models.mdx           — setup
2. column-types.mdx     — setup
3. relationships.mdx    — setup + reverse-FK + prefetch_related
4. querying.mdx         — filter, exclude, order_by, limit, get/first/fetch,
                          count/exists, values(), earliest/latest, distinct,
                          explain, in_bulk
5. expressions.mdx      — slim back to: F, Q (+ exclude as Q::not sugar),
                          DB functions, JSON ops, in_subquery, set ops
6. aggregates.mdx       — aggregate + annotate (unchanged)
7. writes.mdx           — create, bulk_create, update_values, update_expr,
                          update_or_create, bulk_update, save,
                          delete_instance, delete, upsert, get_or_create, raw
8. transactions.mdx     — .atomic() / .on_tx() / builder default,
                          cross-linked from writes.mdx
9. signals.mdx          — lifecycle hooks (unchanged)
```

Mirrors how users actually approach the framework: **setup → read → compose → aggregate → write → wrap in transactions → react via signals.**

**Triggering condition for Tier 2:** ship when EITHER (a) a user complains about navigating `expressions.mdx`, OR (b) any single section in `expressions.mdx` grows another page-worth of content.

### 2.3 Forward-looking rule

**New rule, to prevent the regression from recurring:**

> When shipping a new family of features, add a NEW page rather than appending to the nearest existing page.

Concretely:
- A new QuerySet *category* (reads / writes / aggregation / signals) gets its own `.mdx` file from day one.
- A small addition to an existing category (e.g. one more DB function on `StrColExt`) extends the existing page.
- The line check: if appending the new section would push the page past ~600 lines, it's a new page instead.

The fact that I kept appending to `expressions.mdx` instead of spawning `querying.mdx` when `values()` shipped in Wave B, and `writes.mdx` when `update_or_create` shipped in Wave C, was the failure mode. Capturing this rule here so the next wave doesn't repeat it.

---

## 3. Execution

### 3.1 Tier 1 — concrete steps (single commit)

1. `mv documentation/docs/v0.0.1/orm/expressions.mdx documentation/docs/v0.0.1/orm/querying.mdx`
2. In the renamed file:
   - Change frontmatter `title: Expressions` → `title: Querying`
   - Change frontmatter `description:` to match new scope (something like: "All the QuerySet read, compose, and project surface — filter, expressions, projections, sugar, subqueries, JSON ops.")
   - Update `tags:` to include `querying`
   - Add an H1 intro paragraph (~3 sentences) explaining the page's current shape and pointing at Tier 2 as the future split.
3. Reorder `sidebar_position` frontmatter:
   - `querying.mdx`: 4 (was 4 — same)
   - `aggregates.mdx`: 5 (was 7)
   - `transactions.mdx`: 6 (was 5)
   - `signals.mdx`: 7 (was 6)
4. Update cross-links in See-also lists:
   - `relationships.mdx` → links to `expressions.mdx` become `querying.mdx`
   - `transactions.mdx` → reorder the See-also list
   - `aggregates.mdx` → update See-also if needed
   - `signals.mdx` → update See-also if needed
5. `cargo test --workspace --tests` to confirm no doctests reference the old path.
6. Commit message form: `docs(orm): rename expressions.mdx → querying.mdx + reorder pages (Tier 1)`.

### 3.2 Tier 2 — when triggered

Three new files (`querying.mdx` becomes slimmer, `writes.mdx` is new, `expressions.mdx` returns as a focused page):

1. Move the read-side sections of current `querying.mdx` into a new `querying.mdx` (it already has the name, just gets thinner): keep `values()`, `earliest`/`latest`, `distinct`, `explain`, `in_bulk`.
2. Move the write-side sections into a new `writes.mdx`: `update_or_create`, `bulk_update`, `raw`, plus pull canonical examples for `create`, `bulk_create`, `update_values`, `update_expr`, `save`, `delete_instance`, `delete`, `upsert`, `get_or_create` from `models.mdx` (which currently mixes setup with write examples).
3. Restore `expressions.mdx` to a focused expressions page: F, Q, exclude (as Q::not sugar), DB functions, JSON ops, in_subquery, set ops.
4. Reorder positions to match the section 2.2 table.
5. Update every See-also list across the page set.

Tier 2 is one commit per moved page (3 commits total) so the diff stays reviewable.

---

## 4. Open questions

1. **Should `.exclude()` move to `expressions.mdx` (Tier 2)?** Currently in `querying.mdx`. It's sugar for `Q::not`, which lives under expressions. Argument for keeping it in querying: users discover it as part of the filter chain (`.filter` / `.exclude` pair). Argument for moving: it IS expressions. Defer to Tier 2 execution; pick whichever reads better in context.

2. **Does `models.mdx` need a write-section trim during Tier 2?** It currently includes `create` / `save` examples inline. If they migrate to `writes.mdx`, what stays in `models.mdx` is just declaration + derive details. Cleaner but more churn — same defer-to-Tier-2 decision.

3. **Is the `~600 lines` cap from §2.3 the right threshold?** Picked it because `column-types.mdx` is 564 lines and still readable. If a future page wants to grow past it with cohesive content (e.g. comprehensive examples per field type), the rule should accommodate.

---

## 5. See also

- `CLAUDE.md` § "Documentation" — the "ship a feature, ship its doc page" rule that this restructure formalises further.
- `bugs/features.md` — the Wave A–D ORM features whose docs caused the junk-drawer accretion.
- `documentation/docs/v0.0.1/orm/_category_.json` — sidebar metadata for the ORM section.
