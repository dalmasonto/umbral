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
use umbral::orm::{Cmp, ForeignKey, M2M, ReverseSet};

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
#[umbral(table = "comments", soft_delete)]
pub struct Comment {
    #[umbral(primary_key)]
    pub id: i64,
    pub post: ForeignKey<Post>,
    #[sqlx(default)]
    #[umbral(index)]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "arp_post", soft_delete)]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub author: ForeignKey<User>,
    pub price: i64,
    // Named to match `HARD_DENIED_FIELDS` (`orm::secrets`) — the name-based
    // secrecy backstop, no `Masked<T>`/mask keyring needed to prove the
    // aggregate-path guard. Nullable so the existing raw-SQL `INSERT INTO
    // arp_post (price, author) VALUES (...)` seeds above (which don't name
    // this column) keep working unchanged.
    #[sqlx(default)]
    pub password_hash: Option<String>,
    // gaps6 #5 — `Post` is now an INTERMEDIATE hop in "posts__comments" that
    // can itself be soft-deleted; proves the deep-path aggregate scopes every
    // hop, not just the leaf (`Comment`, already soft_delete above).
    #[sqlx(default)]
    #[umbral(index)]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
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

        // carol (id 3), dave (id 4), erin (id 5): each one post with one
        // comment — all three PASS a `posts__comments_count > 0` filter.
        // Combined with bob (id 2, 0 comments, FAILS the same filter) this
        // gives `filter_annotation_composes_with_limit_after_the_filter`
        // (below) a PK-first-fails-then-several-pass subset
        // (`WHERE id >= 2`) to prove `.limit()` paginates the FILTERED set.
        for name in ["carol", "dave", "erin"] {
            sqlx::query("INSERT INTO arp_user (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        for (post_id, author) in [(3_i64, 3_i64), (4_i64, 4_i64), (5_i64, 5_i64)] {
            sqlx::query("INSERT INTO arp_post (price, author) VALUES (0, ?)")
                .bind(author)
                .execute(&pool)
                .await
                .expect("seed post");
            sqlx::query("INSERT INTO comments (post) VALUES (?)")
                .bind(post_id)
                .execute(&pool)
                .await
                .expect("seed comment");
        }

        // frank (id 6): one post (id 6) with TWO comments — one active, one
        // SOFT-DELETED. `annotate_count("posts__comments")` must count only
        // the active one, matching the single-hop `annotate_count`'s
        // existing soft-delete exclusion (`annotate_count.rs`'s
        // `annotate_count_excludes_soft_deleted_children`).
        sqlx::query("INSERT INTO arp_user (name) VALUES ('frank')")
            .execute(&pool)
            .await
            .expect("seed frank");
        sqlx::query("INSERT INTO arp_post (price, author) VALUES (0, 6)")
            .execute(&pool)
            .await
            .expect("seed frank's post");
        sqlx::query("INSERT INTO comments (post, deleted_at) VALUES (6, NULL)")
            .execute(&pool)
            .await
            .expect("seed frank's active comment");
        sqlx::query("INSERT INTO comments (post, deleted_at) VALUES (6, ?)")
            .bind(chrono::Utc::now())
            .execute(&pool)
            .await
            .expect("seed frank's soft-deleted comment");

        // gina (id 7, post 7, 5 comments), hank (id 8, post 8, 4 comments),
        // iris (id 9, post 9, 2 comments) — three more passing users with
        // DISTINCT `posts__comments_count` values (5, 4, 2), used by
        // `filter_annotation_plus_order_by_annotation_plus_limit_returns_the_correct_top_rows`
        // (below) to pin the CORRECT top-2 by count (gina, hank) — every
        // OTHER passing user in the shared seed has count 1 or 3, so
        // neither can tie for the top 2.
        for (name, post_id, author, n_comments) in [
            ("gina", 7_i64, 7_i64, 5_i64),
            ("hank", 8, 8, 4),
            ("iris", 9, 9, 2),
        ] {
            sqlx::query("INSERT INTO arp_user (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed user");
            sqlx::query("INSERT INTO arp_post (price, author) VALUES (0, ?)")
                .bind(author)
                .execute(&pool)
                .await
                .expect("seed post");
            for _ in 0..n_comments {
                sqlx::query("INSERT INTO comments (post) VALUES (?)")
                    .bind(post_id)
                    .execute(&pool)
                    .await
                    .expect("seed comment");
            }
        }

        // judy (id 10, post 10): the post is itself SOFT-DELETED but its 2
        // comments are still live — gaps6 #5. karen (id 11, post 11): a live
        // post with the same shape, as a control. Post ids continue the
        // autoincrement sequence from iris's post (9) above.
        sqlx::query("INSERT INTO arp_user (name) VALUES ('judy')")
            .execute(&pool)
            .await
            .expect("seed judy");
        sqlx::query("INSERT INTO arp_post (price, author, deleted_at) VALUES (0, 10, ?)")
            .bind(chrono::Utc::now())
            .execute(&pool)
            .await
            .expect("seed judy's soft-deleted post");
        for _ in 0..2 {
            sqlx::query("INSERT INTO comments (post) VALUES (10)")
                .execute(&pool)
                .await
                .expect("seed judy's comment");
        }

        sqlx::query("INSERT INTO arp_user (name) VALUES ('karen')")
            .execute(&pool)
            .await
            .expect("seed karen");
        sqlx::query("INSERT INTO arp_post (price, author) VALUES (0, 11)")
            .execute(&pool)
            .await
            .expect("seed karen's live post");
        for _ in 0..2 {
            sqlx::query("INSERT INTO comments (post) VALUES (11)")
                .execute(&pool)
                .await
                .expect("seed karen's comment");
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
async fn annotate_count_over_deep_path_excludes_soft_deleted_leaf_rows() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // frank has 2 comments seeded, one of them soft-deleted; the deep-path
    // `annotate_count("posts__comments")` must count only the 1 active one
    // — the same soft-delete scoping the single-hop `annotate_count`
    // already enforces for a directly-declared `ReverseSet` relation.
    let rows = User::objects()
        .annotate_count("posts__comments")
        .fetch_annotated()
        .await
        .expect("fetch_annotated");
    assert_eq!(
        by_name(&rows, "frank")["posts__comments_count"].as_i64(),
        Some(1),
        "the soft-deleted comment must not inflate the deep-path count"
    );
}

/// gaps6 #5 — the deep-path aggregate used to scope soft-delete on the LEAF
/// table only. A soft-deleted INTERMEDIATE hop (here, `Post`) must also
/// exclude its children from the aggregate, even though the children
/// themselves are still live rows.
#[tokio::test]
async fn annotate_count_over_deep_path_excludes_children_under_a_soft_deleted_intermediate_hop() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    let rows = User::objects()
        .annotate_count("posts__comments")
        .fetch_annotated()
        .await
        .expect("fetch_annotated");
    assert_eq!(
        by_name(&rows, "judy")["posts__comments_count"].as_i64(),
        Some(0),
        "judy's post is soft-deleted; its 2 live comments must not be counted"
    );
    assert_eq!(
        by_name(&rows, "karen")["posts__comments_count"].as_i64(),
        Some(2),
        "karen's post is live; her 2 comments must count normally"
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
    // Scoped to alice/bob only (`id < 3`) — carol/dave/erin (ids 3-5, added
    // for the limit-composition test below) also pass this filter, so this
    // assertion would otherwise need to grow every time the shared seed
    // does. The `annotate_count("posts__comments")`+ `filter_annotation`
    // wrap works identically whether it or the parent `.filter()` narrows
    // the row set first (both fold into the same one built `SelectStatement`).
    let ids = User::objects()
        .filter(user::ID.lt(3))
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Cmp::Gt, 0.into())
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
async fn filter_annotation_composes_with_limit_after_the_filter() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // Subset `2 <= id <= 5` (excludes frank, id 6, added later for the
    // soft-delete test — scoped explicitly so this test stays correct
    // regardless of what the shared seed grows to next): bob (id 2,
    // PK-FIRST in this subset) FAILS the filter (0 comments); carol/dave/
    // erin (ids 3-5) all PASS it (1 comment each). A buggy implementation
    // that applies `.limit(n)` to the PRE-filter statement would truncate
    // this subset to its first 2 PK rows — [bob (fail), carol (pass)] —
    // THEN filter, leaving only ONE row (carol) instead of two. The correct
    // behavior filters FIRST (dropping bob) and only THEN takes 2 of the 3
    // remaining passing rows.
    let rows = User::objects()
        .filter(user::ID.ge(2))
        .filter(user::ID.lt(6))
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Cmp::Gt, 0.into())
        .limit(2)
        .values(&["id"])
        .await
        .expect("filter_annotation composed with limit");
    assert_eq!(
        rows.len(),
        2,
        "limit(2) must return 2 rows from the FILTERED set, not fewer \
         (saw {rows:?})"
    );
    let passing_ids = [3_i64, 4_i64, 5_i64];
    for row in &rows {
        let id = row["id"].as_i64().expect("id");
        assert!(
            passing_ids.contains(&id),
            "row {id} must come from the passing set {passing_ids:?} — bob (id 2, 0 \
             comments) must never survive the filter just because LIMIT ran first"
        );
    }
}

