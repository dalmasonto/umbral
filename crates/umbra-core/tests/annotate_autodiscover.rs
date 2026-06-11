//! Auto-discovered reverse-FK annotations (gaps2 #45) — Django's
//! `Parent.objects.annotate(Count("child"))` working with NO
//! `child_set: ReverseSet<Child>` field declared on the parent.
//!
//! The resolver scans the model registry for any child whose FK points
//! back at the parent's table, matches the caller's relation string
//! against the conventional name forms (table / snake_case struct /
//! `_set`-suffixed), and feeds the same correlated-subquery machinery
//! the declared path uses. Every aggregate kind (count/sum/avg/min/max)
//! works over the discovered relation; soft-deleted children are
//! excluded; a parent with two candidate children is an ambiguity error
//! that names the candidates and points at the `reverse_fk` escape
//! hatch.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{Aggregate, ForeignKey};
use umbra_core::db;

// Parent with NO ReverseSet field declared. Auto-discovery must find
// its children purely from the children's FK graph.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_blog")]
pub struct Blog {
    pub id: i64,
    pub title: String,
}

// Single child of Blog with a numeric column to aggregate.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_entry")]
pub struct Entry {
    pub id: i64,
    pub headline: String,
    pub amount: i64,
    pub blog: ForeignKey<Blog>,
}

// Soft-delete child of Blog (separate parent table to keep state clean).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_shelf")]
pub struct Shelf {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_book", soft_delete)]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub shelf: ForeignKey<Shelf>,
    #[sqlx(default)]
    #[umbra(index)]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

// Ambiguity setup: a single child model with TWO FK columns pointing
// back at the SAME parent. The child's conventional name (`ad_order` /
// `order` / `order_set`) matches, but there are two (child, fk_column)
// candidates — so it can't be resolved without `reverse_fk`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_store")]
pub struct Store {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "ad_order")]
pub struct Order {
    pub id: i64,
    pub buyer_store: ForeignKey<Store>,
    pub seller_store: ForeignKey<Store>,
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
            .model::<Blog>()
            .model::<Entry>()
            .model::<Shelf>()
            .model::<Book>()
            .model::<Store>()
            .model::<Order>()
            .build()
            .expect("App::build");

        for ddl in [
            "CREATE TABLE ad_blog (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
            "CREATE TABLE ad_entry (id INTEGER PRIMARY KEY AUTOINCREMENT, headline TEXT NOT NULL, amount INTEGER NOT NULL, blog INTEGER NOT NULL REFERENCES ad_blog(id))",
            "CREATE TABLE ad_shelf (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE ad_book (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, shelf INTEGER NOT NULL REFERENCES ad_shelf(id), deleted_at TEXT)",
            "CREATE TABLE ad_store (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE ad_order (id INTEGER PRIMARY KEY AUTOINCREMENT, buyer_store INTEGER NOT NULL REFERENCES ad_store(id), seller_store INTEGER NOT NULL REFERENCES ad_store(id))",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }

        // Blogs: alpha (id 1) gets three entries amounts [10,20,30];
        // beta (id 2) gets none.
        for title in ["alpha", "beta"] {
            sqlx::query("INSERT INTO ad_blog (title) VALUES (?)")
                .bind(title)
                .execute(&pool)
                .await
                .expect("seed blog");
        }
        for (headline, amount, blog) in [("h1", 10_i64, 1), ("h2", 20, 1), ("h3", 30, 1)] {
            sqlx::query("INSERT INTO ad_entry (headline, amount, blog) VALUES (?, ?, ?)")
                .bind(headline)
                .bind(amount)
                .bind(blog)
                .execute(&pool)
                .await
                .expect("seed entry");
        }

        // Shelf (id 1) gets three books; one will be soft-deleted.
        sqlx::query("INSERT INTO ad_shelf (name) VALUES ('s1')")
            .execute(&pool)
            .await
            .expect("seed shelf");
        for title in ["b1", "b2", "b3"] {
            sqlx::query("INSERT INTO ad_book (title, shelf) VALUES (?, 1)")
                .bind(title)
                .execute(&pool)
                .await
                .expect("seed book");
        }

        // Store (id 1) with an order — two FK columns make the
        // auto-discovered relation ambiguous.
        sqlx::query("INSERT INTO ad_store (name) VALUES ('store1')")
            .execute(&pool)
            .await
            .expect("seed store");
        sqlx::query("INSERT INTO ad_order (buyer_store, seller_store) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed order");
    })
    .await;
}

