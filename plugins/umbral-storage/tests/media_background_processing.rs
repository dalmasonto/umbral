//! Background media processing (gaps2 #57): `on_upload` processors + the
//! `MediaFile.status` lifecycle + Mode A (`save` — sync write, background
//! processing) vs Mode B (`save_deferred` — deferred write).
//!
//! The background work runs in a detached `tokio::spawn`, so the assertions
//! are made DETERMINISTIC two ways: a processor that sends on a
//! `tokio::sync::mpsc` the test awaits (proving the processor ran), and a
//! bounded poll loop on the row's `status` (proving the terminal status was
//! persisted) wrapped in `tokio::time::timeout`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use umbral::prelude::{AppContext, Plugin};

/// The processor registry is a single ambient (process-global) seam, like
/// `umbral::storage`. Tests that install processors must not run concurrently
/// or one test's processor list leaks into another's save. Serialise them
/// with this async mutex (the no-processor / deferred tests don't touch the
/// registry, so they stay parallel).
static PROCESSOR_TESTS: AsyncMutex<()> = AsyncMutex::const_new(());

/// Build an `AppContext` for driving a plugin's `on_ready` directly in a test
/// (the boot helper builds the app without this plugin, so `on_ready` — which
/// installs the ambient processor list — must be invoked by hand).
fn test_ctx() -> AppContext {
    AppContext {
        pool: umbral::db::pool_dispatched().clone(),
        settings: umbral::settings::get().clone(),
    }
}
use umbral::storage::Storage;
use umbral_storage::{
    FsStorage, MediaFile, MediaTracking, STATUS_FAILED, STATUS_PROCESSING, STATUS_READY,
    StoragePlugin,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Boot a real ambient app + sqlite tempfile pool + `media_file` table once
/// for the whole binary. Mirrors `media_plugin_save.rs::boot`, but the
/// `media_file` table carries the new `status` column with its `'ready'`
/// default (the additive-column migration shape).
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("media-bg.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE media_file (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                key TEXT NOT NULL,\
                filename TEXT NOT NULL,\
                content_type TEXT NOT NULL,\
                size INTEGER NOT NULL,\
                uploaded_at TEXT NOT NULL,\
                status TEXT NOT NULL DEFAULT 'ready'\
             )",
        )
        .execute(&pool)
        .await
        .expect("create media_file");

        let ambient_dir = tempfile::tempdir().expect("ambient media dir");
        let path = ambient_dir.path().to_path_buf();
        std::mem::forget(ambient_dir);
        let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", path));
        umbral::storage::set_storage(Arc::new(MediaTracking::new(fs)));
    })
    .await;
}

