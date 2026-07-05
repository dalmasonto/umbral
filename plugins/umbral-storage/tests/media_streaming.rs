//! Behavioral tests for streaming Storage (gaps2 #82). Moved from
//! umbral-media.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::storage::{ByteStream, Storage, StorageError, StoredFile};
use umbral_storage::{FsStorage, MediaFile, StoragePlugin};

use futures_util::StreamExt;

fn stream_of(chunks: Vec<Vec<u8>>) -> ByteStream {
    let items = chunks
        .into_iter()
        .map(|c| Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(c)));
    Box::pin(futures_util::stream::iter(items))
}

async fn collect(mut s: ByteStream) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    while let Some(chunk) = s.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

#[tokio::test]
async fn store_stream_round_trips_multi_chunk_body() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let chunks = vec![
        b"the quick ".to_vec(),
        b"brown fox ".to_vec(),
        b"jumps over".to_vec(),
    ];
    let whole: Vec<u8> = chunks.concat();

    let stored = fs
        .store_stream("note.txt", "text/plain", stream_of(chunks))
        .await
        .expect("store_stream should succeed");

    assert_eq!(stored.size, whole.len() as u64);
    assert!(stored.key.ends_with("-note.txt"));
    assert_eq!(stored.url, fs.url(&stored.key));

    let on_disk = std::fs::read(dir.path().join(&stored.key)).unwrap();
    assert_eq!(on_disk, whole);

    let back = fs
        .retrieve_stream(&stored.key)
        .await
        .expect("retrieve_stream");
    assert_eq!(collect(back).await.unwrap(), whole);
}

#[tokio::test]
async fn retrieve_stream_missing_key_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());
    match fs.retrieve_stream("does-not-exist").await {
        Err(StorageError::NotFound) => {}
        Err(other) => panic!("expected NotFound, got {other:?}"),
        Ok(_) => panic!("expected NotFound, got an Ok stream"),
    }
}

/// THE SECURITY TEST. A size-limited backend wrapping `FsStorage` rejects a
/// chunked body whose cumulative bytes exceed the cap MID-STREAM.
#[tokio::test]
async fn size_limited_store_stream_rejects_oversize_mid_stream() {
    let dir = tempfile::tempdir().unwrap();
    let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", dir.path()));

    let max: u64 = 10;
    let limited = StoragePlugin::size_limited_for_test(fs.clone(), max);

    let chunks = vec![vec![b'a'; 10], vec![b'b'; 10], vec![b'c'; 10]];
    let result = limited
        .store_stream("big.bin", "application/octet-stream", stream_of(chunks))
        .await;

    match result {
        Err(StorageError::TooLarge { limit, actual }) => {
            assert_eq!(limit, max, "the cap reported must be the configured max");
            assert!(actual > max, "actual must exceed the cap");
        }
        other => panic!("expected TooLarge from mid-stream cap, got {other:?}"),
    }

    let mut max_file_len: u64 = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            max_file_len = max_file_len.max(entry.metadata().unwrap().len());
        }
    }
    assert!(
        max_file_len <= max,
        "a rejected oversize upload must not leave a file larger than the cap \
         on disk; found a {max_file_len}-byte file (cap {max})"
    );
}

#[tokio::test]
async fn size_limited_store_stream_allows_under_cap() {
    let dir = tempfile::tempdir().unwrap();
    let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", dir.path()));
    let limited = StoragePlugin::size_limited_for_test(fs.clone(), 100);

    let chunks = vec![b"under ".to_vec(), b"the cap".to_vec()];
    let whole: Vec<u8> = chunks.concat();
    let stored = limited
        .store_stream("ok.txt", "text/plain", stream_of(chunks))
        .await
        .expect("a body under the cap must store");
    assert_eq!(stored.size, whole.len() as u64);
    assert_eq!(fs.retrieve(&stored.key).await.unwrap(), whole);
}

#[tokio::test]
async fn store_stream_neutralises_active_content() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let stored = fs
        .store_stream(
            "evil.html",
            "text/html",
            stream_of(vec![b"<script>".to_vec()]),
        )
        .await
        .expect("store_stream");

    assert!(
        stored.key.ends_with(".txt"),
        "an active-content upload must be stored with a `.txt` suffix; key = {}",
        stored.key
    );
    assert!(!stored.key.ends_with(".html"));
}

