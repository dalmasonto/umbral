//! Heavy-relations epic, Plan B Task 3 — multi-hop aggregate annotations.
//!
//! Every annotation compiles to ONE correlated scalar subquery (built via
//! `aggregate_path::build_aggregate_subquery`, which walks the path through
//! the SAME `walk_joins` helper Plan A's to-one resolver uses). Because each
//! annotation gets its own independent subquery — no shared JOIN, no GROUP
//! BY on the base — stacking a SUM and a COUNT over two DIFFERENT relations
//! can never inflate one against the other (the classic Django
//! multi-aggregate JOIN-multiplication bug). `annotate_count_over_m2m_path`
//! and `annotate_count_over_two_hop_reverse_fk_path` are the first
//! behavioral callers to drive `walk_joins`' `M2M` and `ReverseFk` arms
//! (Task 1 review ruling).
//!
//! Schema:
//!
//! ```text
//! User --reverse-FK--> Post --reverse-FK--> Comment   ("posts__comments", 2-hop)
//! User --M2M--------->  Category                       ("categories", 1-hop M2M)
//! Post.price: i64                                       ("posts__price" for SUM)
//! ```
//!
//! Behavioral per the project's testing rule: real rows, the actual public
//! `annotate_*`/`filter_annotation`/`order_by_annotation` API, reading
//! values back via `fetch_annotated`/`values` — never a SQL-string
//! assertion as a proxy for a round-trip.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, M2M, Op, ReverseSet};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "arp_category")]
pub struct Category {
    #[umbral(primary_key)]
    pub id: i64,
    pub label: String,
}

// The child model's TABLE name is literally "comments" so the mid-path
// segment `comments` in `"posts__comments"` resolves through
// `RelPath::from_path`'s deeper-hop auto-discovery, which matches purely on
// naming CONVENTION off the runtime registry (no `T::REVERSE_FK_RELATIONS`
// const exists for an intermediate table reached mid-path — see
// `relation.rs`'s `reverse_fk_lookup_by_table`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "comments")]
pub struct Comment {
    #[umbral(primary_key)]
    pub id: i64,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "arp_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub author: ForeignKey<User>,
    pub price: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "arp_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    // Declared at the ROOT hop — `RelPath::from_path::<User>("posts...")`
    // resolves this via `T::REVERSE_FK_RELATIONS` before ever touching the
    // registry.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "author")]
    pub posts: ReverseSet<Post>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "arp_category")]
    pub categories: M2M<Category>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

