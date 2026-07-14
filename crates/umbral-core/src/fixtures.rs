//! Feature #74 — per-model fixture load / dump.
//!
//! The `backup` module ships the whole-database dump format
//! (envelope with `umbral_dump_version` and every registered
//! model's rows). That's the right shape for migrations between
//! environments, but it's heavy for the common test-and-dev case:
//! "I want to seed five `Post` rows from a file at the top of my
//! test."
//!
//! Fixtures are the simpler shape: a flat JSON array of row
//! objects, per file, per model. The file is hand-editable,
//! diff-friendly, and skips the envelope so a fresh dev seed
//! file can be checked into source control without metadata
//! noise.
//!
//! # Example
//!
//! ```ignore
//! // tests/fixtures/posts.json:
//! //   [
//! //     {"id": 1, "title": "Hello", "body": "..."},
//! //     {"id": 2, "title": "World", "body": "..."}
//! //   ]
//!
//! use umbral::orm::Model;
//!
//! #[tokio::test]
//! async fn seeded_posts_are_listable() {
//!     boot().await;
//!     let inserted = Post::objects()
//!         .load_fixture("tests/fixtures/posts.json")
//!         .await
//!         .expect("seed");
//!     assert_eq!(inserted, 2);
//!     let visible = Post::objects().fetch().await.unwrap();
//!     assert_eq!(visible.len(), 2);
//! }
//! ```
//!
//! Row objects flow through the same `DynQuerySet::insert_json`
//! path the REST plugin uses, so every framework feature applies
//! transparently: auto_now / auto_now_add timestamps, slug_from
//! auto-derive, validator pre-checks, FK existence checks, and
//! the soft-delete WHERE auto-filter on later reads.
//!
//! ## Deferred
//!
//! - `cargo run -- seed --fixture <path>` CLI subcommand — needs
//!   per-model table-name resolution from a string, which the
//!   typed Manager-based shape doesn't expose. Lands when a
//!   real consumer surfaces the need (`Plugin::commands()` from
//!   feature #71 makes the wiring trivial when that day arrives).
//! - `Factory` macros + the `fake` crate for generating
//!   realistic data on demand. Concrete data via JSON file is
//!   the v1.
//! - Transaction-scoped fixture lifecycle (auto-rollback after
//!   each test). Today the caller manages the test transaction
//!   themselves; layering an explicit wrapper is straightforward
//!   once `umbral-testing` grows a `TestClient`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::migrate::ModelMeta;
use crate::orm::{DynError, DynQuerySet, Manager, Model};

/// Failures the fixture pipeline can surface. Splits I/O,
/// JSON-shape, and write-time issues so callers can branch on the
/// failure kind without parsing error messages.
#[derive(Debug)]
pub enum FixtureError {
    /// `std::fs::read` or `std::fs::write` failed.
    Io(std::io::Error),
    /// `serde_json::from_slice` / `serde_json::to_string` failed.
    /// Most often this means the fixture file isn't a JSON array
    /// of objects.
    Json(serde_json::Error),
    /// The JSON top level wasn't a JSON array. The fixture file
    /// shape is intentionally strict — wrap-in-envelope formats
    /// belong in [`crate::backup`].
    NotAnArray { path: PathBuf },
    /// A row's insertion failed. Wraps the underlying
    /// [`crate::orm::write::WriteError`] so callers can pattern-
    /// match on validator / FK / unique violations and surface a
    /// readable error per fixture row.
    Write {
        index: usize,
        source: crate::orm::write::WriteError,
    },
    /// A read-back (during `dump_fixture`) failed.
    Read(DynError),
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "fixture I/O error: {e}"),
            Self::Json(e) => write!(f, "fixture JSON error: {e}"),
            Self::NotAnArray { path } => write!(
                f,
                "fixture {} is not a JSON array of row objects",
                path.display()
            ),
            Self::Write { index, source } => {
                write!(f, "fixture row #{index} insert failed: {source:?}")
            }
            Self::Read(e) => write!(f, "fixture read-back failed: {e:?}"),
        }
    }
}

impl std::error::Error for FixtureError {}

impl From<std::io::Error> for FixtureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for FixtureError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Load a JSON-array fixture file into the given model's table.
///
/// Returns the number of rows successfully inserted. The first
/// failure stops the run — subsequent rows aren't attempted. Wrap
/// the call in [`crate::transaction`] if you want all-or-nothing
/// semantics across the file.
pub async fn load_fixture<T, P>(path: P) -> Result<usize, FixtureError>
where
    T: Model,
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let parsed: Value = serde_json::from_slice(&bytes)?;
    let rows = match parsed {
        Value::Array(rows) => rows,
        _ => {
            return Err(FixtureError::NotAnArray {
                path: path.to_path_buf(),
            });
        }
    };
    let meta = ModelMeta::for_::<T>();
    for (index, raw) in rows.iter().enumerate() {
        let obj: Map<String, Value> = match raw {
            Value::Object(map) => map.clone(),
            _ => {
                return Err(FixtureError::NotAnArray {
                    path: path.to_path_buf(),
                });
            }
        };
        // gaps4 #2: a fixture written by `dumpdata` carries each `Masked<T>`
        // column's sealed ciphertext (dump reads `unredacted_for_backup`).
        // `.presealed()` binds it verbatim instead of sealing it again — which
        // would double-encrypt and make the plaintext unrecoverable on restore.
        DynQuerySet::for_meta(&meta)
            .presealed()
            .insert_json(&obj)
            .await
            .map_err(|source| FixtureError::Write { index, source })?;
    }
    Ok(rows.len())
}

/// Read every row of the given model out as a flat JSON array
/// and write it to `path`. Symmetric counterpart to
/// [`load_fixture`].
///
/// The output is `serde_json::to_string_pretty`-formatted so the
/// file is diff-friendly and hand-editable. Use this to capture
/// a working dev dataset that can be checked into source control
/// and replayed in tests.
pub async fn dump_fixture<T, P>(path: P) -> Result<usize, FixtureError>
where
    T: Model,
    P: AsRef<Path>,
{
    let meta = ModelMeta::for_::<T>();
    // A dump is not an API response. `private` and `secret` columns are stripped from every
    // serialized read by default — correct for a client, catastrophic for a backup: a fixture
    // without `password_hash` restores a database in which nobody can log in, and one without
    // `Masked<T>` ciphertext restores empty encrypted columns. So this reads everything, via
    // the one loudly-named escape that exists for exactly this.
    //
    // Which also means: a dump file holds secrets. Treat it like one.
    let rows = DynQuerySet::for_meta(&meta)
        .unredacted_for_backup()
        .fetch_as_json()
        .await
        .map_err(FixtureError::Read)?;
    let json = serde_json::to_string_pretty(&rows)?;
    std::fs::write(path.as_ref(), json)?;
    Ok(rows.len())
}

/// Manager convenience shims so callers can write
/// `Post::objects().load_fixture(...)` and `dump_fixture(...)`
/// without importing the free functions.
impl<T: Model> Manager<T> {
    /// See [`load_fixture`].
    pub async fn load_fixture<P: AsRef<Path>>(&self, path: P) -> Result<usize, FixtureError> {
        load_fixture::<T, _>(path).await
    }

    /// See [`dump_fixture`].
    pub async fn dump_fixture<P: AsRef<Path>>(&self, path: P) -> Result<usize, FixtureError> {
        dump_fixture::<T, _>(path).await
    }
}
