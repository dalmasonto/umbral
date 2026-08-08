# ORM: database backend stance and first-class custom managers

Status: draft for gaps5 #21 (tf #234) and gaps5 #24 (tf #237). The final call is the maintainer's.
Date: 2026-08-08
Relates: docs/decisions/2026-08-08-product-north-star.md (the Postgres-first stance)

This note answers two ORM backlog items in one place because they share the same grounding: what the real `Manager<T>` / `QuerySet<T>` surface is, and how the `DatabaseBackend` seam actually dispatches.

## Grounding: the real ORM entry points

Before either recommendation, here is the surface as it exists in `crates/umbral-core/src/orm/` today. Both proposals are written against these exact names.

- **`Model::objects()` is the only door.** It is not a trait method. The derive emits it as an inherent method on the struct (`crates/umbral-macros/src/lib.rs:2360`: `pub fn objects() -> ::umbral::orm::Manager<Self>`), mirroring the hand-written reference impl on `Post` (`crates/umbral-core/src/orm/post.rs:36`). It returns a fresh `Manager<T>`.
- **`Manager<T>`** (`crates/umbral-core/src/orm/queryset/mod.rs:68`) is a thin wrapper: `PhantomData<T>` plus an `atomic: Option<bool>` override. Its constructor `Manager::new()` is `pub(crate)` (queryset/mod.rs:79). Its chainable methods (`filter`, `exclude`, `all`, `only`, `with_deleted`, `aggregate`, `annotate`, `join_related`, ...) each call a private `queryset()` helper (queryset/mod.rs:3780) that builds the base `SELECT` from `T::FIELDS` and hands back a `QuerySet<T>`.
- **`QuerySet<T>`** (queryset/mod.rs:144) is the lazy, cloneable sea-query builder. Its constructor `QuerySet::new()` is also `pub(crate)` (queryset/mod.rs:421). Nothing hits the database until a terminal is awaited. The terminals live on `impl<T: Model> QuerySet<T>` (queryset/mod.rs:1409): `filter`, `exclude`, `order_by`, `limit`, `offset`, `first`, `fetch`, `get`, `count`, `exists`, `delete`, `update_values`, `update_expr`, `values`, `only`, `select_related`, `prefetch_related`, `annotate`, `aggregate`, plus the write path in `write.rs` (`create`, `bulk_create`, `get_or_create`, `update_or_create`).

The load-bearing fact for #24: **`Manager::new()` and `QuerySet::new()` are both `pub(crate)`.** User and plugin code outside `umbral-core` cannot construct either type from scratch. The only supported way to obtain a base query is `Model::objects()`. Any custom-manager pattern must therefore compose *from* `objects()`, not around a hand-built `Manager` or `QuerySet`.

- **The backend seam** is two layers. At runtime, `DbPool` (`crates/umbral-core/src/db.rs:65`) is a two-variant enum, `Sqlite(SqlitePool)` and `Postgres(PgPool)`, with `backend_name()` returning `"sqlite"` or `"postgres"` (db.rs:117). Every terminal resolves the pool via `pool_dispatched()` (db.rs:208) and matches on the variant. At schema/DDL time, the `DatabaseBackend` trait (`crates/umbral-core/src/backend.rs:39`) abstracts dialect via `name()`, `supports(BackendFeature)`, `map_type()`, and `map_column()`, with concrete `PostgresBackend` and `SqliteBackend` structs (backend.rs:108, backend.rs:112). `BackendFeature` (backend.rs:74) enumerates the capabilities umbral reasons about explicitly (`InsertReturning`, `UpsertOnConflict`, `ArrayColumns`, `JsonbColumns`, `FullTextSearch`, `UuidNative`, ...) so code asks `supports(feature)` instead of hard-coding `if name == "postgres"`. Predicates can even carry a per-backend override (`Predicate::cond_sqlite`, orm/mod.rs:169) picked at terminal time by `cond_for(backend_name)`.

## Part 1 (gaps5 #21): database backend stance

### Recommendation: declare Postgres-first for production, SQLite for dev and test, and publish a backend roadmap that is explicitly demand-gated.

This is the ORM-level restatement of the product north star (Stage 1 framework, Stage 2 self-hosted platform). The north star already commits umbral to a Postgres-shaped production posture (RLS, tenants, jsonb, full-text search, materialized views). Making the ORM's supported-backend list say the same thing keeps the story honest and stops us implying MySQL parity we do not test.

Concretely, the stance is three sentences an adopter can rely on:

1. **Postgres is the supported production backend.** Postgres-only field types and features (arrays, hstore, real jsonb, tsvector full-text search, CIDR/INET, native UUID) are first-class, and the system check fails at boot on an incompatible field rather than in production.
2. **SQLite is a first-class development and test backend, not a production target.** It is what `cargo test` runs against and what the quickstart uses, so the declare-migrate loop works with zero external services. Where SQLite cannot match Postgres semantics, umbral either degrades with a documented warning or the field's `supported_backends` excludes it, caught at boot.
3. **MySQL/MariaDB, SQL Server, Oracle, and CockroachDB are not first-class and are not on the near-term path.** They are demand-gated: we add one only when a concrete adopter workload justifies the ongoing test and maintenance cost.

