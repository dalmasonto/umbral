//! Gap #44 end-to-end — `prefetch_related("comment_set")` on a
//! parent with a `ReverseSet<Comment>` field loads every comment
//! pointing back at each post in one batched query.
//!
//! Pins: macro recognizes `#[umbral(reverse_fk = "...")]`, the
//! parent's `set_m2m_parent_ids` (renamed concept — now covers both
//! M2M and ReverseSet) wires `parent_id` + `fk_column`, the prefetch
//! dispatch finds the spec in `REVERSE_FK_RELATIONS`, runs one
//! batched IN, and the per-field arm in `set_reverse_fk_resolved_json`
//! populates each parent's `ReverseSet`.
//!
//! Query budget: 1 (parents) + 1 (children) — no N+1.

#![allow(dead_code)]

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use umbral::orm::{ForeignKey, ReverseSet};

// =========================================================================
// Query counter — this file is its own test binary/process, so it carries
// its own copy of the sqlx-tracing query counter — same duplication
// rationale as `tests/query_counts.rs` / `tests/prefetch_accessor_cache.rs`.
// Every DB-touching test below acquires `query_lock()` (even ones that
// don't assert on `count()`) so the two zero/non-zero-query proofs never
// see another test's queries interleaved into the shared counter.
// =========================================================================

static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();
static COUNT_LOCK: Mutex<()> = Mutex::const_new(());

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

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CountLayer {
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

/// Acquire the counting lock — hold it across setup + measurement so no
/// other counting test's queries leak into this count.
async fn query_lock() -> MutexGuard<'static, ()> {
    install();
    COUNT_LOCK.lock().await
}

/// Zero the counter. Call immediately before the operation being measured.
fn reset() {
    QUERY_COUNT.store(0, Ordering::SeqCst);
}

