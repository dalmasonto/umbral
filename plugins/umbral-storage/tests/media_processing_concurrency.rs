//! audit_2 plugin-storage-tasks #4 — media processing runs behind a bounded
//! concurrency gate, so an upload burst can't fan out unbounded CPU/memory-heavy
//! processing tasks. With the cap pinned low, a wave of concurrent uploads must
//! never run more than `cap` processors at once.
//!
//! Only test in its binary so `UMBRAL_MEDIA_PROCESSING_CONCURRENCY` (read once
//! into a process-wide `OnceLock`) is set before any processing starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::prelude::{AppContext, Plugin};
use umbral_storage::{FsStorage, MediaFile, StoragePlugin};

fn test_ctx() -> AppContext {
    AppContext {
        pool: umbral::db::pool_dispatched().clone(),
        settings: umbral::settings::get().clone(),
    }
}

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("media-conc.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .build()
        .expect("App::build");
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
    .execute(&umbral::db::pool())
    .await
    .expect("create media_file");
}

static CONCURRENT: AtomicUsize = AtomicUsize::new(0);
static MAX_SEEN: AtomicUsize = AtomicUsize::new(0);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn processing_concurrency_is_bounded_by_the_cap() {
    // Pin the cap low BEFORE any processing initializes the semaphore.
    // SAFETY: set at test start, only test in the binary, before any save runs.
    unsafe {
        std::env::set_var("UMBRAL_MEDIA_PROCESSING_CONCURRENCY", "2");
    }
    boot().await;

    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));

    // A processor that records the peak number of concurrent invocations and
    // sends on a channel when done. The 40 ms window guarantees overlap if the
    // gate let more than `cap` run at once.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(64);
    let plugin = StoragePlugin::new()
        .media_with_storage("/media", fs.clone())
        .on_upload(move |_media: MediaFile| {
            let tx = tx.clone();
            async move {
                let now = CONCURRENT.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_SEEN.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                CONCURRENT.fetch_sub(1, Ordering::SeqCst);
                tx.send(()).await.ok();
                Ok::<(), std::io::Error>(())
            }
        });
    plugin
        .on_ready(&test_ctx())
        .expect("on_ready installs processors");

    // Fire a wave of uploads; each spawns a detached processing task.
    const N: usize = 8;
    for i in 0..N {
        plugin
            .save(&format!("f{i}.bin"), "application/octet-stream", b"bytes")
            .await
            .expect("save");
    }

    // Wait for all N processors to finish.
    for _ in 0..N {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a processor must finish within 10s")
            .expect("channel open");
    }

    let peak = MAX_SEEN.load(Ordering::SeqCst);
    assert!(
        peak >= 1,
        "sanity: processors must have actually run; peak={peak}"
    );
    assert!(
        peak <= 2,
        "processing concurrency must be capped at 2; peak concurrent processors was {peak}"
    );
}