### Why not add MySQL now

Because the cost is not a driver swap; it is a permanent second dialect to keep correct across the whole ORM. The seam already exists (`DatabaseBackend` plus `DbPool`), which is exactly why this is a stance question and not an architecture question. Adding a `Mysql(MySqlPool)` variant is easy; keeping every terminal, migration operation, and predicate correct on it forever is the expense. The maturity matrix (gaps5 #100) should list backend support explicitly so nobody infers MySQL from the seam's existence.

### What adding MySQL would actually require (the roadmap, if demand lands)

If a backend does get added, this is the real checklist, in dependency order, expressed against the code above:

1. **`DbPool` variant and routing.** Add `Mysql(MySqlPool)` to the enum (db.rs:65), extend `backend_name()`, `pool_dispatched()`, the connect/connect-lazy match arms (db.rs:418, db.rs:441), the health `SELECT 1` arm, and the transaction wrapper (`TransactionInner`, db.rs:818). Every terminal already dispatches through `pool_dispatched()`, so each `match` on `DbPool` in the ORM becomes a three-arm match that must be filled in, not a silent fallthrough. This is the bulk of the mechanical work and the compiler enumerates it for you once the variant is non-exhaustive.
2. **`DatabaseBackend` impl.** Add a `MysqlBackend` struct implementing `name()`, `supports()`, `map_type()`, `map_column()` (backend.rs:39). Set `supports(InsertReturning)` to `false`: MySQL has no `INSERT ... RETURNING`, so the write path must fall back to `last_insert_id()` for auto-increment PKs, which the SQLite branch already models but MySQL's semantics differ on. `UpsertOnConflict` maps to `ON DUPLICATE KEY UPDATE`, not `ON CONFLICT (col) DO UPDATE`; the upsert renderer needs a MySQL arm. `ArrayColumns`, `HStoreColumns`, `JsonbColumns` (real jsonb), `FullTextSearch` (tsvector), `CidrInet`, and `UuidNative` are all `false`, which the boot system check already turns into clear field-incompatibility errors.
3. **Dialect and placeholder differences.** sea-query has a `MysqlQueryBuilder`, so most SQL renders for free, but the differences that bite are: placeholder syntax (sqlx MySQL uses `?`, same as SQLite, unlike Postgres `$1`, so the raw-SQL exceptions in plugins would still be wrong, which is another reason the "plugins use the ORM, not raw SQL" rule matters), identifier quoting (backticks, not double quotes), `AUTO_INCREMENT` vs `SERIAL`/`INTEGER PRIMARY KEY`, no transactional DDL (migrations cannot wrap schema changes in a rollback-safe transaction), `utf8mb4` collation defaults, `TEXT`/`VARCHAR` length rules, and the lack of partial and expression indexes. Each is a concrete migration-engine or predicate-rendering task, not a config flag.
4. **Predicate per-backend overrides.** The JSON-operator path already carries a SQLite override via `Predicate::new_with_sqlite` (orm/mod.rs:201). Any predicate that renders dialect-specifically (JSON extraction, full-text, upsert conflict targets) needs a MySQL arm too, which means `cond_for` (orm/mod.rs:216) grows beyond its current two-way `"sqlite"` vs default split.
5. **Test matrix.** MySQL joins the ignored-by-default live-service tests (the pattern `crates/README.md` documents for `UMBRAL_TEST_POSTGRES_URL`), gated on a `UMBRAL_TEST_MYSQL_URL`, and CI must run it (gaps5 #93) or the backend rots.

The honest read: steps 1 and 2 are a day; step 3 is where mature ORMs spent years, and it never fully ends. That is the argument for demand-gating rather than pre-building.

## Part 2 (gaps5 #24): first-class custom managers and querysets

### The Django analogue and the umbral constraint

Django lets you attach a custom `Manager` subclass (`Post.published`) and/or a custom `QuerySet` with reusable chainable methods (`Post.objects.published().recent()`). The value is a named home for domain queries so `published` is defined once, not copy-pasted as `filter(status="published")` across the codebase.

umbral cannot copy Django's subclassing shape directly, because `Manager<T>` and `QuerySet<T>` have `pub(crate)` constructors and are generic structs, not user-subclassable classes. But it does not need to: the composition primitive is already there. `Model::objects()` returns a `Manager<T>`, `.filter(...)` / `.all()` return a `QuerySet<T>`, and `QuerySet<T>` is cheaply cloneable and fully chainable. A "custom manager" in umbral is therefore **a named function that returns a `QuerySet<T>` built by composing off `objects()`**, plus an optional extension trait when the domain queries are reusable across models.

### Recommendation: document three composition patterns; no derive is required for v1.

All three work against the real API today. They are ordered from simplest to most reusable.

#### Pattern A: inherent methods on the model (the common case)

The direct analogue of a Django custom manager. You already own an `impl` block on the model (the derive only adds `objects()` and the column module, so a hand-written `impl Post { ... }` alongside it is free). Each domain query returns a `QuerySet<Post>` that the caller keeps chaining.

```rust
use umbral::prelude::*;

#[derive(Model)]
struct Post {
    id: i64,
    title: String,
    status: String,
    published_at: Option<DateTime<Utc>>,
}

impl Post {
    /// Domain query: only published posts. Returns a QuerySet, so
    /// callers can keep chaining: `Post::published().order_by(...).fetch()`.
    pub fn published() -> QuerySet<Post> {
        Post::objects().filter(post::STATUS.eq("published"))
    }

    /// Composes on top of `published()` because QuerySet is chainable.
    pub fn recent_published() -> QuerySet<Post> {
        Post::published().order_by(post::PUBLISHED_AT.desc()).limit(10)
    }
}

// Usage reads like Django:
let posts = Post::recent_published().fetch().await?;
let count = Post::published().filter(post::TITLE.contains("rust")).count().await?;
```

Return `QuerySet<Post>`, never `Vec<Post>`, so the method stays composable and lazy; the caller chooses the terminal. This is the pattern to reach for first and the one the docs should lead with.

#### Pattern B: an extension trait on `QuerySet<T>` for reusable, cross-model filters

When the same domain filter applies to several models that share a shape (every model with a `deleted_at`, every model with a `tenant_id`), put it on an extension trait implemented for `QuerySet<T>` under a marker bound. Because `QuerySet::filter` needs `T: Model` and the column is model-specific, the clean version bounds the trait on a small marker trait the models implement (hand-written or, later, derive-emitted).

```rust
/// Marker: models that carry a `published` boolean column named "published".
trait Publishable: Model {}
impl Publishable for Post {}
impl Publishable for Article {}

/// Reusable chainable verbs for any Publishable model's QuerySet.
trait PublishableQueries<T: Publishable> {
    fn published(self) -> QuerySet<T>;
}

impl<T: Publishable> PublishableQueries<T> for QuerySet<T> {
    fn published(self) -> QuerySet<T> {
        // Uses the generic Predicate::col_eq escape hatch (orm/mod.rs:184)
        // because the concrete column module isn't reachable under a generic T.
        self.filter(Predicate::col_eq("published", true))
    }
}

// Now every Publishable model gets `.published()` mid-chain:
let posts = Post::objects().all().published().order_by(post::ID.desc()).fetch().await?;
```

`Predicate::col_eq` (orm/mod.rs:184) is the sanctioned escape hatch for exactly this generic-over-`T` case, where the typed `post::STATUS.eq(...)` column constant is not reachable. It trades the compile-time column-name check for reusability; use Pattern A when you have a concrete model and want the typo-catching typed column.

#### Pattern C: a repository newtype wrapping the model's queries

When you want a named object grouping several domain queries (a `PostRepo` you can pass around or swap in tests), a zero-field newtype whose methods return `QuerySet<T>` gives Django's "manager as a namespace" feel without needing `Manager<T>`'s private constructor.

```rust
pub struct PostRepo;

impl PostRepo {
    pub fn published(&self) -> QuerySet<Post> {
        Post::objects().filter(post::STATUS.eq("published"))
    }
    pub fn by_author(&self, author_id: i64) -> QuerySet<Post> {
        Post::objects().filter(post::AUTHOR_ID.eq(author_id))
    }
}
```

This is Patterns A composed under a name; it does not touch the ORM internals and needs no framework change.

### On a derive helper

**None is needed for v1, and the docs should say so explicitly** so nobody waits for one. Patterns A through C already deliver Django's ergonomics against the shipped API. The bar for adding a derive is that it removes real boilerplate the trait-based patterns cannot; today it would only save the one-line `impl` block, which is not worth a macro surface.

If demand appears, the smallest future hook that fits the existing derive is an attribute that emits an inherent shortcut, for example `#[umbral(manager_method(published = "status == 'published'"))]` expanding to Pattern A. That is a strictly additive follow-up, not a v1 requirement, and it should be designed only after the documented patterns have real usage to learn from. The reusable-marker case (Pattern B) could later be helped by a derive that emits the marker trait impl, but again: additive, demand-gated.

### What this commits us to

- Ship a user-facing docs page (`documentation/docs/v0.0.1/orm/custom-managers.mdx`) that leads with Pattern A, shows B and C, and states plainly that `objects()` is the only constructor and domain queries return `QuerySet<T>`.
- Do **not** expose `Manager::new()` or `QuerySet::new()` publicly to enable subclassing-style patterns. The composition-from-`objects()` model is the supported one; widening those constructors would invite hand-built querysets that bypass `T::FIELDS` seeding, default ordering, and the soft-delete snapshot that `Manager::queryset()` sets up (queryset/mod.rs:3790-3799).

## Open decisions for the maintainer

1. **#21:** Ratify Postgres-first-for-production as the stated ORM stance and publish it in the maturity matrix, or commit to a specific additional backend (which reprioritizes the roadmap checklist above into near-term work plus a CI service).
2. **#24:** Ratify "document the three patterns, no derive in v1," or ask for the `#[umbral(manager_method(...))]` derive hook now rather than as a demand-gated follow-up.