#[tokio::test]
async fn auto_discovers_count_by_table_name() {
    boot().await;
    // "ad_entry" is the child's table name; no ReverseSet declared.
    let rows = Blog::objects()
        .annotate_count("ad_entry")
        .fetch_annotated()
        .await
        .expect("auto-discovered count by table name");
    let by_title: std::collections::HashMap<String, i64> = rows
        .into_iter()
        .map(|(b, a)| (b.title, a["ad_entry_count"].as_i64().unwrap()))
        .collect();
    assert_eq!(by_title["alpha"], 3, "three entries on alpha");
    assert_eq!(
        by_title["beta"], 0,
        "a blog with zero entries is still returned as 0"
    );
}

#[tokio::test]
async fn auto_discovers_count_by_snake_struct_and_set_suffix() {
    boot().await;
    // "entry" (snake struct), "entry_set" (Django <model>_set) both
    // resolve to the same child.
    for rel in ["entry", "entry_set"] {
        let rows = Blog::objects()
            .filter(blog::TITLE.eq("alpha"))
            .annotate_count(rel)
            .fetch_annotated()
            .await
            .unwrap_or_else(|e| panic!("auto-discover by `{rel}` failed: {e}"));
        assert_eq!(rows.len(), 1);
        let alias = format!("{rel}_count");
        assert_eq!(
            rows[0].1[&alias].as_i64(),
            Some(3),
            "alpha has 3 entries via `{rel}`"
        );
    }
}

#[tokio::test]
async fn auto_discovered_sum_avg_min_max() {
    boot().await;
    // amounts on alpha = [10, 20, 30] → sum 60, avg 20, min 10, max 30.
    let rows = Blog::objects()
        .filter(blog::TITLE.eq("alpha"))
        .annotate_related("s", "ad_entry", Aggregate::sum("amount"))
        .annotate_related("a", "ad_entry", Aggregate::avg("amount"))
        .annotate_related("mn", "ad_entry", Aggregate::min("amount"))
        .annotate_related("mx", "ad_entry", Aggregate::max("amount"))
        .fetch_annotated()
        .await
        .expect("all aggregate kinds over an auto-discovered relation");
    assert_eq!(rows.len(), 1);
    let a = &rows[0].1;
    assert_eq!(a["s"].as_i64(), Some(60), "SUM(amount)");
    assert_eq!(a["a"].as_f64(), Some(20.0), "AVG(amount)");
    assert_eq!(a["mn"].as_i64(), Some(10), "MIN(amount)");
    assert_eq!(a["mx"].as_i64(), Some(30), "MAX(amount)");
}

#[tokio::test]
async fn auto_discovered_count_excludes_soft_deleted() {
    boot().await;
    // Soft-delete one of the three books via the REAL delete path.
    let removed = Book::objects()
        .filter(book::TITLE.eq("b2"))
        .delete()
        .await
        .expect("soft-delete one book");
    assert_eq!(removed, 1);

    let rows = Shelf::objects()
        .annotate_count("ad_book")
        .fetch_annotated()
        .await
        .expect("auto-discovered count honoring soft-delete");
    let by_name: std::collections::HashMap<String, i64> = rows
        .into_iter()
        .map(|(s, a)| (s.name, a["ad_book_count"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        by_name["s1"], 2,
        "soft-deleted book is excluded from the auto-discovered count"
    );
}

#[tokio::test]
async fn ambiguous_reverse_relation_errors() {
    boot().await;
    // "ad_order" matches a single child that has TWO FKs to ad_store —
    // two (child, fk_column) candidates, so it's ambiguous.
    let err = Store::objects()
        .annotate_count("ad_order")
        .fetch_annotated()
        .await
        .expect_err("ambiguous reverse relation must fail at fetch time");
    let msg = err.to_string();
    assert!(
        msg.contains("ambiguous") && msg.contains("declare"),
        "error teaches disambiguation: {msg}"
    );
}

#[tokio::test]
async fn unknown_relation_lists_autodiscoverable_children() {
    boot().await;
    let err = Blog::objects()
        .annotate_count("nonsense")
        .fetch_annotated()
        .await
        .expect_err("unknown relation must fail");
    let msg = err.to_string();
    assert!(msg.contains("nonsense"), "names the bad relation: {msg}");
    assert!(
        msg.contains("ad_entry"),
        "lists the auto-discoverable child so the user learns the name: {msg}"
    );
}
