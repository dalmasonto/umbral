# Property-graph queries (SQL/PGQ) — zero-join FK traversal

Status: early exploration ("coming later" — parked until the Postgres feature ships and stabilizes)
Date: 2026-08-11
Coverage: planning/gaps5.md #104 (tf#328). This records the idea and the shape it would take, not a committed near-term design.
Relates: the ORM (`select_related`, joins), RLS, and the migration engine.

## The idea

PostgreSQL is gaining **SQL/PGQ** — the SQL:2023 "property graph queries" feature: you declare a *property graph* over existing tables (`CREATE PROPERTY GRAPH`) and query it with graph pattern-matching (`GRAPH_TABLE(g MATCH (a)-[:rel]->(b) …)`), the same idea as Cypher/GQL but standard SQL, over your ordinary relational tables. It's targeted for the PG 19 line.

umbral is unusually well-positioned to expose this, because **it already owns the graph**: every model declares its `ForeignKey` and M2M relationships, so umbral knows the vertices (models) and edges (FKs / junction tables) without the developer defining anything new. That means umbral can auto-derive the property graph and let you traverse linked tables as a graph pattern — deep or variable-length FK walks become one readable pattern instead of N explicit joins or a hand-written recursive CTE.

The one-line pitch: **zero joins — just a graph query.**

## Why this is worth it

The queries this makes easy are exactly the ones that are miserable today:

- **Hierarchies** — "everyone under this manager, any depth": `(:Employee)-[:reports_to]->{1,}(:Employee)`. Today that's a recursive CTE.
- **Friends-of-friends / social graphs** — 2–3 hop reachability across a join table.
- **Role / permission inheritance** — walk a role graph to the effective set.
- **Dependency graphs / bill-of-materials** — components of components.
- **Recommendation traversals** — "books read by people who read the books you read."

Each of these is a multi-join or recursive-CTE exercise now; as a graph pattern it's a single, legible line.

## The shape it would take

### 1. Auto-derive the property graph from models

umbral's model registry already has the metadata. The migration engine emits (Postgres only) a property graph kept in sync as models change:

- one **vertex table** per model,
- one **edge** per `ForeignKey` (directed source → target),
- one **edge** per M2M relationship (through its junction table),

roughly:

```sql
CREATE PROPERTY GRAPH umbral_graph
  VERTEX TABLES ( author, book, genre )
  EDGE TABLES (
    book        SOURCE KEY (author_id) REFERENCES author (id)
                DESTINATION KEY (id)   REFERENCES book (id)   LABEL wrote,
    book_genre  SOURCE KEY (book_id)   REFERENCES book (id)
                DESTINATION KEY (genre_id) REFERENCES genre (id) LABEL in
  );
```

This is a new PG-only migration operation (or a gated `RunSql`), regenerated when the relevant models/relations change — the developer defines nothing extra.

### 2. An ORM graph-query surface

A builder that compiles to `GRAPH_TABLE(...)` and hydrates typed results:

```rust
// deep genre lookup — no joins written
let scifi = Author::graph()
    .match_("(a:Author)-[:wrote]->(b:Book)-[:in]->(g:Genre)")
    .filter(genre::NAME.eq("scifi"))
    .select::<Book>("b")
    .fetch().await?;

// variable-length traversal — the real payoff
let org = Employee::graph()
    .match_("(ceo:Employee)-[:reports_to]->{1,8}(e:Employee)")
    .filter(employee::ID.eq(&ceo_id).on("ceo"))
    .select::<Employee>("e")
    .fetch().await?;
```

On Postgres this becomes `SELECT … FROM GRAPH_TABLE(umbral_graph MATCH … COLUMNS(…))`; results hydrate back into the selected model type across hops. Because the graph is defined over the base tables, **RLS still applies** — a graph query sees only the rows the caller may see.

### 3. One API, two backends

Consistent with umbral's "one ORM path, two backends" principle:

- **Postgres** — native SQL/PGQ (`GRAPH_TABLE`), the fast and expressive path.
- **SQLite** — no SQL/PGQ, so either compile *fixed-length* patterns down to the equivalent joins (the API still works), or PG-only-gate the *variable-length / recursive* patterns with a clear boot/`system-check` warning. Never let the SQLite branch silently diverge; if a pattern can't be honored on SQLite it fails loudly, not quietly-wrong.

## Open questions

- **Feature availability.** Exactly which PG version ships SQL/PGQ, and whether the first release is read-only (almost certainly — no graph *updates*, which is fine; umbral writes through the ORM as usual).
- **Auto vs opt-in.** Auto-deriving one big `umbral_graph` over every model vs letting the app declare named graphs over a chosen subset of models (smaller, purpose-built graphs). Likely: a default auto-graph plus opt-in named graphs.
- **Hydration across hops.** Mapping `GRAPH_TABLE` output columns back to typed model instances (and to path/edge results) needs a clean result contract.
- **SQLite fidelity.** Variable-length paths compile to recursive CTEs on SQLite; how far to take that vs just gating it.
- **Naming.** `umbral-graphql` already exists (the API layer), so this is *not* that. It reads as an **ORM capability** (a `graph()` entrypoint on the query surface) rather than a separate plugin; the derived graph is `umbral_graph`. Final name is open — candidates: keep it in the ORM as `graph()`, or a thin `umbral-pgq` module for the PG-specific bits.

## Why "coming later"

SQL/PGQ isn't broadly available or stable in shipping Postgres yet, and the SQLite fallback for recursive patterns is non-trivial. This is parked as an exploration: the moment the Postgres feature lands in a release we target, umbral's existing relationship metadata makes the auto-derived-graph path a short hop, and this note is the starting point.