/// Merge-gating review fix — `filter_annotation` traps the built statement's
/// ORDER BY inside a derived-table wrap unless it is re-applied to the
/// OUTER (post-filter) query. On Postgres a subquery's `ORDER BY` is not
/// guaranteed to survive into the enclosing query — SQLite happens to
/// preserve it, which is why a row-count/value assertion alone would not
/// catch a regression here (that's what the `to_sql_pg()` assertion below
/// is for: it inspects the OUTER wrapped statement directly, so it fails
/// the same way on either backend).
#[tokio::test]
async fn filter_annotation_plus_order_by_annotation_plus_limit_returns_the_correct_top_rows() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // gina (5 comments), hank (4), iris (2) all pass `> 0`; every other
    // passing user in the shared seed has count 1 or 3 — strictly less
    // than hank's 4 — so the top 2 by count DESC is unambiguously
    // [gina, hank], never a coincidental pair.
    let rows = User::objects()
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Cmp::Gt, 0.into())
        .order_by_annotation("posts__comments_count", true)
        .limit(2)
        .values(&["id", "name"])
        .await
        .expect("filter_annotation + order_by_annotation + limit");
    let names: Vec<&str> = rows
        .iter()
        .map(|r| r["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec!["gina", "hank"],
        "top 2 by posts__comments_count DESC must be [gina (5), hank (4)], \
         not an arbitrary 2 rows — saw {names:?}"
    );

    // Prove this is a genuine Postgres-path fix, not just a SQLite
    // coincidence: the ORDER BY must sit on the OUTER wrapped statement
    // (`SELECT * FROM (...) __anno ORDER BY ... LIMIT ...`), not trapped
    // inside the derived table. `to_sql_pg()` renders the same
    // `build_query_for` output through the Postgres dialect — the wrap
    // itself is not backend-conditional, only `FOR UPDATE SKIP LOCKED` is,
    // so this assertion is exactly as meaningful against either builder.
    let sql = User::objects()
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Cmp::Gt, 0.into())
        .order_by_annotation("posts__comments_count", true)
        .limit(2)
        .to_sql_pg();
    let anno_pos = sql
        .find("__anno")
        .expect("the filter_annotation wrap must produce a __anno derived table");
    let order_pos = sql
        .find("ORDER BY")
        .expect("the wrapped statement must carry an ORDER BY (outer, not lost)");
    assert!(
        order_pos > anno_pos,
        "ORDER BY must appear AFTER the __anno wrap (i.e. on the OUTER \
         statement), not trapped inside the derived table's subquery: {sql}"
    );
    assert_eq!(
        sql.matches("ORDER BY").count(),
        1,
        "exactly one ORDER BY — the inner's is cleared, not duplicated: {sql}"
    );
}

