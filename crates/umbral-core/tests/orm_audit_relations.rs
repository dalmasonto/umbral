//! ORM audit — bug-catching tests for the relation paths
//! (select_related / join_related / prefetch_related / ReverseSet /
//! values traversal).
//!
//! These tests target edges the original feature tests covered only
//! weakly or by side effect:
//!
//!   - **Multi-parent dedup**: when N parents share a related row,
//!     does the batched fetch dedup ids before the IN query? Tested
//!     by inserting parents that point at the same related row and
//!     verifying both parents see the SAME hydrated value (not a
//!     phantom duplicate).
//!
//!   - **Cross-parent contamination**: when prefetching ReverseSets
//!     across multiple parents, does each parent get ONLY its own
//!     children? A bucketing bug would mix them. Tested by giving
//!     each parent a distinct child set + asserting strict equality
//!     against the per-parent expected slice.
//!
//!   - **JOIN cartesian on .values() traversal**: a multi-relation
//!     JOIN over many parents could blow up rows (one per
//!     parent×rel-cardinality combo). Verified by counting result
//!     rows against the parent count, not the cartesian product.
//!
//!   - **Mixed M2M + ReverseSet prefetch**: both kinds resolved in
//!     one fetch should each populate their own slot without
//!     stepping on the other.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, M2M, ReverseSet};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "audr_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "audr_user")]
pub struct User {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub manager: Option<ForeignKey<User>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "audr_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "audr_review")]
pub struct Review {
    pub id: i64,
    pub stars: i64,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "audr_post")]