/// Poll the row's `status` until it equals `want`, bounded by `timeout`.
/// Returns the final status (which on timeout is the last-read value, so the
/// caller's assert produces a useful message).
async fn await_status(id: i64, want: &str, timeout: Duration) -> String {
    let deadline = tokio::time::timeout(timeout, async {
        loop {
            let row = MediaFile::objects()
                .filter(umbral_storage::media_file::ID.eq(id))
                .first()
                .await
                .expect("query media_file")
                .expect("media_file row must exist");
            if row.status == want {
                return row.status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    match deadline {
        Ok(status) => status,
        Err(_) => MediaFile::objects()
            .filter(umbral_storage::media_file::ID.eq(id))
            .first()
            .await
            .expect("query media_file")
            .map(|r| r.status)
            .unwrap_or_else(|| "<missing>".to_string()),
    }
}

// ── 1. Mode A, no processors → immediately "ready" ──────────────────────

#[tokio::test]
async fn save_without_processors_is_ready_immediately() {
    boot().await;
    // Serialise against the processor-installing tests + reset the ambient
    // registry: this test asserts the no-processor path, so no other test's
    // processor list may be active.
    let _guard = PROCESSOR_TESTS.lock().await;
    umbral_storage::clear_processors_for_test();
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    let plugin = StoragePlugin::new().media_with_storage("/media", fs.clone());

    let outcome = plugin
        .save("plain.txt", "text/plain", b"hello")
        .await
        .expect("save should succeed");

    assert_eq!(
        outcome.file.status, STATUS_READY,
        "no processors → save must return status=ready immediately"
    );
    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("file must be retrievable");
    assert_eq!(got, b"hello");
}

// ── 2. Mode A, with a processor → "processing" then "ready" ─────────────

#[tokio::test]
async fn save_with_processor_processes_then_ready() {
    boot().await;
    let _guard = PROCESSOR_TESTS.lock().await;
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));

    // The processor is observable: it sends the media id on an mpsc the test
    // awaits, so we KNOW the background task ran (no sleep-and-hope).
    let (tx, mut rx) = tokio::sync::mpsc::channel::<i64>(1);
    let plugin = StoragePlugin::new()
        .media_with_storage("/media", fs.clone())
        .on_upload(move |media: MediaFile| {
            let tx = tx.clone();
            async move {
                tx.send(media.id).await.ok();
                Ok::<(), std::io::Error>(())
            }
        });

    // on_upload installs processors ambiently via on_ready; the boot helper
    // builds the app without this plugin, so drive on_ready directly.
    plugin
        .on_ready(&test_ctx())
        .expect("on_ready installs processors");

    let outcome = plugin
        .save("photo.jpg", "image/jpeg", b"jpegbytes")
        .await
        .expect("save should succeed");

    assert_eq!(
        outcome.file.status, STATUS_PROCESSING,
        "with a processor registered, save must return status=processing"
    );

    // Deterministic: the processor ran (it sent on the channel).
    let processed_id = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("processor must run within 5s")
        .expect("processor sends the media id");
    assert_eq!(processed_id, outcome.file.id);

    // Deterministic: the terminal status was persisted.
    let status = await_status(outcome.file.id, STATUS_READY, Duration::from_secs(5)).await;
    assert_eq!(status, STATUS_READY, "processing must end status=ready");

    // The original is still stored.
    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("file must be retrievable");
    assert_eq!(got, b"jpegbytes");
}

// ── 3. Mode A, failing processor → "failed", original kept ──────────────

#[tokio::test]
async fn save_with_failing_processor_ends_failed_keeps_file() {
    boot().await;
    let _guard = PROCESSOR_TESTS.lock().await;
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));

    let ran = Arc::new(AtomicUsize::new(0));
    let ran2 = ran.clone();
    let plugin = StoragePlugin::new()
        .media_with_storage("/media", fs.clone())
        .on_upload(move |_media: MediaFile| {
            let ran2 = ran2.clone();
            async move {
                ran2.fetch_add(1, Ordering::SeqCst);
                Err::<(), std::io::Error>(std::io::Error::other("processor blew up"))
            }
        });
    plugin
        .on_ready(&test_ctx())
        .expect("on_ready installs processors");

    let outcome = plugin
        .save("doc.pdf", "application/pdf", b"pdfbytes")
        .await
        .expect("save itself must succeed even though the processor will fail");
    assert_eq!(outcome.file.status, STATUS_PROCESSING);

    let status = await_status(outcome.file.id, STATUS_FAILED, Duration::from_secs(5)).await;
    assert_eq!(
        status, STATUS_FAILED,
        "a failing processor must end status=failed"
    );
    assert!(ran.load(Ordering::SeqCst) >= 1, "processor must have run");

    // Processing failure does NOT lose the upload — the original is stored.
    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("original upload must still be stored after a processing failure");
    assert_eq!(got, b"pdfbytes");
}

// ── 4. Mode B, save_deferred → "processing", file absent, then stored ───

#[tokio::test]
async fn save_deferred_writes_in_background() {
    boot().await;
    // The deferred WRITE is the background work here; reset the registry so a
    // stray processor from another test doesn't fail the deferred put.
    let _guard = PROCESSOR_TESTS.lock().await;
    umbral_storage::clear_processors_for_test();
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    // No on_ready/processors needed: the deferred WRITE is the background work.
    let plugin = StoragePlugin::new().media_with_storage("/media", fs.clone());

    let bytes = b"deferred upload payload".to_vec();
    let outcome = plugin
        .save_deferred("late.bin", "application/octet-stream", bytes.clone())
        .await
        .expect("save_deferred should succeed");

    assert_eq!(
        outcome.file.status, STATUS_PROCESSING,
        "save_deferred must return status=processing"
    );
    // The bytes are NOT written yet → the key is absent right after return.
    assert!(
        !fs.exists(&outcome.file.key).await.expect("exists check"),
        "deferred file must NOT be in storage immediately after save_deferred returns"
    );

    // After the background write, the row is ready and the file IS stored.
    let status = await_status(outcome.file.id, STATUS_READY, Duration::from_secs(5)).await;
    assert_eq!(status, STATUS_READY, "deferred write must end status=ready");

    assert!(
        fs.exists(&outcome.file.key).await.expect("exists check"),
        "deferred file must be in storage after the background write"
    );
    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("deferred file must be retrievable after the background write");
    assert_eq!(got, bytes, "the returned URL/key now resolves to the bytes");
}
