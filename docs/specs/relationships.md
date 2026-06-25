# Relationships — ForeignKey and beyond

Status: **v1 shipped** (gap 14). This document records the v1 scope, the cut decisions, and the deferred backlog.

## What shipped at v1

`ForeignKey<T: Model>` is a generic field type that:

- Stores an `i64` reference to the primary key of model `T` in the SQL column.
- Emits `BIGINT REFERENCES "<T::TABLE>"("id")` in both SQLite and Postgres DDL via the migration engine.
- Serialises / deserialises transparently as `i64` (serde, sqlx `FromRow`, backup).
- Exposes `.id() -> i64`, `.set(i64)`, `From<i64>`, `Into<i64>`.
- Exposes async `.resolve(&SqlitePool) -> Result<T, sqlx::Error>` and `.resolve_pg(&PgPool) -> ...` for fetching the referenced row with one point-lookup query.
- Is detected by `#[derive(Model)]` and emits a `ForeignKeyCol<Owner>` constant in the sibling column module, supporting `.eq(i64)`, `.ne(i64)`, `.lt`, `.le`, `.gt`, `.ge`, `.in_(&[i64])`, `.asc()`, `.desc()`.
- The nullable variant (`Option<ForeignKey<T>>`) is detected by the derive and emits `NullableForeignKeyCol<Owner>` with the same surface plus `.is_null()` / `.is_not_null()`.

The `FieldSpec.fk_target: Option<&'static str>` field carries `Some(T::TABLE)` so the migration engine can emit the `REFERENCES` clause without the generic `T` in scope at render time.

## Cut decisions — what v1 does NOT do

### Non-`i64` FK targets

All FK columns store `i64`. A model whose PK is `uuid::Uuid` or `String` cannot be used as a `ForeignKey<T>` target at v1. The `FieldSpec` and `Column` structs have no mechanism to encode a non-integer FK column type. This is a deliberate simplification: the overwhelming majority of FK targets are integer PKs. Lifting it requires:

1. `ForeignKey<T>` becoming `ForeignKey<T, PkType = i64>` with a const-generic or a `ForeignKeyTarget` trait, and
2. `FieldSpec.fk_target_ty: Option<SqlType>` alongside `fk_target` so the migration engine knows the column type.

Deferred to post-M13. Users needing a UUID FK declare the column as `i64` / `Option<i64>` and manage the FK constraint in the migration JSON by hand.

### `ON DELETE` behaviour

The DDL emits no `ON DELETE` clause, which means the database default (RESTRICT on most engines) applies. A user who needs CASCADE, SET NULL, or SET DEFAULT edits the generated migration directly. The `Column` and `FieldSpec` types have no `on_delete` field. Adding one requires expanding the `SqlType::ForeignKey` encoding and landing it through the migration engine.

### Reverse accessors

There is no `User::posts()` or `user.posts` reverse relation. Computing the reverse requires either proc-macro inspection of all models at compile time or a runtime registry walk. Both options add complexity that v1 defers. Users wanting the reverse pattern write:

```rust
Post::objects().filter(post::AUTHOR.eq(user.id)).fetch().await
```

### Many-to-many relationships

Many-to-many needs a join-table helper type (the relation is backed by a junction table). Deferred.

### Eager loading / `select_related` / `prefetch_related`

No join or prefetch surface. Users who need to avoid N+1 either write a raw SQL query via `Manager::fetch_raw` (once landed) or batch with `.in_` predicates. Tracked as gap 28.

## Rationale for `fk_target` on `FieldSpec` rather than in `SqlType`

`SqlType` is `Copy`. Embedding `target_table: &'static str` inside the enum variant keeps the enum `Copy` and avoids heap allocation in `const FIELDS` slices. The alternative — a separate `ForeignKey { target_table: &'static str }` variant — was considered but rejected because `&'static str` in a serde-derived enum is not trivially deserialisable from JSON (the lifetime can't be synthesised from owned data). Moving the target table name to `FieldSpec.fk_target: Option<&'static str>` (static in the compiled binary) and `Column.fk_target: Option<String>` (owned in the migration file) keeps both serde paths clean.

## Migration engine notes

`build_column_def_sqlite` and `build_column_def_postgres` both check `if matches!(col.ty, SqlType::ForeignKey)` before the normal type dispatch and append the `REFERENCES` clause via `sea_query::ColumnDef::extra(...)`. This is necessary because sea-query (at v0.32) does not expose a first-class FK DDL API. The `extra` method renders the string verbatim after the column type and constraints, which produces correct SQL on both dialects.