pub struct Post {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub author: ForeignKey<User>,
    pub editor: Option<ForeignKey<User>>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "audr_tag")]
    pub tags: M2M<Tag>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "post")]
    pub comment_set: ReverseSet<Comment>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "post")]
    pub review_set: ReverseSet<Review>,
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
            .model::<User>()
            .model::<Tag>()
            .model::<Post>()
            .model::<Comment>()
            .model::<Review>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Users:
        //   1: ceo       (no manager)
        //   2: alice     (manager = ceo)
        //   3: bob       (manager = ceo)
        for (name, mgr) in &[("ceo", None::<i64>), ("alice", Some(1)), ("bob", Some(1))] {
            sqlx::query("INSERT INTO audr_user (name, manager) VALUES (?, ?)")
                .bind(*name)
                .bind(*mgr)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        // Tags
        for name in &["rust", "web", "db"] {
            sqlx::query("INSERT INTO audr_tag (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        // Posts:
        //   alpha (1): author=alice(2), editor=ceo(1)
        //   beta  (2): author=alice(2), editor=NULL   ← SAME author as alpha; tests dedup
        //   gamma (3): author=bob(3),   editor=NULL
        for (title, author, editor) in &[
            ("alpha", 2_i64, Some(1_i64)),
            ("beta", 2, None),
            ("gamma", 3, None),
        ] {
            sqlx::query("INSERT INTO audr_post (title, author, editor) VALUES (?, ?, ?)")
                .bind(*title)
                .bind(*author)
                .bind(*editor)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // Post tags (M2M):
        //   alpha → rust, web
        //   beta  → web
        //   gamma → (none)
        for (post, tag) in &[(1_i64, 1_i64), (1, 2), (2, 2)] {
            sqlx::query("INSERT INTO audr_post_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(*post)
                .bind(*tag)
                .execute(&pool)
                .await
                .expect("seed junction");
        }
        // Comments (reverse FK):
        //   alpha → 2 comments
        //   beta  → 1 comment
        //   gamma → 0 comments
        for (body, post) in &[
            ("alpha comment 1", 1_i64),
            ("alpha comment 2", 1),
            ("beta comment 1", 2),
        ] {
            sqlx::query("INSERT INTO audr_comment (body, post) VALUES (?, ?)")
                .bind(*body)
                .bind(*post)
                .execute(&pool)
                .await
                .expect("seed comment");
        }
        // Reviews (reverse FK):
        //   alpha → 5★, 3★
        //   beta  → 4★
        //   gamma → (none)
        for (stars, post) in &[(5_i64, 1_i64), (3, 1), (4, 2)] {
            sqlx::query("INSERT INTO audr_review (stars, post) VALUES (?, ?)")
                .bind(*stars)
                .bind(*post)
                .execute(&pool)
                .await
                .expect("seed review");
        }
    })
    .await;
}

fn by_title(posts: &[Post]) -> std::collections::HashMap<&str, &Post> {
    posts.iter().map(|p| (p.title.as_str(), p)).collect()
}

// =========================================================================
// select_related: multi-parent dedup. Two posts pointing at the same
// author should both end up with the SAME hydrated author (not, say,
// one hydrated and one not, or two different decoded instances with
// drifted data). The batched IN dedup is what makes this efficient
// AND correct.
// =========================================================================
#[tokio::test]
async fn select_related_dedups_shared_fk_target_across_parents() {
    boot().await;
    let posts = Post::objects()
        .filter(post::AUTHOR.eq(2)) // both alpha + beta point at alice(2)
        .select_related("author")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    let alpha = by.get("alpha").expect("alpha");
    let beta = by.get("beta").expect("beta");
    let alpha_author = alpha.author.resolved().expect("alpha.author hydrated");
    let beta_author = beta.author.resolved().expect("beta.author hydrated");
    assert_eq!(alpha_author.id, beta_author.id, "same author id");
    assert_eq!(alpha_author.name, beta_author.name, "same author name");
    assert_eq!(alpha_author.name, "alice");
    // Both posts share the SAME author id (no drift across decode paths).
    assert_eq!(alpha.author.id(), 2);
    assert_eq!(beta.author.id(), 2);
}

// =========================================================================
// Nested select_related with multi-parent + shared chain. Two posts
// share alice as author, and alice's manager is ceo. The hop2 query
// should dedup ceo even though it's reachable from two posts via
// alice.
// =========================================================================
#[tokio::test]
async fn nested_select_related_hydrates_full_chain_for_each_parent() {
    boot().await;
    let posts = Post::objects()
        .filter(post::AUTHOR.eq(2)) // alpha + beta → alice → ceo
        .select_related("author__manager")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    for (label, p) in &[("alpha", by["alpha"]), ("beta", by["beta"])] {
        let alice = p
            .author
            .resolved()
            .unwrap_or_else(|| panic!("{label}.author not hydrated"));
        assert_eq!(alice.name, "alice");
        let mgr = alice
            .manager
            .as_ref()
            .unwrap_or_else(|| panic!("{label}.author.manager wrapper missing"))
            .resolved()
            .unwrap_or_else(|| panic!("{label}.author.manager.resolved missing"));
        assert_eq!(
            mgr.name, "ceo",
            "{label} chain should resolve through to ceo"
        );
    }
}

// =========================================================================
// ReverseSet: cross-parent contamination check. Each parent gets ONLY
// its own children. A bucketing bug would let alpha's comments show
// up on beta or gamma. We assert STRICT membership: beta's comments
// must NOT contain alpha's comment bodies.
// =========================================================================
#[tokio::test]
async fn reverse_set_no_cross_parent_contamination() {
    boot().await;
    let posts = Post::objects()
        .prefetch_related("comment_set")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);

    let alpha_bodies: Vec<&str> = by["alpha"]
        .comment_set
        .resolved()
        .expect("alpha hydrated")
        .iter()
        .map(|c| c.body.as_str())
        .collect();
    let beta_bodies: Vec<&str> = by["beta"]
        .comment_set
        .resolved()
        .expect("beta hydrated")
        .iter()
        .map(|c| c.body.as_str())
        .collect();
    let gamma_bodies: Vec<&str> = by["gamma"]
        .comment_set
        .resolved()
        .expect("gamma hydrated (empty)")
        .iter()
        .map(|c| c.body.as_str())
        .collect();

    // Each post gets its expected count.
    assert_eq!(alpha_bodies.len(), 2, "alpha got 2 comments");
    assert_eq!(beta_bodies.len(), 1, "beta got 1 comment");
    assert_eq!(gamma_bodies.len(), 0, "gamma got 0 comments");

    // Strict membership — no comment leaks across parents.
    assert!(alpha_bodies.contains(&"alpha comment 1"));
    assert!(alpha_bodies.contains(&"alpha comment 2"));
    assert!(!alpha_bodies.contains(&"beta comment 1"));

    assert_eq!(beta_bodies, vec!["beta comment 1"]);
    assert!(!beta_bodies.contains(&"alpha comment 1"));
    assert!(!beta_bodies.contains(&"alpha comment 2"));

    // Confirm every Comment row's `post` FK truly points at its
    // declared parent — invariant check the seed data wasn't broken.
    for body in &alpha_bodies {
        assert!(body.starts_with("alpha"), "alpha contamination: {body}");
    }
}

// =========================================================================
// Multiple ReverseSet fields on one parent. Each field's prefetch
// should populate its own slot without touching the other. (A bug
// that routed reviews into comment_set, or vice versa, would surface
// here.)
// =========================================================================
#[tokio::test]
async fn multiple_reverse_set_fields_populate_independently() {
    boot().await;
    let posts = Post::objects()
        .prefetch_related("comment_set")
        .prefetch_related("review_set")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    let alpha = by["alpha"];

    let comments = alpha.comment_set.resolved().expect("comment_set hydrated");
    let reviews = alpha.review_set.resolved().expect("review_set hydrated");

    assert_eq!(comments.len(), 2, "comments on alpha");
    assert_eq!(reviews.len(), 2, "reviews on alpha");

    // Reviews bucket shouldn't contain Comment data.
    let stars: Vec<i64> = reviews.iter().map(|r| r.stars).collect();
    assert!(stars.contains(&5));
    assert!(stars.contains(&3));

    // Comments bucket shouldn't contain Review data — it would be a
    // type mismatch but worth confirming the body text is real.
    for c in comments {
        assert!(
            c.body.starts_with("alpha comment"),
            "stray data in comment_set: {}",
            c.body
        );
    }
}

// =========================================================================
// Mixed M2M + ReverseSet prefetch in the same fetch. Both should
// populate their respective slots, neither stepping on the other.
// =========================================================================
#[tokio::test]
async fn mixed_m2m_and_reverse_set_prefetch_in_one_query() {
    boot().await;
    let posts = Post::objects()
        .prefetch_related("tags") // M2M
        .prefetch_related("comment_set") // ReverseSet
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    let alpha = by["alpha"];

    let tag_names: Vec<&str> = alpha
        .tags
        .resolved()
        .expect("tags hydrated")
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        tag_names.contains(&"rust"),
        "alpha tagged rust: {tag_names:?}"
    );
    assert!(
        tag_names.contains(&"web"),
        "alpha tagged web: {tag_names:?}"
    );
    assert_eq!(tag_names.len(), 2);

    let comment_count = alpha
        .comment_set
        .resolved()
        .expect("comment_set hydrated")
        .len();
    assert_eq!(comment_count, 2);
}