/// alice (id 1): posts [price 10 w/ 3 comments, price 40 w/ 0 comments],
/// 2 categories. bob (id 2): no posts, no categories — the zero-row proof.
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // Build the pool DIRECTLY rather than via `db::connect_sqlite`:
        // production `connect_sqlite` sets `log_statements(Off)`, which would
        // suppress the `sqlx::query` tracing events
        // `annotate_multiple_deep_paths_is_one_query_not_n_plus_1` (below)
        // counts. A real temp-file-backed SQLite db (not a shared-cache
        // `:memory:`) avoids the shared-cache-drops-when-idle flakiness a
        // `:memory:` pool is prone to under parallel test threads — the same
        // trick `tests/prefetch_by_name.rs` / `tests/query_counts.rs` use.
        let path = std::env::temp_dir().join(format!(
            "umbral_annotate_relation_path_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Category>()
            .model::<Comment>()
            .model::<Post>()
            .model::<User>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        for name in ["alice", "bob"] {
            sqlx::query("INSERT INTO arp_user (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        // alice = id 1, bob = id 2 (autoincrement, insertion order).
        for (price, author) in [(10_i64, 1_i64), (40_i64, 1_i64)] {
            sqlx::query("INSERT INTO arp_post (price, author) VALUES (?, ?)")
                .bind(price)
                .bind(author)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // Post 1 (price 10) gets 3 comments; post 2 (price 40) gets none.
        for _ in 0..3 {
            sqlx::query("INSERT INTO comments (post) VALUES (1)")
                .execute(&pool)
                .await
                .expect("seed comment");
        }
        for label in ["rust", "orm"] {
            sqlx::query("INSERT INTO arp_category (label) VALUES (?)")
                .bind(label)
                .execute(&pool)
                .await
                .expect("seed category");
        }
        // alice (user 1) linked to both categories; bob gets none.
        for child in [1_i64, 2_i64] {
            sqlx::query("INSERT INTO arp_user_categories (parent_id, child_id) VALUES (1, ?)")
                .bind(child)
                .execute(&pool)
                .await
                .expect("seed category link");
        }
    })
    .await;
}

fn by_name(
    rows: &[(User, serde_json::Map<String, serde_json::Value>)],
    name: &str,
) -> serde_json::Map<String, serde_json::Value> {
    rows.iter()
        .find(|(u, _)| u.name == name)
        .unwrap_or_else(|| panic!("no row for {name}"))
        .1
        .clone()
}

#[tokio::test]
async fn annotate_count_over_two_hop_reverse_fk_path() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let rows = User::objects()
        .annotate_count("posts__comments")
        .fetch_annotated()
        .await
        .expect("fetch_annotated");
    assert_eq!(
        by_name(&rows, "alice")["posts__comments_count"].as_i64(),
        Some(3),
        "3 comments across alice's two posts (2-hop reverse-FK path)"
    );
    assert_eq!(
        by_name(&rows, "bob")["posts__comments_count"].as_i64(),
        Some(0),
        "no posts means 0, not a missing row"
    );
}

#[tokio::test]
async fn annotate_count_over_m2m_path() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // Single-hop M2M path — proves `walk_joins`' M2M arm via the NEW
    // correlated-subquery pipeline (not the legacy junction-count shape).
    let rows = User::objects()
        .annotate_count("categories")
        .fetch_annotated()
        .await
        .expect("fetch_annotated over m2m path");
    assert_eq!(
        by_name(&rows, "alice")["categories_count"].as_i64(),
        Some(2),
        "alice is linked to both categories"
    );
    assert_eq!(
        by_name(&rows, "bob")["categories_count"].as_i64(),
        Some(0),
        "bob has no category links"
    );
}

#[tokio::test]
async fn annotate_sum_over_path_and_no_cross_inflation() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // THE anti-inflation test: a SUM over one to-many relation (posts) and a
    // COUNT over a DIFFERENT to-many relation reached THROUGH posts
    // (comments) in the SAME fetch. A shared-JOIN implementation would
    // multiply price_total by the per-post comment fan-out
    // (10*3 + 40*0 = 30, wrong); independent correlated subqueries give the
    // true sum (10+40=50) alongside the true count (3), because neither
    // subquery's JOIN is visible to the other.
    let rows = User::objects()
        .annotate_sum("price_total", "posts__price")
        .annotate_count("posts__comments")
        .fetch_annotated()
        .await
        .expect("stacked deep-path annotations");
    let alice = by_name(&rows, "alice");
    assert_eq!(
        alice["price_total"],
        json!(50),
        "SUM(price) across alice's two posts must be 10+40=50, not multiplied by comment fan-out"
    );
    assert_eq!(
        alice["posts__comments_count"].as_i64(),
        Some(3),
        "COUNT(comments) must stay 3, not divided/multiplied by the sibling SUM annotation"
    );
}

#[tokio::test]
async fn filter_annotation_cuts_rows_by_aggregate() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let ids = User::objects()
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Op::Gt, 0.into())
        .order_by_annotation("posts__comments_count", true)
        .values(&["id"])
        .await
        .expect("filtered + ordered by annotation");
    // Only alice has any comments; bob (0 comments) is cut by the
    // portable-HAVING wrap.
    assert_eq!(
        ids,
        vec![json!({"id": 1})],
        "filter_annotation must cut bob's zero-comment row"
    );
}

