//! The database backend abstraction.
//!
//! `DatabaseBackend` is the seam where dialect differences live. The
//! trait sits on top of sea-query (which already abstracts dialect
//! rendering) and sqlx (which abstracts drivers); umbra adds the
//! umbra-specific reasoning layer on top so the system check (`check`)
//! and the migration engine (M5, `06-migration-engine.md`) can ask the
//! same questions of every backend.
//!
//! M4 ships two backends:
//!
//! - [`SqliteBackend`] — the runtime default. SQLite is what the M0–M3
//!   pool already opens; this just gives it a queryable identity in the
//!   check phase.
//! - [`PostgresBackend`] — declared and queryable for compatibility
//!   checks, but the umbra pool is still `sqlx::SqlitePool` at M4. The
//!   real `sqlx::PgPool` wiring lands when there's a real user need;
//!   the trait is in place so M5's migration engine can render Postgres
//!   DDL today and run it tomorrow.
//!
//! `MySqlBackend`, `OracleBackend`, and friends stay in the deferred
//! backlog per PRD §14.
//!
//! See `docs/specs/05-backends-and-system-check.md` for the target
//! design and the rationale for each `BackendFeature` variant.

use std::sync::OnceLock;

/// One umbra-supported relational backend.
///
/// Trait surface kept narrow at M4: identity (`name`), feature queries
/// (`supports`), and SQL-type mapping for the migration engine
/// (`map_type`). `quote_identifier`, `render_upsert`, and dialect-
/// specific rendering helpers get added when M5's migration engine and
/// bulk-insert paths need them; sea-query exposes those via per-backend
/// `QueryBuilder` types rather than a single dialect enum, so umbra
/// dispatches through `name()` for now and adds typed rendering helpers
/// when there's a real consumer.
pub trait DatabaseBackend: std::fmt::Debug + Send + Sync + 'static {
    /// Stable string identifier. `"postgres"`, `"sqlite"`, etc. Used as
    /// the matching key in `FieldSpec::supported_backends`, and shown
    /// in system-check error messages.
    fn name(&self) -> &'static str;

    /// Whether this backend supports the given feature. Used by the
    /// system check to gate Postgres-only field types (Array, HStore,
    /// jsonb) and by the migration engine to choose between
    /// `INSERT ... RETURNING` and `INSERT; last_insert_rowid()`.
    fn supports(&self, feature: BackendFeature) -> bool;

    /// Map an umbra `SqlType` to the sea-query `ColumnType` that
    /// renders the right native SQL column type on this backend. The
    /// migration engine (M5) reads this when generating `CREATE TABLE`.
    fn map_type(&self, ty: crate::orm::SqlType) -> sea_query::ColumnType;

    /// Map a full column (type + per-column hints like `max_length`)
    /// to its sea-query `ColumnType`. Default impl delegates to
    /// `map_type` — backends that want to lift hints (Postgres
    /// rendering `Text + max_length=N` as `VARCHAR(N)`, for example)
    /// override this. The migration engine prefers this over
    /// `map_type` so the per-column attributes flow into DDL.
    fn map_column(&self, col: &crate::migrate::Column) -> sea_query::ColumnType {
        self.map_type(col.ty)
    }
}

/// Backend feature flags surfaced to umbra.
///
/// New variants land alongside new backend behaviour. Each variant
/// represents one capability that umbra reasons about explicitly; the
/// system check or the migration engine asks via `supports(feature)`
/// rather than hard-coding `if backend.name() == "postgres"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendFeature {
    /// `INSERT ... RETURNING column[, ...]` on inserts. Postgres + SQLite
    /// (3.35+); MySQL doesn't have it natively.
    InsertReturning,
    /// `INSERT ... ON CONFLICT (col) DO UPDATE` upserts. Postgres + SQLite.
    UpsertOnConflict,
    /// Array column types (`text[]`, `int[]`, etc.). Postgres only.
    ArrayColumns,
    /// `HStoreField` analogue: `key => value` text maps. Postgres only.
    HStoreColumns,
    /// Native `jsonb` column with index / operator support. Postgres only;
    /// SQLite supports JSON-as-TEXT but without the operator surface, so
    /// this flag is more honest as "real jsonb" than "any JSON."
    JsonbColumns,
    /// Native full-text search (`tsvector` + `to_tsquery`). Postgres only.
    FullTextSearch,
    /// CIDR / INET / MACADDR network address column types. Postgres only.
    CidrInet,
    /// Native `UUID` column type. Postgres only; SQLite encodes UUIDs as
    /// `TEXT` instead.
    UuidNative,
    /// Native `BOOLEAN` column type. Postgres + SQLite (since 3.23); MySQL
    /// historically encodes as TINYINT.
    Boolean,
}

/// Postgres backend. **Specified, not yet wired at runtime.**
///
/// The M0–M4 pool is still `sqlx::SqlitePool`; this struct exists so
/// the system check can flag field-type incompatibilities consistently
/// today, and so the M5 migration engine can render Postgres DDL ahead
/// of the runtime wiring. Switching the live pool happens when a real
/// user lands with a Postgres workload (deferred backlog entry).
#[derive(Debug)]
pub struct PostgresBackend;

