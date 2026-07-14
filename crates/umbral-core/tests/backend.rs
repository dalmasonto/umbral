//! Coverage for the M4 `DatabaseBackend` abstraction and the URL ->
//! backend detection helper. These tests stay clear of
//! `App::builder()` so they don't touch the process-wide OnceLocks; each
//! one constructs the backend structs directly and asks them questions.
//!
//! See `crates/umbral-core/src/backend.rs` for the trait surface this file
//! exercises.

use umbral_core::backend::{
    BackendDetectError, BackendFeature, DatabaseBackend, PostgresBackend, SqliteBackend, detect,
};
use umbral_core::orm::SqlType;

/// The full set of `BackendFeature` variants. Used by the
/// `postgres_supports_every_backend_feature` test below as a static
/// reference so it can't silently drift when a new variant is added: a
/// new variant forces an update here, which is the right place for the
/// "Postgres covers everything" invariant to be re-asserted.
const ALL_FEATURES: &[BackendFeature] = &[
    BackendFeature::InsertReturning,
    BackendFeature::UpsertOnConflict,
    BackendFeature::ArrayColumns,
    BackendFeature::HStoreColumns,
    BackendFeature::JsonbColumns,
    BackendFeature::FullTextSearch,
    BackendFeature::CidrInet,
    BackendFeature::UuidNative,
    BackendFeature::Boolean,
];

#[test]
fn postgres_backend_name_is_postgres() {
    assert_eq!(PostgresBackend.name(), "postgres");
}

#[test]
fn sqlite_backend_name_is_sqlite() {
    assert_eq!(SqliteBackend.name(), "sqlite");
}

/// Postgres is the umbral superset: every `BackendFeature` variant umbral
/// reasons about today should report supported. If a new variant lands
/// that Postgres genuinely doesn't carry, the right move is to update
/// `ALL_FEATURES` and split this test, not to silently let the slice
/// fall behind.
#[test]
fn postgres_supports_every_backend_feature() {
    for feature in ALL_FEATURES {
        assert!(
            PostgresBackend.supports(*feature),
            "PostgresBackend.supports({feature:?}) should be true",
        );
    }
}

/// SQLite carries the modern transactional surface (RETURNING, ON
/// CONFLICT, native BOOLEAN) but not the Postgres-only extensions
/// (array, hstore, jsonb, native UUID). Pin each one explicitly so a
/// regression in the catalogue surfaces here rather than at the system
/// check.
#[test]
fn sqlite_supports_basic_features_but_not_postgres_only() {
    assert!(SqliteBackend.supports(BackendFeature::InsertReturning));
    assert!(SqliteBackend.supports(BackendFeature::UpsertOnConflict));
    assert!(SqliteBackend.supports(BackendFeature::Boolean));
    assert!(!SqliteBackend.supports(BackendFeature::ArrayColumns));
    assert!(!SqliteBackend.supports(BackendFeature::HStoreColumns));
    assert!(!SqliteBackend.supports(BackendFeature::JsonbColumns));
    assert!(!SqliteBackend.supports(BackendFeature::UuidNative));
}

/// Postgres has a native UUID type, so `SqlType::Uuid` should map to the
/// sea-query `Uuid` column variant rather than falling back to text.
#[test]
fn postgres_maps_uuid_to_uuid_column_type() {
    let mapped = PostgresBackend.map_type(SqlType::Uuid);
    assert!(
        matches!(mapped, sea_query::ColumnType::Uuid),
        "PostgresBackend.map_type(Uuid) should be ColumnType::Uuid, got {mapped:?}",
    );
}

/// SQLite has no native UUID type, so `SqlType::Uuid` lands on `Blob` — the column is
/// declared as what is actually stored in it (gaps3 #80).
///
/// This used to assert `Text`, and it was wrong: sqlx encodes a `Uuid` as its 16 raw bytes
/// on SQLite and its decoder reads only those bytes back (hand it the 36-char hyphenated
/// text and it fails with `ParseByteLength { len: 36 }`). The value in the column was a blob
/// however the column was declared, so calling it TEXT told the reader something false —
/// `CAST(id AS TEXT)` on a uuid PK returned mojibake.
#[test]
fn sqlite_maps_uuid_to_blob_column_type() {
    let mapped = SqliteBackend.map_type(SqlType::Uuid);
    assert!(
        matches!(mapped, sea_query::ColumnType::Blob),
        "SqliteBackend.map_type(Uuid) should be ColumnType::Blob — sqlx stores a Uuid as \
         raw bytes there, so declaring TEXT would be a lie; got {mapped:?}",
    );
}

/// `detect` should recognise both the in-memory form and the file-with-
/// query-params form as SQLite.
#[test]
fn detect_sqlite_url_returns_sqlite_backend() {
    let in_memory = detect("sqlite::memory:").expect("sqlite::memory: should detect as sqlite");
    assert_eq!(in_memory.name(), "sqlite");

    let on_disk = detect("sqlite://path/to/db.db?mode=rwc")
        .expect("sqlite:// file URL should detect as sqlite");
    assert_eq!(on_disk.name(), "sqlite");
}

/// `detect` should accept both the `postgres://` and `postgresql://`
/// schemes — sqlx and most Postgres tooling treat them as aliases, so
/// umbral has to as well.
#[test]
fn detect_postgres_url_returns_postgres_backend() {
    let short = detect("postgres://user:pw@host/db").expect("postgres:// URL should detect");
    assert_eq!(short.name(), "postgres");

    let long =
        detect("postgresql://user:pw@host/db").expect("postgresql:// URL should also detect");
    assert_eq!(long.name(), "postgres");
}

/// `detect` should fail loudly on URL schemes that name an unshipped
/// backend rather than silently returning a default. The error's
/// Display should at minimum name the offending scheme so an operator
/// can tell why their config was rejected.
#[test]
fn detect_unknown_scheme_errors_clearly() {
    let err = detect("mysql://user:pw@host/db")
        .expect_err("mysql:// should not detect; M4 only ships sqlite + postgres");
    let rendered = err.to_string();
    assert!(
        rendered.contains("mysql") || rendered.contains("scheme"),
        "error Display should name the offending scheme or use the word 'scheme', got: {rendered}",
    );

    // Pin the variant too — the API contract is `Unsupported(scheme)`,
    // not some opaque catch-all, so users (and future plugin code) can
    // match on it.
    assert!(
        matches!(err, BackendDetectError::Unsupported(_)),
        "expected BackendDetectError::Unsupported, got {err:?}",
    );
}
