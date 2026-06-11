//! Related-aggregate annotations — Django's chainable
//! `Post.objects.filter(...).annotate(n=Count("comments"))`:
//! annotations are QUERY-BUILDER STATE, applied inside the one SELECT
//! every terminal builds. Pins:
//!
//! - counts arrive with the rows in one query (no N+1),
//! - annotations STACK (`.annotate_count(...)` +
//!   `.annotate_related(..., Aggregate::avg(...))` on one queryset),
//! - `.to_sql()` and `.explain()` see the annotations out of the box
//!   (the user-facing proof that this is builder state, not a bolt-on
//!   side query),
//! - parent `.filter()` composes,
//! - an unknown relation fails loudly on every fallible consumer.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{Aggregate, ForeignKey, ReverseSet};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_review")]
pub struct Review {
    pub id: i64,
    pub rating: f64,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_note", soft_delete)]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
    pub moderation: String,
    #[sqlx(default)]
    #[umbra(index)]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_tag")]
pub struct Tag {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "post")]
    pub comment_set: ReverseSet<Comment>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "post")]
    pub review_set: ReverseSet<Review>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "post")]
    pub note_set: ReverseSet<Note>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(m2m = "anc_tag")]
    pub tags: umbra::orm::M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Post>()
            .model::<Comment>()
            .model::<Review>()
            .model::<Note>()
            .model::<Tag>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE anc_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_post");
        sqlx::query(
            "CREATE TABLE anc_comment (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                body TEXT NOT NULL,
                post INTEGER NOT NULL REFERENCES anc_post(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_comment");
        sqlx::query(
            "CREATE TABLE anc_review (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rating REAL NOT NULL,
                post INTEGER NOT NULL REFERENCES anc_post(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_review");
        sqlx::query(
            "CREATE TABLE anc_note (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                body TEXT NOT NULL,
                post INTEGER NOT NULL REFERENCES anc_post(id),
                moderation TEXT NOT NULL DEFAULT 'visible',
                deleted_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_note");
        sqlx::query(
            "CREATE TABLE anc_tag (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_tag");
        sqlx::query(
            "CREATE TABLE anc_post_tags (
                parent_id INTEGER NOT NULL REFERENCES anc_post(id),
                child_id INTEGER NOT NULL REFERENCES anc_tag(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_post_tags");

        for title in ["alpha", "beta", "gamma"] {
            sqlx::query("INSERT INTO anc_post (title) VALUES (?)")
                .bind(title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // alpha (id 1): two comments, beta (id 2): one, gamma: none.
        for (body, post) in [("a1", 1), ("a2", 1), ("b1", 2)] {
            sqlx::query("INSERT INTO anc_comment (body, post) VALUES (?, ?)")
                .bind(body)
                .bind(post)
                .execute(&pool)
                .await
                .expect("seed comment");
        }
        // alpha: ratings 4.0 + 5.0 (avg 4.5); beta/gamma: none.
        for (rating, post) in [(4.0_f64, 1), (5.0_f64, 1)] {
            sqlx::query("INSERT INTO anc_review (rating, post) VALUES (?, ?)")
                .bind(rating)
                .bind(post)
                .execute(&pool)
                .await
                .expect("seed review");
        }
        // alpha (id 1): two visible notes + one hidden, none soft-deleted.
        // Kept pristine so the annotate_count_where test is order-independent
        // (the soft-delete test seeds its OWN throwaway parent + notes).
        for (body, post, moderation) in [
            ("n1", 1, "visible"),
            ("n2", 1, "visible"),
            ("n3", 1, "hidden"),
        ] {
            sqlx::query("INSERT INTO anc_note (body, post, moderation) VALUES (?, ?, ?)")
                .bind(body)
                .bind(post)
                .bind(moderation)
                .execute(&pool)
                .await
                .expect("seed note");
        }
        // Three candidate tags; alpha (post 1) gets 2 of them via real
        // junction rows. beta/gamma get none.
        for name in ["rust", "web", "orm"] {
            sqlx::query("INSERT INTO anc_tag (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        for (parent, child) in [(1, 1), (1, 2)] {
            sqlx::query("INSERT INTO anc_post_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(parent)
                .bind(child)
                .execute(&pool)
                .await
                .expect("seed junction");
        }
    })
    .await;
}

#[tokio::test]
async fn counts_arrive_with_the_rows_in_one_query() {
    boot().await;
    let rows = Post::objects()
        .annotate_count("comment_set")
        .fetch_annotated()
        .await
        .expect("fetch_annotated");
    // At least the three seeded posts come back (another test may have
    // added a throwaway parent to the shared in-memory DB; we assert on
    // titles, not total length, to stay order-independent).
    assert!(rows.len() >= 3);
    let by_title: std::collections::HashMap<String, i64> = rows
        .into_iter()
        .map(|(p, anns)| (p.title, anns["comment_set_count"].as_i64().unwrap()))
        .collect();
    assert_eq!(by_title["alpha"], 2);
    assert_eq!(by_title["beta"], 1);
    assert_eq!(
        by_title["gamma"], 0,
        "no children must mean 0, not a missing row"
    );
}

#[tokio::test]
async fn annotations_stack_count_and_avg_in_one_query() {
    boot().await;
    // The Django story: .annotate(comments_count).annotate(rating_avg).
    let rows = Post::objects()
        .annotate_count("comment_set")
        .annotate_related("rating_avg", "review_set", Aggregate::avg("rating"))
        .fetch_annotated()
        .await
        .expect("stacked annotations");
    let alpha = rows
        .iter()
        .find(|(p, _)| p.title == "alpha")
        .expect("alpha row");
    assert_eq!(alpha.1["comment_set_count"].as_i64(), Some(2));
    assert_eq!(alpha.1["rating_avg"].as_f64(), Some(4.5));
    let gamma = rows
        .iter()
        .find(|(p, _)| p.title == "gamma")
        .expect("gamma row");
    assert_eq!(gamma.1["comment_set_count"].as_i64(), Some(0));
    assert!(
        gamma.1["rating_avg"].is_null(),
        "AVG over an empty set is NULL, never a fabricated number"
    );
}

#[tokio::test]
async fn to_sql_and_explain_see_the_annotations() {
    boot().await;
    // to_sql: the annotation is part of the built statement.
    let sql = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .annotate_count("comment_set")
        .annotate_related("rating_avg", "review_set", Aggregate::avg("rating"))
        .to_sql();
    assert!(
        sql.contains("SELECT COUNT(*) FROM \"anc_comment\""),
        "count subquery missing from to_sql: {sql}"
    );
    assert!(
        sql.contains("AVG(\"rating\")") && sql.contains("\"anc_review\""),
        "avg subquery missing from to_sql: {sql}"
    );
    assert!(
        sql.contains("\"comment_set_count\"") && sql.contains("\"rating_avg\""),
        "aliases missing from to_sql: {sql}"
    );

    // explain: works out of the box on an annotated queryset.
    let plan = Post::objects()
        .annotate_count("comment_set")
        .explain()
        .await
        .expect("explain on an annotated queryset");
    assert!(!plan.is_empty(), "explain produced a plan");
}

#[tokio::test]
async fn composes_with_parent_filters() {
    boot().await;
    let rows = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .annotate_count("comment_set")
        .fetch_annotated()
        .await
        .expect("filtered annotated fetch");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.title, "alpha");
    assert_eq!(rows[0].1["comment_set_count"].as_i64(), Some(2));
}

#[tokio::test]
async fn unknown_relation_fails_loudly_everywhere() {
    boot().await;
    let err = Post::objects()
        .annotate_count("nope_set")
        .fetch_annotated()
        .await
        .expect_err("fetch_annotated must reject an unknown relation");
    let msg = err.to_string();
    assert!(
        msg.contains("nope_set") && msg.contains("comment_set"),
        "error names the bad field and the valid ones: {msg}"
    );

    let err = Post::objects()
        .annotate_count("nope_set")
        .explain()
        .await
        .expect_err("explain must reject an unknown relation too");
    assert!(err.to_string().contains("nope_set"));
}

#[test]
fn reverse_fk_spec_carries_child_soft_delete() {
    use umbra::orm::Model;
    let note = Post::REVERSE_FK_RELATIONS
        .iter()
        .find(|r| r.field_name == "note_set")
        .expect("note_set relation");
    assert!(note.soft_delete, "child Note is soft-delete");
    let comment = Post::REVERSE_FK_RELATIONS
        .iter()
        .find(|r| r.field_name == "comment_set")
        .expect("comment_set relation");
    assert!(!comment.soft_delete, "child Comment is not soft-delete");
}

#[tokio::test]
async fn annotate_count_excludes_soft_deleted_children() {
    boot().await;
    // Seed a throwaway parent + 3 notes so this test owns its own state
    // (boot()'s OnceCell is process-wide; alpha's seed stays pristine for
    // the annotate_count_where test). Then soft-delete exactly one via the
    // REAL soft-delete path: delete() on a soft_delete model UPDATEs
    // deleted_at rather than removing the row.
    let p = umbra_core::db::pool();
    sqlx::query("INSERT INTO anc_post (title) VALUES ('delta')")
        .execute(&p)
        .await
        .expect("seed delta post");
    let delta_id: i64 = sqlx::query_scalar("SELECT id FROM anc_post WHERE title = 'delta'")
        .fetch_one(&p)
        .await
        .expect("delta id");
    for body in ["d1", "d2", "d3"] {
        sqlx::query("INSERT INTO anc_note (body, post, moderation) VALUES (?, ?, 'visible')")
            .bind(body)
            .bind(delta_id)
            .execute(&p)
            .await
            .expect("seed delta note");
    }

    let removed = Note::objects()
        .filter(note::BODY.eq("d2"))
        .delete()
        .await
        .expect("soft-delete one note");
    assert_eq!(removed, 1, "exactly one note soft-deleted");

    let rows = Post::objects()
        .annotate_count("note_set")
        .fetch_annotated()
        .await
        .expect("fetch_annotated");
    let by_title: std::collections::HashMap<String, i64> = rows
        .into_iter()
        .map(|(p, a)| (p.title, a["note_set_count"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        by_title["delta"], 2,
        "soft-deleted note must NOT be counted (3 seeded, 1 trashed)"
    );
    assert_eq!(
        by_title["gamma"], 0,
        "a parent with zero notes is still returned as 0, not dropped"
    );
}

#[tokio::test]
async fn annotate_count_where_filters_children() {
    boot().await;
    // alpha's pristine seed: n1/n2 visible, n3 hidden (none soft-deleted).
    // The child predicate filters the correlated count to visible-only.
    let rows = Post::objects()
        .annotate_count_where::<Note>("visible_notes", "note_set", note::MODERATION.eq("visible"))
        .fetch_annotated()
        .await
        .expect("fetch_annotated with child filter");
    let alpha = rows
        .iter()
        .find(|(p, _)| p.title == "alpha")
        .expect("alpha row");
    assert_eq!(
        alpha.1["visible_notes"].as_i64(),
        Some(2),
        "only the two visible notes count; the hidden one is excluded"
    );
}

#[tokio::test]
async fn annotate_count_over_m2m_counts_junction_rows() {
    boot().await;
    let rows = Post::objects()
        .annotate_count("tags")
        .fetch_annotated()
        .await
        .expect("fetch_annotated over m2m");
    let by_title: std::collections::HashMap<String, i64> = rows
        .into_iter()
        .map(|(p, a)| (p.title, a["tags_count"].as_i64().unwrap()))
        .collect();
    assert_eq!(by_title["alpha"], 2, "two junction rows attach to alpha");
    assert_eq!(by_title["beta"], 0, "beta has no tags");
}
