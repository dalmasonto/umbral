//! gaps2 #38: column predicate constants are reachable BOTH as
//! `module::COL` (the historical form) AND as `Model::COL` (an associated
//! const on the struct), so a filter can be written `Doc::TITLE.eq(...)`
//! without importing the column module — the qualified form the gap asked
//! for. Real rows in, the filtered row read back.
use tokio::sync::OnceCell;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "mqc_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub views: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Doc>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE mqc_doc (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, views INTEGER NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO mqc_doc (title, views) VALUES ('hello', 10)")
            .execute(&pool).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn model_qualified_column_const_filters() {
    boot().await;
    // The point of #38: the `Model::COL` associated-const form works.
    let found = Doc::objects()
        .filter(Doc::TITLE.eq("hello"))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(found.title, "hello");

    // A second column via the qualified form (numeric predicate).
    let n = Doc::objects()
        .filter(Doc::VIEWS.eq(10))
        .count()
        .await
        .expect("count");
    assert_eq!(n, 1);

    // The historical module form (`doc::TITLE`, snake of the struct name)
    // still works — no regression.
    let still = Doc::objects()
        .filter(doc::TITLE.eq("hello"))
        .count()
        .await
        .expect("count");
    assert_eq!(still, 1);
}