// =========================================================================
// values() traversal with multi-row + shared related rows. JOIN
// against a many-to-one relation can blow up rows if the
// implementation accidentally cross-joins. Three posts, two sharing
// the same author — the result should have exactly 3 rows.
// =========================================================================
#[tokio::test]
async fn values_traversal_row_count_matches_parent_count_not_cartesian() {
    boot().await;
    let rows = Post::objects()
        .values(&["id", "title", "author__name"])
        .await
        .expect("values");
    // Strict count: one row per parent post. A cartesian bug
    // (joining each parent against the unconstrained author table)
    // would balloon to parent_count × author_count.
    let our_titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.as_object()?.get("title")?.as_str())
        .filter(|t| matches!(*t, "alpha" | "beta" | "gamma"))
        .collect();
    assert_eq!(
        our_titles.len(),
        3,
        "expected exactly 3 rows for our 3 seeded posts, got {our_titles:?}"
    );

    // Each row's nested author must match the actual FK.
    let by_title: std::collections::HashMap<String, &serde_json::Value> = rows
        .iter()
        .filter_map(|r| {
            let title = r.as_object()?.get("title")?.as_str()?.to_string();
            Some((title, r))
        })
        .collect();
    let alpha_author = by_title["alpha"]
        .get("author")
        .and_then(|v| v.as_object())
        .expect("alpha author obj");
    let beta_author = by_title["beta"]
        .get("author")
        .and_then(|v| v.as_object())
        .expect("beta author obj");
    let gamma_author = by_title["gamma"]
        .get("author")
        .and_then(|v| v.as_object())
        .expect("gamma author obj");

    assert_eq!(alpha_author["name"].as_str(), Some("alice"));
    assert_eq!(beta_author["name"].as_str(), Some("alice"));
    assert_eq!(gamma_author["name"].as_str(), Some("bob"));
}