/// Statements counted since the last [`reset`].
fn count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfk_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfk_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    /// The macro recognises this as a `ReverseSet<Comment>` field
    /// and skips it from the FromRow column list (hence
    /// `#[sqlx(skip)]`) + the Serialize-by-default shape (hence
    /// `#[serde(skip)]`). The `reverse_fk = "post"` attribute names
    /// the FK column on `Comment` that points back.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "post")]
    pub comment_set: ReverseSet<Comment>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // A real temp FILE DB, not `sqlite::memory:`. An in-memory SQLite DB
        // lives only as long as its owning connection, and under `#[tokio::test]`
        // each test has its own runtime: the lazily-created pool's keep-alive
        // connection is bound to whichever test won `boot()`, so when that
        // test's runtime ends the shared in-memory DB is torn down and other
        // parallel tests see an empty schema ("no such table" race). A file DB
        // is shared across every connection and runtime with no lifetime
        // coupling. Unique per process + removed first so seeding stays
        // idempotent across `cargo test` runs (built directly, not via
        // `connect_sqlite`, to keep `sqlx::query` tracing ON for the counter).
        let db_path = std::env::temp_dir().join(format!("rfk_prefetch_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .connect_with(opts)
            .await
            .expect("temp-file sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Post>()
            .model::<Comment>()
            .model::<Article>()
            .model::<Note>()
            .model::<Tagline>()
            .build()
            .expect("App::build");

        // orm_fixes #1 fixture: an Article with TWO reverse sets. The schema
        // for rfk_post/rfk_comment/rfk_article/rfk_note/rfk_tagline all comes
        // from the registered models above.
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        sqlx::query("INSERT INTO rfk_article (headline) VALUES ('a1')")
            .execute(&pool)
            .await
            .expect("seed article");
        for text in &["n1", "n2"] {
            sqlx::query("INSERT INTO rfk_note (text, article) VALUES (?, 1)")
                .bind(*text)
                .execute(&pool)
                .await
                .expect("seed note");
        }
        // Bind a real `DateTime<Utc>` exactly as production writes it —
        // sqlx encodes it space-separated for SQLite. This is the value
        // chrono's RFC3339 `Deserialize` later chokes on.
        let now: chrono::DateTime<chrono::Utc> = chrono::Utc::now();
        for phrase in &["t1"] {
            sqlx::query("INSERT INTO rfk_tagline (phrase, article, created_at) VALUES (?, 1, ?)")
                .bind(*phrase)
                .bind(now)
                .execute(&pool)
                .await
                .expect("seed tagline");
        }

        // alpha (1): 2 comments
        // beta  (2): 1 comment
        // gamma (3): 0 comments
        for title in &["alpha", "beta", "gamma"] {
            sqlx::query("INSERT INTO rfk_post (title) VALUES (?)")
                .bind(*title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        for (body, post) in &[
            ("first on alpha", 1_i64),
            ("second on alpha", 1),
            ("first on beta", 2),
        ] {
            sqlx::query("INSERT INTO rfk_comment (body, post) VALUES (?, ?)")
                .bind(*body)
                .bind(*post)
                .execute(&pool)
                .await
                .expect("seed comment");
        }
    })
    .await;
}

#[tokio::test]
async fn prefetch_related_populates_reverse_set_for_each_parent() {
    let _g = query_lock().await;
    boot().await;
    let posts = Post::objects()
        .prefetch_related("comment_set")
        .fetch()
        .await
        .expect("fetch");

    // Index by title so test parallelism (the boot is shared) doesn't
    // collapse the assertions if other tests add posts.
    let by_title: std::collections::HashMap<&str, &Post> =
        posts.iter().map(|p| (p.title.as_str(), p)).collect();

    reset();
    let alpha = by_title.get("alpha").expect("alpha present");
    let alpha_comments = alpha
        .comment_set()
        .fetch()
        .await
        .expect("ReverseSet hydrated post-prefetch");
    let beta = by_title.get("beta").expect("beta present");
    let beta_comments = beta.comment_set().fetch().await.expect("hydrated");
    let gamma = by_title.get("gamma").expect("gamma present");
    let gamma_comments = gamma.comment_set().fetch().await.expect("hydrated (empty)");
    assert_eq!(
        count(),
        0,
        "post.comment_set().fetch() must serve the prefetch cache with zero queries"
    );

    assert_eq!(alpha_comments.len(), 2, "alpha has 2 comments");
    let bodies: Vec<&str> = alpha_comments.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"first on alpha"));
    assert!(bodies.contains(&"second on alpha"));

    assert_eq!(beta_comments.len(), 1);
    assert_eq!(beta_comments[0].body, "first on beta");

    assert!(
        gamma_comments.is_empty(),
        "gamma has no children → accessor resolves to []"
    );
}

#[tokio::test]
async fn without_prefetch_reverse_set_accessor_still_queries() {
    let _g = query_lock().await;
    boot().await;
    let posts = Post::objects().fetch().await.expect("fetch");

    reset();
    for p in &posts {
        // Without .prefetch_related("comment_set"), every post's
        // ReverseSet cache is empty, so the accessor must fall back to a
        // real per-post query rather than silently serving stale data.
        let _ = p.comment_set().fetch().await.expect("un-cached fetch");
    }
    assert!(
        count() >= posts.len(),
        "un-prefetched comment_set accessor must issue a real query per post; saw {} for {} posts",
        count(),
        posts.len()
    );
}

#[tokio::test]
async fn loud_error_on_unknown_prefetch_field_naming_reverse_set() {
    let _g = query_lock().await;
    boot().await;
    let err = Post::objects()
        .prefetch_related("no_such_field")
        .fetch()
        .await
        .expect_err("unknown field must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no_such_field"),
        "error names the bad field: {msg}"
    );
}

