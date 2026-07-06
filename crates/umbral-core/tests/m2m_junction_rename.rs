#![allow(dead_code, private_interfaces)]

//! gaps.md #93 — renaming a model that owns an `M2M<T>` field must RENAME its
//! junction table, not drop + recreate it. This is the end-to-end proof that
//! the relationship rows SURVIVE the rename (the whole point of the fix):
//!
//! 1. `make`/`migrate` create `mjr_article`, `mjr_tag`, and the junction
//!    `mjr_article_tags`. Real rows go in, including a junction row linking an
//!    article to a tag.
//! 2. The developer renames `Article`'s table `mjr_article` → `mjr_post`.
//!    `diff` emits `RenameTable` for BOTH the parent and the junction
//!    (`mjr_article_tags` → `mjr_post_tags`) — no `DropM2MTable`/`CreateM2MTable`.
//! 3. Applying that migration leaves the junction row intact under the new name.

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::M2M;

use umbral::migrate::{MigrationFile, Operation, Snapshot, diff, make_in, run_in};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mjr_tag")]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mjr_article")]
struct Article {
    id: i64,
    title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "mjr_tag")]
    tags: M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings load in test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Tag>()
            .model::<Article>()
            .build()
            .expect("App::build happy path");
    })
    .await;
}

fn sqlite_pool() -> sqlx::SqlitePool {
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p.clone(),
        umbral::db::DbPool::Postgres(_) => unreachable!("test pool is sqlite"),
    }
}

async fn table_exists(pool: &sqlx::SqlitePool, name: &str) -> bool {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?")
            .bind(name)
            .fetch_one(pool)
            .await
            .expect("query sqlite_master");
    n == 1
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_model_renames_its_m2m_junction_and_keeps_the_rows() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // 1) Create the initial schema (mjr_article, mjr_tag, mjr_article_tags).
    make_in(dir).await.expect("make 0001");
    run_in(dir).await.expect("apply 0001");

    let pool = sqlite_pool();
    assert!(
        table_exists(&pool, "mjr_article_tags").await,
        "the junction table was created"
    );

    // Real rows, including a relationship row linking article 1 ↔ tag 1.
    sqlx::query("INSERT INTO mjr_article (id, title) VALUES (1, 'hello')")
        .execute(&pool)
        .await
        .expect("insert article");
    sqlx::query("INSERT INTO mjr_tag (id, name) VALUES (1, 'rust')")
        .execute(&pool)
        .await
        .expect("insert tag");
    sqlx::query("INSERT INTO mjr_article_tags (parent_id, child_id) VALUES (1, 1)")
        .execute(&pool)
        .await
        .expect("insert junction row");

    // 2) Rename Article's table mjr_article → mjr_post. The registry is fixed
    //    at boot, so model the rename as a snapshot delta: previous = current
    //    registry state; target = the same models with Article's table renamed.
    let previous = Snapshot::current();
    let mut target = previous.clone();
    target
        .models
        .iter_mut()
        .find(|m| m.name == "Article")
        .expect("Article model in snapshot")
        .table = "mjr_post".to_string();

    let ops = diff(&previous, &target).expect("diff");
    // The junction is RENAMED, not dropped+created.
    assert!(
        ops.iter()
            .any(|op| matches!(op, Operation::RenameTable { from, to }
            if from == "mjr_article_tags" && to == "mjr_post_tags")),
        "junction must be renamed; ops: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(
            op,
            Operation::DropM2MTable { .. } | Operation::CreateM2MTable { .. }
        )),
        "junction must not be dropped/recreated; ops: {ops:?}"
    );

    // 3) Write + apply the rename migration.
    let migration = MigrationFile {
        id: "0002_rename_article_to_post".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: ops,
        snapshot_after: target,
        replaces: Vec::new(),
    };
    std::fs::write(
        dir.join("app").join("0002_rename_article_to_post.json"),
        serde_json::to_string_pretty(&migration).expect("serialize"),
    )
    .expect("write 0002");
    run_in(dir).await.expect("apply the rename migration");

    // The junction moved to its new name, and the relationship row SURVIVED.
    assert!(
        !table_exists(&pool, "mjr_article_tags").await,
        "the old junction name is gone"
    );
    assert!(
        table_exists(&pool, "mjr_post_tags").await,
        "the junction now lives under the new name"
    );
    let (parent_id, child_id): (i64, i64) =
        sqlx::query_as("SELECT parent_id, child_id FROM mjr_post_tags")
            .fetch_one(&pool)
            .await
            .expect("the relationship row survived the rename");
    assert_eq!(
        (parent_id, child_id),
        (1, 1),
        "the article↔tag link is intact under the renamed junction"
    );

    eprintln!("renaming_a_model_renames_its_m2m_junction_and_keeps_the_rows: PASS");
}
