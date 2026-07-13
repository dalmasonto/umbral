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

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, ReverseSet};
use umbral_core::db;

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
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
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

    let alpha = by_title.get("alpha").expect("alpha present");
    let alpha_comments = alpha
        .comment_set
        .resolved()
        .expect("ReverseSet hydrated post-prefetch");
    assert_eq!(alpha_comments.len(), 2, "alpha has 2 comments");
    let bodies: Vec<&str> = alpha_comments.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"first on alpha"));
    assert!(bodies.contains(&"second on alpha"));

    let beta = by_title.get("beta").expect("beta present");
    let beta_comments = beta.comment_set.resolved().expect("hydrated");
    assert_eq!(beta_comments.len(), 1);
    assert_eq!(beta_comments[0].body, "first on beta");

    let gamma = by_title.get("gamma").expect("gamma present");
    let gamma_comments = gamma.comment_set.resolved().expect("hydrated (empty)");
    assert!(
        gamma_comments.is_empty(),
        "gamma has no children → resolved is Some(&[])"
    );
}

#[tokio::test]
async fn without_prefetch_reverse_set_resolved_is_none() {
    boot().await;
    let posts = Post::objects().fetch().await.expect("fetch");
    for p in &posts {
        // Without .prefetch_related("comment_set"), every post's
        // ReverseSet stays unloaded.
        assert!(
            p.comment_set.resolved().is_none(),
            "unloaded ReverseSet must read as None"
        );
    }
}

#[tokio::test]
async fn loud_error_on_unknown_prefetch_field_naming_reverse_set() {
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

    let notes = a.note_set.resolved().expect("note_set hydrated");
    let mut note_texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
    note_texts.sort();
    assert_eq!(note_texts, vec!["n1", "n2"], "note_set has both notes");

    let taglines = a.tagline_set.resolved().expect("tagline_set hydrated");
    let tag_phrases: Vec<&str> = taglines.iter().map(|t| t.phrase.as_str()).collect();
    assert_eq!(tag_phrases, vec!["t1"], "tagline_set has its tagline");
}

/// The exact website shape: prefetch ONLY the SECOND reverse set and
/// assert it populates (the first is left untouched / None).
#[tokio::test]
async fn prefetch_only_second_reverse_set_populates_it() {
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

    let taglines = a
        .tagline_set
        .resolved()
        .expect("second reverse set must hydrate even when prefetched alone");
    let tag_phrases: Vec<&str> = taglines.iter().map(|t| t.phrase.as_str()).collect();
    assert_eq!(tag_phrases, vec!["t1"]);

    // First set wasn't prefetched → stays unloaded.
    assert!(
        a.note_set.resolved().is_none(),
        "un-prefetched first set stays None"
    );
}