/// SQLite backend. The umbra runtime default through M3.
#[derive(Debug)]
pub struct SqliteBackend;

// =========================================================================
// Trait impls — methods filled in by the M4 fan-out subagent A.
// =========================================================================

impl DatabaseBackend for PostgresBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    /// Postgres feature catalogue. Source of truth: spec
    /// `docs/specs/05-backends-and-system-check.md` §7.1. Postgres carries
    /// every `BackendFeature` umbra reasons about today; `HStoreColumns`
    /// is reported true and the HSTORE extension stays a DBA concern.
    fn supports(&self, feature: BackendFeature) -> bool {
        match feature {
            BackendFeature::InsertReturning
            | BackendFeature::UpsertOnConflict
            | BackendFeature::ArrayColumns
            | BackendFeature::HStoreColumns
            | BackendFeature::JsonbColumns
            | BackendFeature::FullTextSearch
            | BackendFeature::CidrInet
            | BackendFeature::UuidNative
            | BackendFeature::Boolean => true,
        }
    }

    /// Postgres lifts `Text + max_length = N` to `VARCHAR(N)` so the
    /// length cap is enforced at the database level. `Text` without
    /// `max_length` stays `TEXT` (unbounded). SQLite ignores the
    /// length entirely — `VARCHAR(N)` and `TEXT` carry the same
    /// affinity there — so its `map_column` keeps the default impl.
    fn map_column(&self, col: &crate::migrate::Column) -> sea_query::ColumnType {
        use crate::orm::SqlType;
        use sea_query::ColumnType;
        if matches!(col.ty, SqlType::Text) && col.max_length > 0 {
            return ColumnType::String(sea_query::StringLen::N(col.max_length));
        }
        self.map_type(col.ty)
    }

    /// Postgres `SqlType` -> `sea_query::ColumnType` mapping. Source of
    /// truth: spec `05-backends-and-system-check.md` §7.1.
    fn map_type(&self, ty: crate::orm::SqlType) -> sea_query::ColumnType {
        use crate::orm::SqlType;
        use sea_query::ColumnType;
        match ty {
            SqlType::SmallInt => ColumnType::SmallInteger,
            SqlType::Integer => ColumnType::Integer,
            SqlType::BigInt => ColumnType::BigInteger,
            SqlType::Real => ColumnType::Float,
            SqlType::Double => ColumnType::Double,
            SqlType::Boolean => ColumnType::Boolean,
            SqlType::Text => ColumnType::Text,
            SqlType::Date => ColumnType::Date,
            SqlType::Time => ColumnType::Time,
            SqlType::Timestamptz => ColumnType::TimestampWithTimeZone,
            SqlType::Uuid => ColumnType::Uuid,
            // Postgres has both `json` and `jsonb`; we always pick `jsonb`
            // because that's the variant with index support and the
            // operator surface (`@>`, `->`, `->>`). The performance gap
            // vs `json` is meaningful for any real workload; the storage
            // overhead is negligible.
            SqlType::Json => ColumnType::JsonBinary,
            // Postgres array. The inner type round-trips through this
            // same map_type recursively (lifting ArrayElement to its
            // SqlType equivalent), which keeps the per-element rendering
            // in one place and lets future SqlType variants pick up
            // array support automatically once they're added to
            // ArrayElement.
            SqlType::Array(elem) => {
                ColumnType::Array(std::sync::Arc::new(self.map_type(elem.to_sql_type())))
            }
            SqlType::Inet => ColumnType::Inet,
            SqlType::Cidr => ColumnType::Cidr,
            SqlType::MacAddr => ColumnType::MacAddr,
            // sea-query has no built-in `tsvector` variant — go through
            // ColumnType::Custom to render it. Populate via Postgres
            // trigger or GENERATED clause; umbra's migration engine
            // emits the bare column declaration.
            SqlType::FullText => ColumnType::custom("tsvector"),
            // ForeignKey is stored as BIGINT in the DB; the REFERENCES
            // clause is appended separately by the migration engine's
            // `build_column_def_*` helpers (sea-query doesn't have a
            // first-class FK DDL API at our version).
            SqlType::ForeignKey => ColumnType::BigInteger,
        }
    }
}

impl DatabaseBackend for SqliteBackend {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    /// SQLite feature catalogue. Source of truth: spec
    /// `docs/specs/05-backends-and-system-check.md` §7.1. SQLite carries
    /// the modern transactional features (RETURNING since 3.35, ON
    /// CONFLICT since 3.24) and native `BOOLEAN`, but no array / hstore /
    /// jsonb / full-text / network / native-UUID surface. UUIDs go
    /// through `TEXT` instead; see `map_type` below.
    fn supports(&self, feature: BackendFeature) -> bool {
        match feature {
            BackendFeature::InsertReturning
            | BackendFeature::UpsertOnConflict
            | BackendFeature::Boolean => true,
            BackendFeature::ArrayColumns
            | BackendFeature::HStoreColumns
            | BackendFeature::JsonbColumns
            | BackendFeature::FullTextSearch
            | BackendFeature::CidrInet
            | BackendFeature::UuidNative => false,
        }
    }