// =========================================================================
// values() traversal: NULL on a nullable FK becomes the JSON null at
// the relation key (NOT a nested object with all-null fields). Edge
// case that would silently break consumer code branching on
// `obj["editor"].is_null()`.
// =========================================================================
#[tokio::test]
async fn values_traversal_null_fk_becomes_relation_key_null_not_null_object() {
    boot().await;
    let rows = Post::objects()
        .filter(post::TITLE.eq("beta")) // beta has editor = NULL
        .values(&["title", "editor__name"])
        .await
        .expect("values");
    let row = rows
        .iter()
        .find(|r| r.as_object().and_then(|o| o.get("title")?.as_str()) == Some("beta"))
        .expect("beta row present")
        .as_object()
        .expect("object");
    let editor = row.get("editor").expect("editor key present");
    assert!(
        editor.is_null(),
        "editor should be JSON null (not a nested obj with name=null): {editor:?}"
    );
    // Negative assertion: it's NOT a nested object at all.
    assert!(editor.as_object().is_none());
}

// =========================================================================
// .only() composition: combining with select_related gives a typed
// fetch error per gap #111. The error must name BOTH the violated
// method and point at the right replacement. Tests message quality,
// not just presence.
// =========================================================================
#[tokio::test]
async fn only_with_select_related_errors_with_actionable_message() {
    boot().await;
    let err = Post::objects()
        .select_related("author")
        .only(&["id"])
        .fetch()
        .await
        .expect_err("typed fetch with .only() must error");
    let msg = err.to_string();
    assert!(
        msg.contains(".only(...)"),
        "error must name the offending method literally: {msg}"
    );
    assert!(
        msg.contains(".values("),
        "error must point at the right alternative: {msg}"
    );
    assert!(
        msg.contains("fetch"),
        "error must name the terminal that rejected: {msg}"
    );
}

// =========================================================================
// .only() with no parent col in the projection (just joined-child
// aliases) still emits a working JOIN — the inner subquery trim
// must include the FK col for ON, not just only_cols. Regression
// test for the optimization's "FK columns from joins" clause.
// =========================================================================
#[tokio::test]
async fn only_with_just_joined_child_cols_still_includes_fk_in_inner_select() {
    boot().await;
    let sql = Post::objects()
        .only(&["author__name"]) // no parent cols at all in .only()
        .join_related("author")
        .to_sql();
    // Outer SELECT has only the requested aliased child col.
    assert!(sql.contains("\"author__name\""), "outer projection: {sql}");
    // Inner subquery still includes the FK column needed for the
    // JOIN ON — even though it's not in .only().
    let inner_start = sql.find("FROM (SELECT").expect("subquery wrap");
    let inner_end = inner_start
        + sql[inner_start..]
            .find(") AS \"__p\"")
            .expect("subquery close");
    let inner = &sql[inner_start..inner_end];
    assert!(
        inner.contains("\"author\""),
        "inner must include FK col `author` for JOIN ON: {inner}"
    );
}

// =========================================================================
// Manager forwarders match QuerySet behaviour. If select_related on
// Manager forgot to forward, the hydration path silently wouldn't
// fire — typed terminals would just return raw FKs.
// =========================================================================
#[tokio::test]
async fn manager_select_related_actually_hydrates() {
    boot().await;
    let posts = Post::objects()
        .select_related("author") // direct on Manager, no `.filter()`/etc first
        .fetch()
        .await
        .expect("fetch");
    // At least one of OUR posts must come back hydrated. Parallel
    // tests sometimes share state; filter by our seeded titles.
    let our_posts: Vec<&Post> = posts
        .iter()
        .filter(|p| matches!(p.title.as_str(), "alpha" | "beta" | "gamma"))
        .collect();
    assert!(!our_posts.is_empty(), "should see our seeded posts");
    for p in our_posts {
        assert!(
            p.author.resolved().is_some(),
            "Manager forwarder should hydrate just like QuerySet: {} unresolved",
            p.title
        );
    }
}