#[tokio::test]
async fn annotate_avg_min_max_over_path() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let rows = User::objects()
        .annotate_avg("price_avg", "posts__price")
        .annotate_min("price_min", "posts__price")
        .annotate_max("price_max", "posts__price")
        .fetch_annotated()
        .await
        .expect("avg/min/max over a relation path");
    let alice = by_name(&rows, "alice");
    assert_eq!(alice["price_avg"].as_f64(), Some(25.0), "avg(10, 40) = 25");
    assert_eq!(alice["price_min"].as_i64(), Some(10));
    assert_eq!(alice["price_max"].as_i64(), Some(40));

    let bob = by_name(&rows, "bob");
    assert!(
        bob["price_avg"].is_null() && bob["price_min"].is_null() && bob["price_max"].is_null(),
        "an empty related set must be NULL, never a fabricated 0"
    );
}

#[tokio::test]
async fn sum_over_a_relation_path_missing_the_column_segment_fails_loudly() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let err = User::objects()
        .annotate_sum("bad", "posts") // no `__column` segment to split off
        .fetch_annotated()
        .await
        .expect_err("must reject a sum path with no column segment");
    assert!(
        err.to_string().contains("posts") && err.to_string().contains("column"),
        "error names the bad path and explains the missing column: {err}"
    );
}

#[tokio::test]
async fn unknown_relation_path_fails_loudly() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let err = User::objects()
        .annotate_count("nope__comments")
        .fetch_annotated()
        .await
        .expect_err("an unresolvable path must not silently run a wrong query");
    assert!(
        err.to_string().contains("nope"),
        "error names the bad segment: {err}"
    );
}

// ---------------------------------------------------------------------------
// Query-count proof: N stacked deep-path annotations still fetch in ONE
// statement (Security & Performance — the epic's "never N+1" contract for
// aggregates). A dedicated, self-contained tracing counter: integration
// tests are separate binaries, so there is no shared harness to import
// (mirrors `tests/query_counts.rs` / `tests/prefetch_by_name.rs`).
// ---------------------------------------------------------------------------

mod query_count_harness {
    use std::sync::Once;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;

    static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
    static INIT: Once = Once::new();
    static COUNT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct CountLayer;

    struct StmtVisitor(Option<String>);
    impl tracing::field::Visit for StmtVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "summary" || field.name() == "db.statement" {
                self.0 = Some(format!("{value:?}"));
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "summary" || field.name() == "db.statement" {
                self.0 = Some(value.to_string());
            }
        }
    }

    impl<S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>> Layer<S>
        for CountLayer
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            if !event.metadata().target().starts_with("sqlx::query") {
                return;
            }
            let mut v = StmtVisitor(None);
            event.record(&mut v);
            let stmt = v.0.unwrap_or_default();
            if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
                return;
            }
            QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn install() {
        INIT.call_once(|| {
            tracing_subscriber::registry()
                .with(LevelFilter::TRACE)
                .with(CountLayer)
                .init();
        });
    }

    pub async fn query_lock() -> tokio::sync::MutexGuard<'static, ()> {
        install();
        COUNT_LOCK.lock().await
    }

    pub fn reset() {
        QUERY_COUNT.store(0, Ordering::SeqCst);
    }

    pub fn count() -> usize {
        QUERY_COUNT.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn annotate_multiple_deep_paths_is_one_query_not_n_plus_1() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    query_count_harness::reset();
    let rows = User::objects()
        .annotate_sum("price_total", "posts__price")
        .annotate_count("posts__comments")
        .annotate_count("categories")
        .fetch_annotated()
        .await
        .expect("three stacked deep-path annotations");
    assert!(rows.len() >= 2, "sanity: both seeded users returned");
    assert_eq!(
        query_count_harness::count(),
        1,
        "three correlated-subquery annotations must still ride in ONE statement"
    );
}