/// gaps6 #3 — `first()` used to set `LIMIT 1` directly on `self.query`,
/// bypassing the TRACKED `user_limit` the `filter_annotation` wrap re-applies
/// to the outer query. So `first()` after a `filter_annotation` silently lost
/// its LIMIT and fetched the whole filtered set before taking the first row.
#[tokio::test]
async fn filter_annotation_first_applies_limit_1_after_the_wrap() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    query_count_harness::reset();
    // alice (id 1) passes `posts__comments_count > 0`; bob (id 2) fails it.
    let row = User::objects()
        .filter(user::ID.lt(3))
        .annotate_count("posts__comments")
        .filter_annotation("posts__comments_count", Cmp::Gt, 0.into())
        .first()
        .await
        .expect("first after filter_annotation")
        .expect("alice must match");
    assert_eq!(
        row.name, "alice",
        "first() must return the single passing row"
    );

    let sql = query_count_harness::last_sql();
    let anno_pos = sql
        .find("__anno")
        .expect("first() must ride the filter_annotation wrap");
    // The bound value renders as `?` (sea_query_binder parameterizes it), not
    // the literal `1` — presence of a LIMIT clause at all on the outer query
    // is exactly what the bug dropped (the wrap only re-applied the TRACKED
    // `user_limit`, which `first()` never set).
    let limit_pos = sql.find("LIMIT ?").unwrap_or_else(|| {
        panic!("first() must carry a LIMIT on the OUTER (post-filter) query: {sql}")
    });
    assert!(
        limit_pos > anno_pos,
        "LIMIT must sit on the outer wrapped query, not be dropped: {sql}"
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
async fn annotate_sum_over_a_secret_leaf_column_is_refused() {
    let _g = query_count_harness::query_lock().await;
    boot().await;
    // `posts__password_hash` names a real column (`Post::password_hash`,
    // seeded as NULL on every post) that matches `HARD_DENIED_FIELDS` by
    // name alone — the same secrecy gate that keeps a hashed password out
    // of any serialized response. Security & Performance
    // (docs/specs/orm-heavy-relations-epic.md): a SUM/AVG/MIN/MAX must
    // never let a masked/secret column's plaintext leak out through the
    // aggregate, so this must be refused loudly — never silently emit
    // `SUM("password_hash")`.
    let err = User::objects()
        .annotate_sum("leak", "posts__password_hash")
        .fetch_annotated()
        .await
        .expect_err("aggregating a hard-denied/secret column must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("password_hash") && (msg.contains("secret") || msg.contains("masked")),
        "error names the offending column and explains why it's refused: {msg}"
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
    static LAST_SQL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
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
            *LAST_SQL.lock().unwrap() = stmt;
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
        LAST_SQL.lock().unwrap().clear();
    }

    pub fn count() -> usize {
        QUERY_COUNT.load(Ordering::SeqCst)
    }

    pub fn last_sql() -> String {
        LAST_SQL.lock().unwrap().clone()
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
