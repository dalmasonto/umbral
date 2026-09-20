use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use http::HeaderMap;
use umbral_storage::{Decision, MediaCaller, StoragePlugin};

#[tokio::test]
async fn cache_first_runs_closure_once_per_caller_key() {
    // install a real memory cache as the ambient tagged cache (set-once per test binary)
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_cached(move |caller: MediaCaller, _key: &str| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Decision::of(caller.user_id().is_some()).depends_on(["t:x".to_string()])
            }
        });
    let access = plugin.resolve_access().expect("access fn");
    let h = HeaderMap::new();
    let _ = access(&h, "invoices/1.pdf").await; // miss → closure runs
    let _ = access(&h, "invoices/1.pdf").await; // hit → closure does NOT run
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second identical access must hit cache"
    );
}
