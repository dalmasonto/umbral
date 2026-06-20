//! Verify that accessing the ambient storage backend when no backend has been
//! registered returns a clean, typed error and does NOT panic.
//!
//! ## Process isolation
//!
//! The ambient storage registry is a process-global `OnceLock` that can only
//! be set once. This file compiles as its own integration-test binary (Cargo
//! rule: every file in `tests/` gets its own binary), so the lock starts
//! unset here regardless of what `plugin_save.rs` or `storage_ambient.rs`
//! do in their own processes.
//!
//! ## What is tested
//!
//! - `umbra::storage::try_storage()` returns `StorageError::NoBackend` when
//!   the lock is unset — clean error, no panic.
//! - `umbra::storage::storage_opt()` returns `None` in the same state.
//! - `FsStorage` and `MediaTracking` are safe to construct without a
//!   registered ambient backend: neither calls `storage()` at construction
//!   time, so building them does not panic.

use std::sync::Arc;

use umbra::storage::{StorageError, storage_opt};
use umbra_media::{FsStorage, MediaTracking};

/// `try_storage()` must return `Err(StorageError::NoBackend)` when no
/// backend has been registered. This is the clean-error contract that
/// replaces the per-request panic that would occur if callers used the
/// panicking `storage()` accessor instead.
#[test]
fn try_storage_returns_no_backend_error_when_unset() {
    // Fresh process — the OnceLock has never been written.
    let result = umbra::storage::try_storage();
    match result {
        Err(StorageError::NoBackend) => {} // the correct, non-panicking path
        Ok(_) => panic!("expected NoBackend but storage() returned a backend"),
        Err(other) => panic!("expected NoBackend but got: {other}"),
    }
}

/// `storage_opt()` must return `None` when no backend is registered.
#[test]
fn storage_opt_returns_none_when_unset() {
    assert!(
        storage_opt().is_none(),
        "storage_opt() must be None before any set_storage call"
    );
}

/// The `NoBackend` error must format to a human-readable string that
/// tells the operator exactly how to fix the misconfiguration.
#[test]
fn no_backend_error_formats_clearly() {
    let msg = StorageError::NoBackend.to_string();
    // Must contain something actionable — either the accessor name or the plugin.
    assert!(
        msg.contains("backend") || msg.contains("MediaPlugin") || msg.contains("set_storage"),
        "NoBackend message should be actionable, got: {msg}"
    );
}

/// `FsStorage::new` does NOT touch the ambient OnceLock — it is safe to
/// construct even when no backend is registered. Constructing it must not
/// panic.
#[test]
fn fs_storage_constructs_without_ambient_backend() {
    // If this panics, some constructor call-path reached `storage()` unexpectedly.
    let dir = tempfile::tempdir().expect("tempdir");
    let _fs = FsStorage::new("/media", dir.path());
    // No assertion needed; reaching here without panic is the test.
}

/// `MediaTracking::new` wraps an existing backend; it does not call the
/// ambient `storage()`. Constructing it must not panic.
#[test]
fn media_tracking_constructs_without_ambient_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inner: Arc<dyn umbra::storage::Storage> = Arc::new(FsStorage::new("/media", dir.path()));
    let _tracking = MediaTracking::new(inner);
    // Reaching here without panic is the test.
}
