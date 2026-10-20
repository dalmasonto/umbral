//! DIAGNOSTIC — heap-allocation profile of the ORM read paths, under dhat.
//!
//! Cycle B removed the allocator's own overhead (mimalloc, +20.5% capacity);
//! what remains is the *number* of allocations the framework asks for, and
//! this run is the map: dhat attributes every heap allocation to its call site
//! (total bytes AND total blocks), so the top rows ARE the framework's
//! allocation bill for these paths — the input to any allocation-count work.
//!
//! It is NOT a test of behaviour and NOT part of any measurement: it swaps the
//! global allocator (via the `dhat-alloc` feature), which is exactly the thing
//! measured binaries must not do. Run it explicitly:
//!
//! ```bash
//! cd harness/backend
//! UMBRAL_DATABASE_URL='sqlite://<abs-path>/bench/data/bench-run.db?mode=ro' \
//! UMBRAL_SECRET_KEY=alloc-profile-diagnostic-0001 \
//!   cargo test --release --features dhat-alloc --test alloc_profile \
//!   -- --ignored --nocapture
//! # writes dhat-heap.json in the crate directory
//! ```
//!
//! The database is opened READ-ONLY (`?mode=ro`): the bench dataset is live
//! state and this run only ever reads it.
#![cfg(feature = "dhat-alloc")]

#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

use umbral::prelude::*;
use umbral_auth::AuthPlugin;

use umbral_bench::domain;
use umbral_bench::models::BenchPlugin;

/// Iterations per read path. Enough that per-call noise disappears, small
/// enough that dhat's own bookkeeping (it serializes backtraces) stays fast.
const ITERS: usize = 400;

#[tokio::test]
#[ignore = "diagnostic: heap-allocation profile; run explicitly (see file header)"]
async fn alloc_profile_orm_read_paths() {
    let settings = Settings::from_env().expect("settings");
    let db = umbral::db::connect(&settings.database_url)
        .await
        .expect("connect (point UMBRAL_DATABASE_URL at bench-run.db?mode=ro)");
    let url = settings.database_url.clone();

    let mut settings = settings;
    settings.database_url = url;
    App::builder()
        .settings(settings)
        .database("default", db)
        .plugin(BenchPlugin)
        .plugin(AuthPlugin::new())
        .build()
        .expect("App::build — registers the model registry the ORM reads");

    // Warmup: let pools connect, plans cache, caches fill — the profiler
    // should see steady-state traffic, not boot.
    for _ in 0..20 {
        let _ = domain::recent_posts(25).await.expect("feed");
        let _ = domain::post_page_data(1).await.expect("post page");
        let _ = domain::profile_page_data(1).await.expect("profile page");
    }

    // One profiled batch. `Profiler::new_heap()` starts recording here and
    // writes `dhat-heap.json` when dropped at the end of the test.
    let _profiler = dhat::Profiler::new_heap();

    for _ in 0..ITERS {
        // The feed route: batched feed read (select_related prefetch) + the
        // two engagement queries.
        let posts = domain::recent_posts(25).await.expect("feed");
        let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
        let _ = domain::engage_rows(&ids).await.expect("engage");
        let _ = domain::feed_page_data(25).await.expect("feed page");

        // The post route: one row + prefetch + two counts.
        let _ = domain::post_page_data(1).await.expect("post page");

        // The profile route: five reads.
        let _ = domain::profile_page_data(1).await.expect("profile page");
    }

    eprintln!(
        "alloc_profile: {ITERS} iterations of feed(+engage,page) / post page / profile page; \
         dhat-heap.json written on drop"
    );
}
