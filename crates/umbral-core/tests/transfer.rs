//! Data-transfer engine: PK-preserving, FK-ordered, resumable copy between two
//! umbral SQLite databases. See `crates/umbral-core/src/transfer.rs`.

use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbral::migrate::ModelMeta;
use umbral::orm::{ForeignKey, M2M};
use umbral::transfer::{TransferOptions, fk_topo_order, transfer};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Author {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Tag {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Book {
    pub id: i64,
    pub title: String,
    #[umbral(index)]
    pub author_id: ForeignKey<Author>,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let pool = umbral::db::connect_sqlite("sqlite::memory:").await.unwrap();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Author>()
            .model::<Tag>()
            .model::<Book>()
            .build()
            .unwrap();
    })
    .await;
}

/// Create the `author` / `book` schema on a pool, FK enforcement ON so the
/// transfer's parent-before-child ordering is genuinely tested.
async fn create_schema(pool: &SqlitePool) {
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE book (id INTEGER PRIMARY KEY, title TEXT NOT NULL, \
         author_id INTEGER NOT NULL REFERENCES author(id))",
    )
    .execute(pool)
    .await
    .unwrap();
    // The M2M junction umbral auto-generates for `Book.tags: M2M<Tag>`.
    sqlx::query(
        "CREATE TABLE book_tags (\
         parent_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE, \
         child_id INTEGER NOT NULL REFERENCES tag(id) ON DELETE CASCADE, \
         PRIMARY KEY (parent_id, child_id))",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn open(path: &std::path::Path) -> SqlitePool {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = umbral::db::connect_sqlite(&url).await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

/// The engine's headline guarantee: rows land on the target with their SOURCE
/// primary keys AND foreign keys intact, and children are copied after parents
/// (FK enforcement on the target would reject them otherwise).
#[tokio::test]
async fn transfer_preserves_pks_and_fks_across_two_databases() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("src.sqlite3")).await;
    let target = open(&dir.path().join("dst.sqlite3")).await;
    create_schema(&source).await;
    create_schema(&target).await;

    // Non-contiguous ids so a re-numbering (autoincrement) bug would show.
    sqlx::query("INSERT INTO author (id, name) VALUES (5, 'Ada'), (9, 'Grace')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book (id, title, author_id) VALUES (100, 'A', 5), (200, 'B', 9)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let metas = vec![ModelMeta::for_::<Book>(), ModelMeta::for_::<Author>()];
    let report = transfer(&src, &dst, metas, &TransferOptions::default())
        .await
        .expect("transfer");

    assert_eq!(report.rows, 4, "2 authors + 2 books copied; got {report:?}");

    // Exact ids preserved on the target.
    let authors: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM author ORDER BY id")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(authors, vec![(5, "Ada".into()), (9, "Grace".into())]);

    // FK links intact: book 100 -> author 5, book 200 -> author 9.
    let books: Vec<(i64, i64)> = sqlx::query_as("SELECT id, author_id FROM book ORDER BY id")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(books, vec![(100, 5), (200, 9)]);
}

/// M2M junction rows copy too, AFTER both endpoint tables, with `(parent_id,
/// child_id)` preserved — so `book.tags` links survive the move.
#[tokio::test]
async fn transfer_copies_m2m_junction_rows() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("src_m2m.sqlite3")).await;
    let target = open(&dir.path().join("dst_m2m.sqlite3")).await;
    create_schema(&source).await;
    create_schema(&target).await;

    sqlx::query("INSERT INTO author (id, name) VALUES (1, 'Ada')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tag (id, name) VALUES (100, 'rust'), (200, 'db')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book (id, title, author_id) VALUES (10, 'A', 1)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book_tags (parent_id, child_id) VALUES (10, 100), (10, 200)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let metas = vec![
        ModelMeta::for_::<Author>(),
        ModelMeta::for_::<Tag>(),
        ModelMeta::for_::<Book>(),
    ];
    transfer(&src, &dst, metas, &TransferOptions::default())
        .await
        .expect("transfer");

    // The junction links copied verbatim (book 10 -> tags 100, 200).
    let links: Vec<(i64, i64)> =
        sqlx::query_as("SELECT parent_id, child_id FROM book_tags ORDER BY parent_id, child_id")
            .fetch_all(&target)
            .await
            .unwrap();
    assert_eq!(links, vec![(10, 100), (10, 200)]);
}

/// FK-topological ordering: a child never precedes its parent.
#[tokio::test]
async fn fk_topo_order_places_parents_first() {
    let ordered = fk_topo_order(vec![ModelMeta::for_::<Book>(), ModelMeta::for_::<Author>()]);
    let tables: Vec<&str> = ordered.iter().map(|m| m.table.as_str()).collect();
    let author = tables.iter().position(|t| *t == "author").unwrap();
    let book = tables.iter().position(|t| *t == "book").unwrap();
    assert!(author < book, "author must precede book; got {tables:?}");
}

/// Resumability: a second run over an already-transferred target is a no-op
/// (checkpoint marks each table done) — no duplicate rows, no PK conflicts.
#[tokio::test]
async fn transfer_is_idempotent_on_rerun() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("src2.sqlite3")).await;
    let target = open(&dir.path().join("dst2.sqlite3")).await;
    create_schema(&source).await;
    create_schema(&target).await;
    sqlx::query("INSERT INTO author (id, name) VALUES (1, 'Solo')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book (id, title, author_id) VALUES (7, 'Only', 1)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let metas = || vec![ModelMeta::for_::<Author>(), ModelMeta::for_::<Book>()];

    let first = transfer(&src, &dst, metas(), &TransferOptions::default())
        .await
        .unwrap();
    assert_eq!(first.rows, 2);

    // Re-run: every table is already `done`, so nothing is copied and no
    // duplicate/PK-conflict error is raised.
    let second = transfer(&src, &dst, metas(), &TransferOptions::default())
        .await
        .expect("re-run must succeed");
    assert_eq!(second.rows, 0, "re-run copies nothing; got {second:?}");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book")
        .fetch_one(&target)
        .await
        .unwrap();
    assert_eq!(count, 1, "no duplicate rows after re-run");
}
