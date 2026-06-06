# DB Testing Session — SQLite vs Postgres Benchmarks

Date: 2026-06-04
Tester: Claude (automated Apache Bench suite)
App: examples/shop (e-commerce + content demo, 41 models)

## Test Setup

- **SQLite**: Local file (`shop.db`) with WAL mode enabled
- **Postgres**: PostgreSQL 18, `umbra_shop` database, user `umbra_shop` / password `umbra_shop`
- **Server**: Debug build, single-process Tokio runtime
- **Load**: Apache Bench (`ab`) — 1000 requests per concurrency level
- **Endpoints tested**:
  - `GET /api/post/` — list 20 heavy blog posts (~8KB JSON each = ~160KB response)
  - `GET /api/post/1/` — retrieve single post (~8KB JSON)
  - `GET /api/product/` — list 3 products (~2KB JSON)
  - `GET /` — home page (HTML template rendering)

## Summary Results

### Heavy List Endpoint (`GET /api/post/`)

| Concurrency | SQLite (req/s) | SQLite p99 (ms) | Postgres (req/s) | Postgres p99 (ms) |
|-------------|----------------|-----------------|------------------|-------------------|
| c=1         | ~106           | 19              | ~114             | 17                |
| c=10        | ~1,070         | 40              | ~1,100           | 19                |
| c=50        | ~1,190         | 98              | ~1,190           | 67                |
| c=100       | ~1,080         | 125             | ~1,190           | 122               |

**Observation**: SQLite (WAL mode) and Postgres are surprisingly close on read-heavy workloads in this debug build. Postgres has slightly better tail latencies at high concurrency. The bottleneck is likely JSON serialization + template/response generation, not the database.

### Single Retrieve (`GET /api/post/1/`)

| Concurrency | SQLite p50 (ms) | Postgres p50 (ms) |
|-------------|-----------------|---------------------|
| c=1         | 2               | 2                   |
| c=50        | 9               | 9                   |

**Observation**: Nearly identical. Single-row by-PK lookups are fast on both backends.

### Light List (`GET /api/product/`)

| Concurrency | SQLite p50 (ms) | Postgres p50 (ms) |
|-------------|-----------------|---------------------|
| c=1         | 3               | 3                   |
| c=50        | 12              | 12                  |

### Home Page (`GET /`)

| Concurrency | SQLite p50 (ms) | Postgres p50 (ms) |
|-------------|-----------------|---------------------|
| c=1         | 5               | 5                   |
| c=50        | 16              | 17                  |

## Bugs Found

### BUG-1: `.env` file is not automatically loaded [fixed]

**Severity**: Medium — affects developer experience
**Status**: Fixed in `1dd9e07` (`fix(settings): load .env files in Settings::from_env`).
**Repro**: Put `UMBRA_DATABASE_URL=postgres://...` in `.env` and run `cargo run -- serve`. The app still connects to the URL in `umbra.toml`.
**Root cause**: `Settings::from_env()` does not call `dotenvy::dotenv()` (or equivalent) to load the `.env` file into the process environment.
**Workaround**: Export the env var explicitly before running: `UMBRA_DATABASE_URL=postgres://... cargo run -- serve`
**Fix**: Add `dotenvy::dotenv().ok()` at the top of `Settings::from_env()`.
**Implemented**: `Settings::from_env()` now reads project-root `.env` values through Figment without mutating the global process environment; real process env vars still override `.env`.

### BUG-2: `loaddata` panics on Postgres — backup module is SQLite-only [fixed]