// =========================================================================
// orm_fixes #1 — a parent with TWO `ReverseSet<C>` fields (two different
// child models). Prefetching the SECOND set (or both) must populate the
// right slot. The website hit this: `Plugin` had `comment_set` +
// `feature_set`, and prefetching `feature_set` came back empty.
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfk_note")]
pub struct Note {
    pub id: i64,
    pub text: String,
    pub article: ForeignKey<Article>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfk_tagline")]
pub struct Tagline {
    pub id: i64,
    pub phrase: String,
    pub article: ForeignKey<Article>,
    /// A `DateTime<Utc>` child column — the prefetch hydration decodes
    /// each child row via `serde_json::from_value::<Tagline>(..)`, so a
    /// datetime that didn't round-trip would silently drop the whole row
    /// and empty the bucket. Mirrors `PluginFeature::created_at` on the
    /// website; pins that the round-trip holds.
    #[umbral(auto_now_add)]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Mirrors `Plugin` on the website as closely as possible: it is
/// `soft_delete`, carries an explicit `#[umbral(primary_key)] id`, and has
/// two reverse sets to two different child models (both reverse via the
/// same FK column name, `article`).
#[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(soft_delete, table = "rfk_article")]
pub struct Article {
    #[umbral(primary_key)]
    pub id: i64,
    pub headline: String,
    /// FIRST reverse set.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "article")]
    pub note_set: ReverseSet<Note>,
    /// SECOND reverse set — the one the website's prefetch silently
    /// dropped.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "article")]
    pub tagline_set: ReverseSet<Tagline>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Direct test of the documented (wrong) root cause: the macro must
/// emit a `REVERSE_FK_RELATIONS` entry for EVERY `ReverseSet` field,
/// not just the first. (It does — this guards against a regression to
/// the single-field shape.)
#[test]
fn macro_emits_a_reverse_fk_spec_for_every_set() {
    use umbral::orm::Model;
    let names: Vec<&str> = Article::REVERSE_FK_RELATIONS
        .iter()
        .map(|s| s.field_name)
        .collect();
    assert!(names.contains(&"note_set"), "first set present: {names:?}");
    assert!(
        names.contains(&"tagline_set"),
        "SECOND set present: {names:?}"
    );
    assert_eq!(names.len(), 2, "exactly the two declared sets: {names:?}");
}

/// Prefetch BOTH reverse sets — each slot must carry its own children.
#[tokio::test]
async fn prefetch_both_reverse_sets_populates_each_slot() {
    let _g = query_lock().await;
    boot().await;
    let articles = Article::objects()
        .prefetch_related("note_set")
        .prefetch_related("tagline_set")
        .fetch()
        .await
        .expect("fetch");
    let a = articles
        .iter()
        .find(|a| a.headline == "a1")
        .expect("a1 present");

    reset();
    let notes = a.note_set().fetch().await.expect("note_set hydrated");
    let taglines = a.tagline_set().fetch().await.expect("tagline_set hydrated");
    assert_eq!(
        count(),
        0,
        "both reverse-set accessors must serve the prefetch cache with zero queries"
    );

    let mut note_texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
    note_texts.sort();
    assert_eq!(note_texts, vec!["n1", "n2"], "note_set has both notes");

    let tag_phrases: Vec<&str> = taglines.iter().map(|t| t.phrase.as_str()).collect();
    assert_eq!(tag_phrases, vec!["t1"], "tagline_set has its tagline");
}

/// The exact website shape: prefetch ONLY the SECOND reverse set and
/// assert it populates (the first stays un-cached and falls back to a
/// real query).
#[tokio::test]
async fn prefetch_only_second_reverse_set_populates_it() {
    let _g = query_lock().await;
    boot().await;
    let articles = Article::objects()
        .prefetch_related("tagline_set")
        .fetch()
        .await
        .expect("fetch");
    let a = articles
        .iter()
        .find(|a| a.headline == "a1")
        .expect("a1 present");

    reset();
    let taglines = a
        .tagline_set()
        .fetch()
        .await
        .expect("second reverse set must hydrate even when prefetched alone");
    assert_eq!(
        count(),
        0,
        "tagline_set().fetch() must serve the prefetch cache with zero queries"
    );
    let tag_phrases: Vec<&str> = taglines.iter().map(|t| t.phrase.as_str()).collect();
    assert_eq!(tag_phrases, vec!["t1"]);

    // First set wasn't prefetched → stays un-cached, so its accessor must
    // fall back to a real query (not silently return an empty/stale set).
    reset();
    let notes = a.note_set().fetch().await.expect("un-cached fetch");
    assert_eq!(
        notes.len(),
        2,
        "un-prefetched first set must still resolve correctly via a real query"
    );
    assert!(
        count() >= 1,
        "un-prefetched note_set accessor must issue a real query"
    );
}
