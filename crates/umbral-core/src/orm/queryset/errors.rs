//! Error types surfaced by QuerySet terminals.
//!
//! These two enums are public surface (re-exported from
//! `crate::orm::queryset` and through the facade), so each carries a
//! full `Display` + `Error` impl plus the appropriate
//! `From<sqlx::Error>`.

/// Error type for [`super::QuerySet::get`] / [`super::Manager::get`]
/// (the exactly-one shape).
///
/// `.get()` deliberately returns this rather than `Result<Option<T>,
/// sqlx::Error>` because three outcomes need three branches:
///
/// - `Ok(row)` — exactly one matched.
/// - `Err(NotFound)` — zero matched. The common 404 path.
/// - `Err(MultipleObjectsReturned)` — more than one matched. A
///   data-integrity signal: filters that should pin a unique row
///   (PK lookup, UNIQUE-constrained column) hitting this variant
///   means an invariant has already broken upstream.
/// - `Err(Sqlx)` — the DB itself returned an error.
#[derive(Debug)]
pub enum GetError {
    NotFound,
    MultipleObjectsReturned,
    Sqlx(sqlx::Error),
}

impl std::fmt::Display for GetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no matching row"),
            Self::MultipleObjectsReturned => {
                write!(f, "expected exactly one row, found more")
            }
            Self::Sqlx(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlx(e) => Some(e),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for GetError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

/// Feature 29 — composite error returned by
/// [`super::QuerySet::try_for_each`]. The chunked streaming terminal
/// can fail in two ways and the call site usually wants to
/// distinguish: a SQL fetch failure is a system-level problem (DB
/// went away, schema mismatch, etc.), while a callback error is
/// whatever domain-specific failure the user's body produced (file
/// write blew up, validation rejected the row, etc.).
#[derive(Debug)]
pub enum TryForEachError<E> {
    /// A database fetch returned an error mid-iteration. The
    /// callback never saw this row.
    Sqlx(sqlx::Error),
    /// The user's callback returned an error for some row. The
    /// walk stopped immediately; rows after the failing one were
    /// not fetched.
    Callback(E),
}

impl<E: std::fmt::Display> std::fmt::Display for TryForEachError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlx(e) => write!(f, "{e}"),
            Self::Callback(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for TryForEachError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlx(e) => Some(e),
            Self::Callback(_) => None,
        }
    }
}