#[tokio::test]
async fn store_stream_sanitises_path_separators() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let stored = fs
        .store_stream(
            "../../etc/passwd",
            "text/plain",
            stream_of(vec![b"x".to_vec()]),
        )
        .await
        .expect("store_stream");

    assert!(
        !stored.key.contains('/'),
        "key must not contain a separator: {}",
        stored.key
    );
    assert!(dir.path().join(&stored.key).exists());
}

// ---- A non-overriding Storage exercises the buffering DEFAULT impls. ----

struct BufferOnlyStorage {
    map: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

#[umbral::storage::async_trait]
impl Storage for BufferOnlyStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let key = format!("k-{filename}");
        self.map.lock().unwrap().insert(key.clone(), bytes.to_vec());
        Ok(StoredFile {
            url: format!("/buf/{key}"),
            key,
            size: bytes.len() as u64,
        })
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.map
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.map.lock().unwrap().remove(key);
        Ok(())
    }

    fn url(&self, key: &str) -> String {
        format!("/buf/{key}")
    }
}

#[tokio::test]
async fn default_streaming_impls_buffer_and_round_trip() {
    let store = BufferOnlyStorage {
        map: std::sync::Mutex::new(std::collections::HashMap::new()),
    };

    let chunks = vec![b"hello ".to_vec(), b"default ".to_vec(), b"stream".to_vec()];
    let whole: Vec<u8> = chunks.concat();

    let stored = store
        .store_stream("doc.bin", "application/octet-stream", stream_of(chunks))
        .await
        .expect("default store_stream");
    assert_eq!(stored.size, whole.len() as u64);

    let back = store
        .retrieve_stream(&stored.key)
        .await
        .expect("default retrieve_stream");
    assert_eq!(collect(back).await.unwrap(), whole);
}

#[tokio::test]
async fn default_store_stream_propagates_stream_error() {
    let store = BufferOnlyStorage {
        map: std::sync::Mutex::new(std::collections::HashMap::new()),
    };

    let err_stream: ByteStream = Box::pin(futures_util::stream::iter(vec![
        Ok(bytes::Bytes::from_static(b"ok")),
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "boom")),
    ]));

    match store.store_stream("x", "text/plain", err_stream).await {
        Err(StorageError::Io(_)) => {}
        other => panic!("expected Io error to propagate, got {other:?}"),
    }
}

// ---- StoragePlugin::save_stream end-to-end against a real pool. ----

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("media_stream.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
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
    })
    .await;
}

#[tokio::test]
async fn save_stream_records_accurate_size() {
    boot().await;
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    let plugin = StoragePlugin::new().media_with_storage("/media", fs.clone());

    let chunks = vec![vec![b'z'; 10], vec![b'z'; 8], vec![b'z'; 7]];
    let total = 25u64;

    let outcome = plugin
        .save_stream("upload.bin", "application/octet-stream", stream_of(chunks))
        .await
        .expect("save_stream should succeed");

    assert_eq!(
        outcome.file.size, total as i64,
        "MediaFile.size must equal the real streamed byte count"
    );
    assert!(
        outcome.file.id > 0,
        "saved row must have a real primary key"
    );

    let rows_for_key = MediaFile::objects()
        .filter(umbral_storage::media_file::KEY.eq(&outcome.file.key))
        .count()
        .await
        .expect("count by key");
    assert_eq!(rows_for_key, 1, "save_stream must insert exactly one row");

    let back = fs.retrieve(&outcome.file.key).await.unwrap();
    assert_eq!(back.len(), total as usize);
    assert!(back.iter().all(|&b| b == b'z'));
}

#[tokio::test]
async fn save_stream_enforces_max_size_mid_stream() {
    boot().await;
    let media_dir = tempfile::tempdir().expect("media dir");
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    let plugin = StoragePlugin::new()
        .media_with_storage("/media", fs.clone())
        .max_size(10);

    let chunks = vec![vec![b'a'; 10], vec![b'b'; 10], vec![b'c'; 10]];
    let result = plugin
        .save_stream("toobig.bin", "application/octet-stream", stream_of(chunks))
        .await;

    match result {
        Err(umbral_storage::MediaError::TooLarge { limit, actual }) => {
            assert_eq!(limit, 10);
            assert!(actual > 10);
        }
        other => panic!("expected MediaError::TooLarge, got {other:?}"),
    }

    let mut max_file_len = 0u64;
    for entry in std::fs::read_dir(media_dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            max_file_len = max_file_len.max(entry.metadata().unwrap().len());
        }
    }
    assert!(
        max_file_len <= 10,
        "rejected oversize upload left a {max_file_len}-byte file"
    );
}
