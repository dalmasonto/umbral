//! The `TsVector` newtype — umbral's Rust binding for Postgres
//! `tsvector`.
//!
//! Postgres tsvector is the lexeme-vector representation used by the
//! full-text search subsystem. It's typically read out of the
//! database (via `to_tsvector(...)`) and queried via the `@@`
//! operator against a `tsquery`. Users rarely construct one
//! by hand — the column is usually populated by a Postgres trigger
//! or a `GENERATED ALWAYS AS (to_tsvector(...)) STORED` clause.
//!
//! sqlx doesn't ship a native binding, so umbral defines a thin
//! newtype around `String` with manual `Type`/`Encode`/`Decode`
//! impls for the Postgres driver. The String holds the on-the-wire
//! text representation, which is what Postgres returns when a
//! `tsvector` is selected without an explicit cast.

use sqlx::Postgres;
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef};
use sqlx::{Decode, Encode, Type};

/// Postgres `tsvector` column value.
///
/// The inner string carries the lexeme-vector's text representation,
/// the format Postgres uses for `tsvector::text` cast output —
/// space-separated lexemes with optional positions (`'word':1
/// 'phrase':2,3A`).
///
/// Most user code reads this from the database and queries with
/// `FullTextCol::matches(...)`; constructing a TsVector by hand is
/// unusual but supported via the `From<String>` impl.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TsVector(pub String);

impl TsVector {
    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for TsVector {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for TsVector {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for TsVector {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// =========================================================================
// sqlx bindings (Postgres only).
//
// TsVector decodes from the wire as text (the `tsvector::text` cast
// output Postgres returns when the column is selected). Encoding
// writes the same text — Postgres accepts implicit `text -> tsvector`
// cast on INSERT/UPDATE so a bare-text bind works for typical paths.
// Users with strict tsvector input semantics (e.g., must call
// `to_tsvector('english', $1)` to apply a specific config) wrap
// the value in a custom INSERT expression rather than binding through
// the QuerySet.
//
// SQLite gets no impl — `TsVector` only makes sense on Postgres, and
// the `field.backend` system check blocks models with FullText fields
// from booting against SQLite.
// =========================================================================

impl Type<Postgres> for TsVector {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("tsvector")
    }
}

impl<'r> Decode<'r, Postgres> for TsVector {
    fn decode(value: PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        // The PG wire format for tsvector is the same text form Postgres
        // returns for the `::text` cast. sqlx exposes it via the value's
        // text view; fall back to the string decoder.
        let s = <String as Decode<Postgres>>::decode(value)?;
        Ok(TsVector(s))
    }
}

impl Encode<'_, Postgres> for TsVector {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as Encode<Postgres>>::encode_by_ref(&self.0, buf)
    }
}
