//! Part 4 deep-join behavioral + SQL-shape tests.
//!
//! Every join type asserts the SQL keyword ALONGSIDE a row-set proof:
//! an orphan parent is DROPPED under INNER and KEPT (with a null/empty
//! relation) under LEFT — proven by the returned rows, not just the SQL
//! substring. Nested chains assert real three-level graph hydration
//! from ONE query. The harness copies `join_related.rs`'s App::builder +
//! raw-DDL in-memory SQLite setup (the sanctioned test-only raw-SQL
//! exception per CLAUDE.md).
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::ForeignKey;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dj_author")]
pub struct Author {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dj_plugin")]
pub struct Plugin {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
    // NOT NULL forward FK -> auto INNER under plain join_related.
    pub author: ForeignKey<Author>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dj_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    // NULLABLE forward FK -> auto LEFT under plain join_related; the
    // orphan comment (plugin = NULL) is the INNER/LEFT discriminator.
    pub plugin: Option<ForeignKey<Plugin>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Plugin>()
            .model::<Comment>()
            .build()
            .expect("App::build");
        for ddl in [
            "CREATE TABLE dj_author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE dj_plugin (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             author INTEGER NOT NULL REFERENCES dj_author(id))",
            "CREATE TABLE dj_comment (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL, \
             plugin INTEGER REFERENCES dj_plugin(id))",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        // author 1 = Ada ; plugin 1 -> author 1 ; comment 1 -> plugin 1
        // comment 2 -> plugin NULL (orphan).
        sqlx::query("INSERT INTO dj_author (name) VALUES ('Ada')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO dj_plugin (name, author) VALUES ('Cache', 1)")
            .execute(&pool)
            .await
            .unwrap();
        // plugin 2 -> author 999 (dangling): an inferred INNER drops it,
        // LEFT keeps it. The discriminator for the NOT NULL FK case in
        // `plain_join_infers_inner_for_not_null_fk` (Task 5). umbra's
        // connect_sqlite enables `PRAGMA foreign_keys=ON`, so the
        // dangling reference is written with enforcement toggled off for
        // this one INSERT (test-only setup — the sanctioned raw-SQL
        // exception). The row models "NOT NULL FK whose target is gone",
        // exactly what an INNER JOIN must drop.
        {
            let mut conn = pool.acquire().await.unwrap();
            // PRAGMA foreign_keys is per-connection and can't change
            // inside a transaction, so toggle it on a single pinned
            // connection around the dangling INSERT.
            sqlx::query("PRAGMA foreign_keys=OFF")
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("INSERT INTO dj_plugin (name, author) VALUES ('Orphaned', 999)")
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("PRAGMA foreign_keys=ON")
                .execute(&mut *conn)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO dj_comment (body, plugin) VALUES ('nice', 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO dj_comment (body, plugin) VALUES ('orphan', NULL)")
            .execute(&pool)
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn inner_join_drops_orphan_left_keeps_it() {
    boot().await;
    // INNER: the orphan comment (plugin NULL) must be dropped.
    let inner = Comment::objects()
        .inner_join_related("plugin")
        .fetch()
        .await
        .expect("inner fetch");
    let bodies: Vec<&str> = inner.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"nice"), "INNER keeps the matched row");
    assert!(
        !bodies.contains(&"orphan"),
        "INNER drops the orphan, got {bodies:?}"
    );
    // and the SQL says INNER JOIN.
    let sql = Comment::objects().inner_join_related("plugin").to_sql();
    assert!(sql.contains("INNER JOIN"), "expected INNER JOIN: {sql}");

    // LEFT: the orphan survives with an unresolved relation.
    let left = Comment::objects()
        .left_join_related("plugin")
        .fetch()
        .await
        .expect("left fetch");
    let lbodies: Vec<&str> = left.iter().map(|c| c.body.as_str()).collect();
    assert!(
        lbodies.contains(&"orphan"),
        "LEFT keeps the orphan, got {lbodies:?}"
    );
    let orphan = left.iter().find(|c| c.body == "orphan").unwrap();
    assert!(orphan.plugin.is_none(), "orphan's plugin relation is None");
    let lsql = Comment::objects().left_join_related("plugin").to_sql();
    assert!(lsql.contains("LEFT JOIN"), "expected LEFT JOIN: {lsql}");
}
