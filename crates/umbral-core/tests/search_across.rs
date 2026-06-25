//! Behavioral coverage for Search::across on SQLite: real rows in, the
//! ranked SearchHit list out, read back through the public API.
use tokio::sync::OnceCell;
use umbral_core::db;
use umbral_core::orm::{Search, Searchable}; // core path: Task 4 (facade re-export) runs later

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "sa_plugin")]
pub struct Plugin {
    pub id: i64,
    pub name: String,
    pub blurb: String,
}
impl Searchable for Plugin {
    fn kind() -> &'static str {
        "plugin"
    }
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "sa_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
}
impl Searchable for Post {
    fn kind() -> &'static str {
        "post"
    }
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "sa_note")]
pub struct Note {
    pub id: i64,
    pub heading: String,
    pub state: String,
}
impl Searchable for Note {
    fn kind() -> &'static str {
        "note"
    }
    // Only "live" notes are searchable — the visibility-scope filter.
    fn filter_sql() -> Option<&'static str> {
        Some("state = 'live'")
    }
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Plugin>()
            .model::<Post>()
            .model::<Note>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE sa_plugin (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, blurb TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE sa_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, body TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_plugin (name, blurb) VALUES ('Redis cache', 'fast in-memory store')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_plugin (name, blurb) VALUES ('Logger', 'writes to redis sometimes')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_post (title, body) VALUES ('Using redis', 'a guide to caching')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_post (title, body) VALUES ('50% off coupon', 'limited time')")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE sa_note (id INTEGER PRIMARY KEY AUTOINCREMENT, heading TEXT NOT NULL, state TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_note (heading, state) VALUES ('Alpha live entry', 'live')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sa_note (heading, state) VALUES ('Alpha draft entry', 'draft')")
            .execute(&pool).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn across_returns_both_models_ranked_with_title_first() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("redis", 10)
        .await
        .expect("search runs");
    // Both a plugin and a post match.
    assert!(
        hits.iter().any(|h| h.kind == "plugin"),
        "a plugin hit: {hits:?}"
    );
    assert!(
        hits.iter().any(|h| h.kind == "post"),
        "a post hit: {hits:?}"
    );
    // The title matches ("Redis cache", "Using redis") outrank the body-only
    // match ("Logger" / "writes to redis").
    let top = &hits[0];
    assert!(
        (top.kind == "plugin" && top.title == "Redis cache")
            || (top.kind == "post" && top.title == "Using redis"),
        "a title match ranks first, got {top:?}"
    );
    let logger = hits.iter().find(|h| h.title == "Logger");
    if let Some(l) = logger {
        assert!(
            l.rank <= top.rank,
            "body-only match ranks no higher than a title match"
        );
    }
}

#[tokio::test]
async fn across_maps_kind_and_pk_back_to_rows() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("caching", 10)
        .await
        .expect("search runs");
    let post = hits
        .iter()
        .find(|h| h.kind == "post")
        .expect("post matched 'caching'");
    assert_eq!(post.pk, "1", "pk is the post's id as text");
    assert_eq!(post.title, "Using redis");
}

#[tokio::test]
async fn blank_query_returns_empty_without_hitting_db() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("   ", 10)
        .await
        .expect("blank is ok");
    assert!(hits.is_empty(), "blank query yields no hits");
}

#[tokio::test]
async fn no_match_returns_empty() {
    boot().await;
    let hits = Search::across::<(Plugin, Post)>("zzznomatch", 10)
        .await
        .expect("runs");
    assert!(hits.is_empty(), "no rows match");
}

#[tokio::test]
async fn filter_sql_restricts_searchable_rows() {
    boot().await;
    // Both notes' headings contain "Alpha", but `filter_sql` scopes Note search
    // to state='live', so the draft must NOT surface — the visibility guard the
    // old hand-written render_search applied (approved/published) now lives in
    // the Searchable contract.
    let hits = Search::across::<(Note,)>("Alpha", 10).await.expect("runs");
    assert!(
        hits.iter().any(|h| h.title == "Alpha live entry"),
        "the live note is searchable: {hits:?}"
    );
    assert!(
        !hits.iter().any(|h| h.title == "Alpha draft entry"),
        "the draft note is filtered out by filter_sql: {hits:?}"
    );
}

#[tokio::test]
async fn like_metacharacters_in_query_match_literally() {
    boot().await;
    // The query's `%` is escaped (`escape_like` + `ESCAPE '\'`), so "50%"
    // matches the row literally containing "50%", not as a trailing wildcard.
    // Without the ESCAPE clause the escaped pattern is inert and this returns
    // nothing — this test is the regression guard for that.
    let hits = Search::across::<(Plugin, Post)>("50%", 10)
        .await
        .expect("runs");
    assert!(
        hits.iter().any(|h| h.title == "50% off coupon"),
        "a literal-% query matches the row containing '50%': {hits:?}"
    );
}
