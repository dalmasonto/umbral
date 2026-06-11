//! Behavioral coverage for M2M form fields. The Form derive classifies
//! `M2M<T>` into a `ModelMultiChoice`: validate() parses the id list,
//! verifies each id exists (one batched query), and stages the ids on
//! the M2M field's pending slot. The typed create() flushes them to the
//! junction table after the parent insert. Tests read the junction
//! table directly and assert exactly the selected rows + atomicity.

#![allow(dead_code)]
use std::collections::HashMap;
use tokio::sync::OnceCell;
use umbra::forms::FormValidate;
use umbra::orm::Model;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fm_tag")]
struct Tag {
    pub id: i64,
    pub name: String,
}

#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbra::orm::Model,
    umbra::forms::Form,
)]
#[umbra(table = "fm_article")]
struct Article {
    #[umbra(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 200))]
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: umbra::orm::M2M<Tag>,
}

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// Multi-value submission: the form layer collapses repeated keys into a
// comma-joined string under the field name. Here we feed the joined form
// directly.
fn data_multi(title: &str, tag_ids: &[&str]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("title".to_string(), title.to_string());
    m.insert("tags".to_string(), tag_ids.join(","));
    m
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Article>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE fm_tag (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
            .execute(&pool).await.expect("create tag");
        sqlx::query("CREATE TABLE fm_article (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
            .execute(&pool).await.expect("create article");
        // Junction follows the <parent_table>_<field> convention.
        sqlx::query("CREATE TABLE fm_article_tags (parent_id INTEGER NOT NULL, child_id INTEGER NOT NULL, PRIMARY KEY (parent_id, child_id))")
            .execute(&pool).await.expect("create junction");
        for name in ["a", "b", "c"] {
            sqlx::query("INSERT INTO fm_tag (name) VALUES (?)")
                .bind(name)
                .execute(&pool).await.expect("seed tag");
        }
    })
    .await;
}

async fn junction_child_ids(parent_id: i64) -> Vec<i64> {
    let pool = db::pool();
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT child_id FROM fm_article_tags WHERE parent_id = ? ORDER BY child_id",
    )
    .bind(parent_id)
    .fetch_all(&pool)
    .await
    .expect("read junction");
    rows.into_iter().map(|(c,)| c).collect()
}

#[tokio::test]
async fn m2m_form_writes_exactly_the_selected_junction_rows() {
    boot().await;
    let article = Article::validate(&data_multi("Intro", &["1", "2"]))
        .await
        .expect("valid m2m");
    let created = Article::objects()
        .create(article)
        .await
        .expect("create article");
    let ids = junction_child_ids(created.id).await;
    assert_eq!(
        ids,
        vec![1, 2],
        "exactly the two selected tags are junction rows (3rd absent)"
    );
}

#[tokio::test]
async fn m2m_form_bad_id_writes_zero_junction_rows() {
    boot().await;
    // A single bad id fails validation BEFORE any insert runs. The
    // returned ValidationErrors never reaches create(), so the parent
    // (with this unique title) is never inserted and no junction row is
    // written. Asserting on the specific title avoids racing with the
    // other test's insert into the shared in-memory DB.
    let err = Article::validate(&data_multi("Broken-unique-title", &["1", "9999"]))
        .await
        .expect_err("bad id rejected");
    assert!(
        err.fields.contains_key("tags"),
        "error keyed to the m2m field"
    );
    // No parent row with this title exists — validation short-circuited
    // before create() ever ran.
    let count = Article::objects()
        .filter(article::TITLE.eq("Broken-unique-title"))
        .count()
        .await
        .expect("count by title");
    assert_eq!(count, 0, "no parent row inserted on a bad m2m id");
}
