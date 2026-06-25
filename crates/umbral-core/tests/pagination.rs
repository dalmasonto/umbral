//! features #65 — `Paginator`/`Page` for template-rendered list views.
//!
//! DB round-trip: a real table with 23 rows, paginated at per_page=10, plus
//! the empty-queryset edge, strict-vs-clamped out-of-range behavior, and the
//! template-facing serialized `page` context (including the elided range and
//! the `querystring_with` global) rendered through the engine.

use serde_json::Value as Json;
use sqlx::SqlitePool;
use umbral_core::db;
use umbral_core::pagination::{PageItem, PaginationError, Paginator};

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "pg_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
}

async fn pool_with(n: i64) -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite should always connect");
    sqlx::query(
        "CREATE TABLE pg_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE pg_post");
    for i in 1..=n {
        sqlx::query("INSERT INTO pg_post (title) VALUES (?)")
            .bind(format!("post-{i}"))
            .execute(&pool)
            .await
            .expect("insert seed");
    }
    pool
}

#[tokio::test]
async fn paginates_23_rows_into_3_pages() {
    let pool = pool_with(23).await;
    let paginator = Paginator::new(Post::objects().order_by(post::ID.asc()).on(&pool), 10);

    assert_eq!(paginator.count().await.unwrap(), 23);
    assert_eq!(paginator.num_pages().await.unwrap(), 3);

    let p1 = paginator.page(1).await.expect("page 1");
    assert_eq!(p1.object_list.len(), 10);
    assert_eq!(p1.object_list[0].title, "post-1");
    assert_eq!(p1.start_index(), 1);
    assert_eq!(p1.end_index(), 10);
    assert!(!p1.has_previous());
    assert!(p1.has_next());
    assert_eq!(p1.next_page_number(), Some(2));
    assert_eq!(p1.previous_page_number(), None);

    let p2 = paginator.page(2).await.expect("page 2");
    assert_eq!(p2.object_list.len(), 10);
    assert_eq!(p2.object_list[0].title, "post-11");
    assert_eq!(p2.start_index(), 11);
    assert_eq!(p2.end_index(), 20);

    let p3 = paginator.page(3).await.expect("page 3");
    assert_eq!(p3.object_list.len(), 3);
    assert_eq!(p3.object_list[0].title, "post-21");
    assert_eq!(p3.start_index(), 21);
    assert_eq!(p3.end_index(), 23);
    assert!(p3.has_previous());
    assert!(!p3.has_next());
    assert_eq!(p3.next_page_number(), None);
}

#[tokio::test]
async fn out_of_range_strict_errors_clamped_returns_last_page() {
    let pool = pool_with(23).await;
    let paginator = Paginator::new(Post::objects().order_by(post::ID.asc()).on(&pool), 10);

    // Strict: page 0 and page 4 both error.
    assert!(matches!(
        paginator.page(0).await,
        Err(PaginationError::InvalidPage { num_pages: 3, .. })
    ));
    assert!(matches!(
        paginator.page(99).await,
        Err(PaginationError::InvalidPage {
            requested: 99,
            num_pages: 3
        })
    ));

    // Clamped: page 99 -> last page (3), page 0 -> page 1.
    let last = paginator.page_clamped(99).await.expect("clamped high");
    assert_eq!(last.number, 3);
    assert_eq!(last.object_list.len(), 3);
    let first = paginator.page_clamped(0).await.expect("clamped low");
    assert_eq!(first.number, 1);
}

#[tokio::test]
async fn empty_queryset_has_one_empty_page() {
    let pool = pool_with(0).await;
    let paginator = Paginator::new(Post::objects().on(&pool), 10);

    assert_eq!(paginator.count().await.unwrap(), 0);
    assert_eq!(paginator.num_pages().await.unwrap(), 1);

    let p1 = paginator.page(1).await.expect("empty page 1");
    assert!(p1.object_list.is_empty());
    assert_eq!(p1.start_index(), 0);
    assert_eq!(p1.end_index(), 0);
    assert!(!p1.has_other_pages());
}

#[tokio::test]
async fn per_page_clamps_to_at_least_one() {
    let pool = pool_with(5).await;
    let paginator = Paginator::new(Post::objects().on(&pool), 0);
    assert_eq!(paginator.per_page(), 1);
    assert_eq!(paginator.num_pages().await.unwrap(), 5);
}

#[tokio::test]
async fn elided_page_range_windows_20_pages() {
    // 200 rows @ 10/page = 20 pages, on page 6 -> 1 … window … 20.
    let pool = pool_with(200).await;
    let paginator = Paginator::new(Post::objects().order_by(post::ID.asc()).on(&pool), 10);
    let page = paginator.page(6).await.expect("page 6");
    let range = page.elided_page_range(2, 1);

    // Must pin both ends and carry ellipsis markers around the window.
    assert_eq!(range.first(), Some(&PageItem::Number(1)));
    assert_eq!(range.last(), Some(&PageItem::Number(20)));
    assert!(range.contains(&PageItem::Ellipsis));
    assert!(range.contains(&PageItem::Number(6)));
    // Window around 6 is present.
    for n in 4..=8 {
        assert!(range.contains(&PageItem::Number(n)), "missing page {n}");
    }
}

#[tokio::test]
async fn serialized_page_context_renders_nav_via_engine() {
    let pool = pool_with(23).await;
    let paginator = Paginator::new(Post::objects().order_by(post::ID.asc()).on(&pool), 10);
    let page = paginator.page(2).await.expect("page 2");
    let ctx = page.context();

    // Serializable shape: the template-facing fields are all present.
    let json: Json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(json["number"], 2);
    assert_eq!(json["num_pages"], 3);
    assert_eq!(json["total_count"], 23);
    assert_eq!(json["has_next"], true);
    assert_eq!(json["has_previous"], true);
    assert_eq!(json["next_page_number"], 3);
    assert_eq!(json["previous_page_number"], 1);
    assert_eq!(json["start_index"], 11);
    assert_eq!(json["end_index"], 20);
    // page_range items carry `n`/`ellipsis`.
    assert!(json["page_range"].is_array());
    let item0 = &json["page_range"][0];
    assert!(item0.get("n").is_some());
    assert!(item0.get("ellipsis").is_some());

    // The querystring_with global rebuilds the page link preserving filters.
    // Register the same closure the core engine registers (it takes the
    // value as a minijinja Value so an int `next_page_number` flows in) and
    // render through it — proving the int->string coercion the nav relies on.
    let mut env = minijinja::Environment::new();
    env.add_function(
        "querystring_with",
        |current_query: String, key: String, value: minijinja::Value| -> String {
            umbral_core::pagination::querystring_with(&current_query, &key, &value.to_string())
        },
    );
    env.add_template(
        "t",
        r#"{{ querystring_with(base_query, "page", page.next_page_number) }}"#,
    )
    .unwrap();
    let rendered = env
        .get_template("t")
        .unwrap()
        .render(minijinja::context! { page => ctx, base_query => "sort=title&page=2" })
        .expect("render querystring_with");
    assert_eq!(rendered, "sort=title&page=3");
}