    /// SQLite `SqlType` -> `sea_query::ColumnType` mapping. Source of
    /// truth: spec `05-backends-and-system-check.md` §7.1. `Uuid` lands
    /// on `Text` because SQLite has no native UUID type, which is the
    /// reason `supports(UuidNative)` reports false above.
    fn map_type(&self, ty: crate::orm::SqlType) -> sea_query::ColumnType {
        use crate::orm::SqlType;
        use sea_query::ColumnType;
        match ty {
            SqlType::SmallInt => ColumnType::SmallInteger,
            SqlType::Integer => ColumnType::Integer,
            SqlType::BigInt => ColumnType::BigInteger,
            SqlType::Real => ColumnType::Float,
            SqlType::Double => ColumnType::Double,
            SqlType::Boolean => ColumnType::Boolean,
            SqlType::Text => ColumnType::Text,
            SqlType::Date => ColumnType::Date,
            SqlType::Time => ColumnType::Time,
            SqlType::Timestamptz => ColumnType::TimestampWithTimeZone,
            SqlType::Uuid => ColumnType::Text,
            // ForeignKey stored as BIGINT; the REFERENCES clause is
            // appended by the migration engine separately.
            SqlType::ForeignKey => ColumnType::BigInteger,
            // SQLite has no native JSON column type — the JSON1 extension
            // operates on TEXT values. Storing the document as TEXT keeps
            // the round-trip portable through sqlx's `json` feature (which
            // serializes `serde_json::Value` to a JSON string and decodes
            // back). Future work: add a JSON1 system check so JSON
            // operators on SQLite fail at boot when the extension isn't
            // compiled in (rare but possible on bare-builds).
            SqlType::Json => ColumnType::Text,
            // Postgres-only. The M4 `field.backend` system check fires
            // at boot when an Array field is registered against SQLite,
            // so reaching this arm at runtime means the boot path was
            // bypassed (low-level test seeding, hand-rolled
            // backend::init, etc.). Panic with a clear pointer rather
            // than rendering a SQL fragment SQLite can't parse.
            SqlType::Array(_) => panic!(
                "umbra::backend::SqliteBackend::map_type: SqlType::Array is Postgres-only. \
                 The field.backend system check should have failed boot; if you reached this \
                 panic, either the model registry wasn't initialised before map_type ran or \
                 the check was disabled. For portable list storage, use SqlType::Json instead."
            ),
            // Postgres-only network address types. field.backend gates
            // these at boot; reaching the SQLite map_type means the
            // boot path was bypassed.
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => panic!(
                "umbra::backend::SqliteBackend::map_type: SqlType::Inet/Cidr/MacAddr are \
                 Postgres-only. The field.backend system check should have failed boot."
            ),
            SqlType::FullText => panic!(
                "umbra::backend::SqliteBackend::map_type: SqlType::FullText is Postgres-only. \
                 The field.backend system check should have failed boot."
            ),
        }
    }
}

// =========================================================================
// Ambient registration. The active backend is published into a process-
// wide `OnceLock` by `AppBuilder::build()`, alongside the pool and the
// settings. Mirrors the pattern from `crate::db` and `crate::settings`.
// =========================================================================

static ACTIVE: OnceLock<&'static dyn DatabaseBackend> = OnceLock::new();

/// Initialize the ambient backend. Called by `AppBuilder::build()` only.
pub(crate) fn init(backend: &'static dyn DatabaseBackend) {
    ACTIVE
        .set(backend)
        .expect("umbra::backend::init called more than once");
}

/// Return the active backend.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn active() -> &'static dyn DatabaseBackend {
    *ACTIVE
        .get()
        .expect("umbra: backend not initialised — did you call App::build()?")
}

/// Detect the right backend for the given database URL by scheme.
///
/// Used by `AppBuilder::build()` to publish the ambient backend before
/// the system check runs. URLs that name an unshipped backend (mysql,
/// oracle) fail at boot with a clear error rather than continuing into
/// the system check phase.
pub fn detect(url: &str) -> Result<&'static dyn DatabaseBackend, BackendDetectError> {
    let scheme = url
        .split("://")
        .next()
        .and_then(|s| s.split(':').next())
        .unwrap_or(url);
    match scheme {
        "sqlite" => Ok(&SqliteBackend),
        "postgres" | "postgresql" => Ok(&PostgresBackend),
        other => Err(BackendDetectError::Unsupported(other.to_owned())),
    }
}

/// Error returned by `detect` when the URL scheme names an unshipped
/// backend.
#[derive(Debug)]
pub enum BackendDetectError {
    /// The URL scheme is one umbra hasn't implemented yet (mysql, oracle).
    Unsupported(String),
}

impl std::fmt::Display for BackendDetectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendDetectError::Unsupported(scheme) => write!(
                f,
                "umbra: no backend shipped for URL scheme `{scheme}://`. \
                 M4 supports `sqlite://` and `postgres://`. \
                 MySQL, Oracle, and other backends are in the deferred backlog."
            ),
        }
    }
}

impl std::error::Error for BackendDetectError {}