**Severity**: High — blocks data portability
**Status**: Fixed in `49efd0c` (`fix(backup): support Postgres dumpdata and loaddata`).
**Repro**:
1. Set database to Postgres
2. Run `cargo run -- dumpdata --output dump.json`
3. Run `cargo run -- loaddata dump.json`
**Expected**: Data loads into Postgres
**Actual**: Panic at `crates/umbra-core/src/db.rs:98`:
```
 umbra: a Postgres pool is registered but this code path still reads SqlitePool.
```
**Root cause**: `backup::dump_one()` and `backup::load_one()` take `&sqlx::SqlitePool` directly:
```rust
async fn dump_one(pool: &sqlx::SqlitePool, model: &ModelMeta) -> ...
async fn load_one(pool: &sqlx::SqlitePool, model: &ModelMeta, ...) -> ...
```
The callers (`dump()` and `load()`) pass `crate::db::pool()` (a `DbPool`) through a `Deref`/`AsRef` that calls `sqlite_or_panic()`.
**Fix**: Change `dump_one` and `load_one` to accept `&DbPool` and dispatch SQL generation per backend. For Postgres, use `$1` placeholders instead of `?`, and handle JSONB/UUID binding properly.
**Implemented**: `backup` now dispatches on `DbPool`, keeps the SQLite path intact, and adds Postgres readers/binders for core ORM types including JSONB, UUID, arrays, bytes, network types, full-text vectors, and decimal. Added `backup_postgres` regression coverage plus the missing ORM `DecimalCol` wrapper the derive macro already emitted.

### BUG-3: `bulk_create` serializes `serde_json::Value` as text on Postgres

**Severity**: High — breaks any model with JSON/JSONB fields on Postgres
**Repro**: On Postgres, call `Model::objects().bulk_create(vec![instance])` where the model has a `serde_json::Value` field (e.g., `Product.metadata` or `Product.dimensions`).
**Expected**: Rows insert successfully
**Actual**:
```
ERROR: column "dimensions" is of type jsonb but expression is of type text
```
**Root cause**: `orm::write::json_to_sea_value()` handles `SqlType::Json` by returning `SeaValue::String(Some(Box::new(value.to_string())))`:
```rust
SqlType::Json => {
    // Store the JSON as-is — sqlx-sqlite will TEXT it, sqlx-pg
    // will jsonb-encode it. sea-query has a Json variant when
    // its `with-json` feature is on; we're going through the
    // string path for portability.
    Ok(SeaValue::String(Some(Box::new(value.to_string()))))
}
```
This assumption is wrong: sqlx-pg does **not** automatically coerce a string `"{}"` to jsonb. It needs to be bound as `sqlx::types::Json<T>` or the SQL needs an explicit `::jsonb` cast.
**Fix**: In the Postgres branch of `bulk_create`, bind JSON values via `sqlx::types::Json(serde_json::Value)` instead of plain strings. Or add a `SeaValue::Json` variant that sea-query's Postgres builder can handle.

### BUG-4: `dumpdata` on SQLite produces `"{}"` string for JSON fields instead of preserving type

**Severity**: Low — cosmetic, but could cause issues on round-trip
**Observation**: In the dumped JSON, `metadata` fields appear as `"{}"` (a JSON string) rather than `{}` (a JSON object). This is because `json_to_sea_value` serializes to string before storing. On round-trip to Postgres, even if BUG-2 and BUG-3 were fixed, the dump format might need explicit type annotations.

## Raw Results Files

- `/home/dalmas/E/projects/umbra/bugs/db-testing-results/sqlite_ab_results.txt`
- `/home/dalmas/E/projects/umbra/bugs/db-testing-results/postgres_ab_results.txt`
- `/home/dalmas/E/projects/umbra/bugs/db-testing-results/shop_dump.json` (2.4MB dump from SQLite)

## Recommendations

1. **Fix BUG-3 next** — `bulk_create` with JSON/JSONB fields is now the remaining serious Postgres blocker.
2. **Verify BUG-4 against current JSON dump behavior** — the backup path now reads JSON columns as `serde_json::Value`, but the original shop dump should be rechecked before marking this closed.
3. **Consider a release build benchmark** — debug builds skew absolute numbers; the *relative* SQLite vs Postgres comparison is still valid.
4. **Test write-heavy endpoints** — all tests above were reads. Postgres write performance (especially with concurrent writers) will diverge more significantly from SQLite WAL.
