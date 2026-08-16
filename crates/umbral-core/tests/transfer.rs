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
    // Exercises cross-backend coercion: SQLite stores bool as 0/1, Postgres as
    // a native boolean. Nullable so existing seeds that omit it stay valid.
    pub active: Option<bool>,
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

/// Umbral-shaped model with a STRIPPED FK field (`owner`, not `owner_id`) and an
/// M2M — the inspectdb-generated shape a `--map django` transfer reads into.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Note {
    pub id: i64,
    pub body: String,
    #[umbral(index)]
    pub owner: ForeignKey<Author>,
    #[sqlx(skip)]
    #[serde(skip)]
    pub labels: M2M<Tag>,
}

/// Self-referential FK: a row can point at a higher id (a forward reference),
/// which a per-row FK check rejects on insert in PK order.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Node {
    pub id: i64,
    pub name: String,
    #[umbral(index)]
    pub parent_id: Option<ForeignKey<Node>>,
}

/// Mutually-referential pair: `Org.lead -> Member` and `Member.org -> Org`.
/// Neither can be inserted first under per-row FK enforcement.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Org {
    pub id: i64,
    pub name: String,
    #[umbral(index)]
    pub lead_id: Option<ForeignKey<Member>>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Member {
    pub id: i64,
    pub name: String,
    #[umbral(index)]
    pub org_id: ForeignKey<Org>,
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
            .model::<Note>()
            .model::<Node>()
            .model::<Org>()
            .model::<Member>()
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
    sqlx::query("CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)")
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
    // Parallel workers open concurrent write transactions; SQLite serializes
    // writers, so a busy_timeout makes the loser wait instead of erroring.
    sqlx::query("PRAGMA busy_timeout = 5000")
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

/// Cross-backend: SQLite source -> Postgres target. Proves the backend-
/// dispatched read/write round-trips ids, a boolean (0/1 -> native bool), a FK,
/// and an M2M junction across the two engines. Runs only with
/// `UMBRAL_TEST_POSTGRES_URL` set to a writable Postgres.
#[tokio::test]
async fn transfer_sqlite_to_postgres_cross_backend() {
    let Ok(pg_url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping cross-backend test: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("xb_src.sqlite3")).await;
    create_schema(&source).await;

    let pg = umbral::db::connect(&pg_url).await.expect("connect pg");
    let umbral::db::DbPool::Postgres(pgpool) = &pg else {
        panic!("UMBRAL_TEST_POSTGRES_URL must be a Postgres URL");
    };
    for stmt in [
        "DROP TABLE IF EXISTS book_tags, book, tag, author, umbral_transfer_state CASCADE",
        "CREATE TABLE author (id BIGINT PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)",
        "CREATE TABLE tag (id BIGINT PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE book (id BIGINT PRIMARY KEY, title TEXT NOT NULL, \
         author_id BIGINT NOT NULL REFERENCES author(id))",
        "CREATE TABLE book_tags (\
         parent_id BIGINT NOT NULL REFERENCES book(id) ON DELETE CASCADE, \
         child_id BIGINT NOT NULL REFERENCES tag(id) ON DELETE CASCADE, \
         PRIMARY KEY (parent_id, child_id))",
    ] {
        sqlx::query(stmt).execute(pgpool).await.expect("pg ddl");
    }

    // Seed the SQLite source: non-contiguous ids, a boolean (as 0/1), M2M links.
    sqlx::query("INSERT INTO author (id, name, active) VALUES (5, 'Ada', 1), (9, 'Grace', 0)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tag (id, name) VALUES (100, 'rust'), (200, 'db')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book (id, title, author_id) VALUES (10, 'A', 5)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book_tags (parent_id, child_id) VALUES (10, 100), (10, 200)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let metas = vec![
        ModelMeta::for_::<Author>(),
        ModelMeta::for_::<Tag>(),
        ModelMeta::for_::<Book>(),
    ];
    let report = transfer(&src, &pg, metas, &TransferOptions::default())
        .await
        .expect("cross-backend transfer");
    assert!(report.rows >= 5, "authors+tags+book+links; got {report:?}");

    // PKs + boolean coerced onto native Postgres types.
    let authors: Vec<(i64, String, Option<bool>)> =
        sqlx::query_as("SELECT id, name, active FROM author ORDER BY id")
            .fetch_all(pgpool)
            .await
            .unwrap();
    assert_eq!(
        authors,
        vec![
            (5, "Ada".into(), Some(true)),
            (9, "Grace".into(), Some(false))
        ]
    );
    // FK preserved.
    let books: Vec<(i64, i64)> = sqlx::query_as("SELECT id, author_id FROM book")
        .fetch_all(pgpool)
        .await
        .unwrap();
    assert_eq!(books, vec![(10, 5)]);
    // M2M junction copied across backends.
    let links: Vec<(i64, i64)> =
        sqlx::query_as("SELECT parent_id, child_id FROM book_tags ORDER BY parent_id, child_id")
            .fetch_all(pgpool)
            .await
            .unwrap();
    assert_eq!(links, vec![(10, 100), (10, 200)]);
}

/// `--map django`: a Django-shaped source (FK column `owner_id`, junction
/// columns `note_id` / `tag_id`) is translated into the umbral target's names
/// (`owner`, `parent_id` / `child_id`) — the data half of the inspectdb port.
#[tokio::test]
async fn transfer_map_django_translates_columns() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("map_src.sqlite3")).await;
    let target = open(&dir.path().join("map_dst.sqlite3")).await;

    // Source = Django-shaped.
    for stmt in [
        "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)",
        "CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL, \
         owner_id INTEGER NOT NULL REFERENCES author(id))",
        "CREATE TABLE note_labels (\
         note_id INTEGER NOT NULL REFERENCES note(id), \
         tag_id INTEGER NOT NULL REFERENCES tag(id), PRIMARY KEY (note_id, tag_id))",
    ] {
        sqlx::query(stmt).execute(&source).await.unwrap();
    }
    // Target = umbral-shaped (the inspectdb-generated schema).
    for stmt in [
        "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)",
        "CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL, \
         owner INTEGER NOT NULL REFERENCES author(id))",
        "CREATE TABLE note_labels (\
         parent_id INTEGER NOT NULL REFERENCES note(id), \
         child_id INTEGER NOT NULL REFERENCES tag(id), PRIMARY KEY (parent_id, child_id))",
    ] {
        sqlx::query(stmt).execute(&target).await.unwrap();
    }

    sqlx::query("INSERT INTO author (id, name, active) VALUES (1, 'Ada', 1)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tag (id, name) VALUES (100, 'x')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO note (id, body, owner_id) VALUES (10, 'hi', 1)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO note_labels (note_id, tag_id) VALUES (10, 100)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let opts = TransferOptions {
        map: umbral::transfer::TransferMap::Django,
        ..Default::default()
    };
    let metas = vec![
        ModelMeta::for_::<Author>(),
        ModelMeta::for_::<Tag>(),
        ModelMeta::for_::<Note>(),
    ];
    transfer(&src, &dst, metas, &opts)
        .await
        .expect("map django transfer");

    // FK column translated owner_id -> owner.
    let notes: Vec<(i64, String, i64)> = sqlx::query_as("SELECT id, body, owner FROM note")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(notes, vec![(10, "hi".into(), 1)]);
    // Junction columns translated note_id/tag_id -> parent_id/child_id.
    let links: Vec<(i64, i64)> = sqlx::query_as("SELECT parent_id, child_id FROM note_labels")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(links, vec![(10, 100)]);
}

/// `--map prisma`: a camelCase source (FK column `ownerId`, junction columns
/// `noteId` / `tagId`) translates into the umbral target's snake names — the
/// genuinely-different convention from the Django family.
#[tokio::test]
async fn transfer_map_prisma_translates_camelcase_columns() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("prisma_src.sqlite3")).await;
    let target = open(&dir.path().join("prisma_dst.sqlite3")).await;

    // Source = Prisma-shaped: camelCase FK + junction columns.
    for stmt in [
        "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)",
        "CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL, \
         \"ownerId\" INTEGER NOT NULL REFERENCES author(id))",
        // Prisma implicit M2M: `_<A>To<B>` table (models sorted), `A`/`B` columns.
        "CREATE TABLE \"_NoteToTag\" (\
         \"A\" INTEGER NOT NULL REFERENCES note(id), \
         \"B\" INTEGER NOT NULL REFERENCES tag(id), PRIMARY KEY (\"A\", \"B\"))",
    ] {
        sqlx::query(stmt).execute(&source).await.unwrap();
    }
    // Target = umbral-shaped.
    for stmt in [
        "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN)",
        "CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL, \
         owner INTEGER NOT NULL REFERENCES author(id))",
        "CREATE TABLE note_labels (\
         parent_id INTEGER NOT NULL REFERENCES note(id), \
         child_id INTEGER NOT NULL REFERENCES tag(id), PRIMARY KEY (parent_id, child_id))",
    ] {
        sqlx::query(stmt).execute(&target).await.unwrap();
    }

    sqlx::query("INSERT INTO author (id, name, active) VALUES (1, 'Ada', 1)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tag (id, name) VALUES (100, 'x')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO note (id, body, \"ownerId\") VALUES (10, 'hi', 1)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO \"_NoteToTag\" (\"A\", \"B\") VALUES (10, 100)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let opts = TransferOptions {
        map: umbral::transfer::TransferMap::Prisma,
        ..Default::default()
    };
    transfer(
        &src,
        &dst,
        vec![
            ModelMeta::for_::<Author>(),
            ModelMeta::for_::<Tag>(),
            ModelMeta::for_::<Note>(),
        ],
        &opts,
    )
    .await
    .expect("map prisma transfer");

    let notes: Vec<(i64, String, i64)> = sqlx::query_as("SELECT id, body, owner FROM note")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(notes, vec![(10, "hi".into(), 1)]);
    let links: Vec<(i64, i64)> = sqlx::query_as("SELECT parent_id, child_id FROM note_labels")
        .fetch_all(&target)
        .await
        .unwrap();
    assert_eq!(links, vec![(10, 100)]);
}

/// Parallel workers copy correctly: independent tables (author, tag) run
/// concurrently in level 0, book in level 1, junctions last — with a small
/// batch so pagination interleaves under concurrency. Output must match the
/// sequential result exactly.
#[tokio::test]
async fn transfer_parallel_workers_copies_correctly() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("par_src.sqlite3")).await;
    let target = open(&dir.path().join("par_dst.sqlite3")).await;
    create_schema(&source).await;
    create_schema(&target).await;
    sqlx::query("INSERT INTO author (id, name) VALUES (1, 'A'), (2, 'B')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tag (id, name) VALUES (100, 'x'), (200, 'y')")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book (id, title, author_id) VALUES (10, 'A', 1), (20, 'B', 2)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO book_tags (parent_id, child_id) VALUES (10, 100), (20, 200)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    let opts = TransferOptions {
        workers: 4,
        batch_size: 1,
        ..Default::default()
    };
    let metas = vec![
        ModelMeta::for_::<Author>(),
        ModelMeta::for_::<Tag>(),
        ModelMeta::for_::<Book>(),
    ];
    let report = transfer(&src, &dst, metas, &opts)
        .await
        .expect("parallel transfer");
    assert_eq!(report.rows, 8, "2 authors + 2 tags + 2 books + 2 links");

    let counts: (i64, i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM author")
            .fetch_one(&target)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM book")
            .fetch_one(&target)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM book_tags")
            .fetch_one(&target)
            .await
            .unwrap(),
    );
    assert_eq!(counts, (2, 2, 2));
}

/// A self-referential FK with a FORWARD reference (a row whose parent has a
/// higher id) is copied under a single deferred transaction, so the FK check at
/// commit sees every row — a per-row check would reject it in PK order.
#[tokio::test]
async fn transfer_self_referential_forward_reference() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("selffk_src.sqlite3")).await;
    let target = open(&dir.path().join("selffk_dst.sqlite3")).await;
    for pool in [&source, &target] {
        sqlx::query(
            "CREATE TABLE node (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             parent_id INTEGER REFERENCES node(id))",
        )
        .execute(pool)
        .await
        .unwrap();
    }
    // Seed parent-first (satisfies FK on the source); the transfer copies in PK
    // order (1 before 3), which is where deferral matters.
    sqlx::query("INSERT INTO node (id, name, parent_id) VALUES (3, 'root', NULL)")
        .execute(&source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO node (id, name, parent_id) VALUES (1, 'child', 3)")
        .execute(&source)
        .await
        .unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    transfer(
        &src,
        &dst,
        vec![ModelMeta::for_::<Node>()],
        &TransferOptions::default(),
    )
    .await
    .expect("self-FK forward reference must transfer under deferral");

    let nodes: Vec<(i64, Option<i64>)> =
        sqlx::query_as("SELECT id, parent_id FROM node ORDER BY id")
            .fetch_all(&target)
            .await
            .unwrap();
    assert_eq!(nodes, vec![(1, Some(3)), (3, None)]);
}

/// A mutual FK cycle (`Org.lead -> Member`, `Member.org -> Org`) — neither side
/// is insertable first — copies as one deferred-FK transaction group.
#[tokio::test]
async fn transfer_mutual_fk_cycle() {
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("cycle_src.sqlite3")).await;
    let target = open(&dir.path().join("cycle_dst.sqlite3")).await;
    for pool in [&source, &target] {
        sqlx::query(
            "CREATE TABLE org (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             lead_id INTEGER REFERENCES member(id))",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE member (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             org_id INTEGER NOT NULL REFERENCES org(id))",
        )
        .execute(pool)
        .await
        .unwrap();
    }
    // Seed the cycle on the source inside a deferred transaction (it's a cycle
    // there too).
    let mut tx = source.begin().await.unwrap();
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO org (id, name, lead_id) VALUES (1, 'Acme', 10)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (id, name, org_id) VALUES (10, 'Ada', 1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    let dst = umbral::db::DbPool::Sqlite(target.clone());
    transfer(
        &src,
        &dst,
        vec![ModelMeta::for_::<Org>(), ModelMeta::for_::<Member>()],
        &TransferOptions::default(),
    )
    .await
    .expect("mutual FK cycle must transfer as a deferred group");

    let org: (i64, Option<i64>) = sqlx::query_as("SELECT id, lead_id FROM org")
        .fetch_one(&target)
        .await
        .unwrap();
    let member: (i64, i64) = sqlx::query_as("SELECT id, org_id FROM member")
        .fetch_one(&target)
        .await
        .unwrap();
    assert_eq!(org, (1, Some(10)));
    assert_eq!(member, (10, 1));
}

/// The mutual FK cycle across backends: SQLite source -> Postgres target, where
/// the deferred group copy issues `SET CONSTRAINTS ALL DEFERRED` — which only
/// works because umbral now emits FK constraints `DEFERRABLE`. Runs only with
/// `UMBRAL_TEST_POSTGRES_URL` set.
#[tokio::test]
async fn transfer_mutual_fk_cycle_to_postgres() {
    let Ok(pg_url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    boot().await;
    let dir = TempDir::new().unwrap();
    let source = open(&dir.path().join("cyclepg_src.sqlite3")).await;
    let pg = umbral::db::connect(&pg_url).await.expect("connect pg");
    let umbral::db::DbPool::Postgres(pgpool) = &pg else {
        panic!("UMBRAL_TEST_POSTGRES_URL must be a Postgres URL");
    };
    // Source schema (SQLite) + target schema (Postgres, cyclic FKs made
    // DEFERRABLE exactly as umbral's migration engine now emits them).
    sqlx::query("CREATE TABLE org (id INTEGER PRIMARY KEY, name TEXT NOT NULL, lead_id INTEGER REFERENCES member(id))")
        .execute(&source).await.unwrap();
    sqlx::query("CREATE TABLE member (id INTEGER PRIMARY KEY, name TEXT NOT NULL, org_id INTEGER NOT NULL REFERENCES org(id))")
        .execute(&source).await.unwrap();
    for stmt in [
        "DROP TABLE IF EXISTS member, org, umbral_transfer_state CASCADE",
        "CREATE TABLE org (id BIGINT PRIMARY KEY, name TEXT NOT NULL, lead_id BIGINT)",
        "CREATE TABLE member (id BIGINT PRIMARY KEY, name TEXT NOT NULL, org_id BIGINT NOT NULL)",
        "ALTER TABLE org ADD CONSTRAINT org_lead_fk FOREIGN KEY (lead_id) \
         REFERENCES member(id) DEFERRABLE INITIALLY IMMEDIATE",
        "ALTER TABLE member ADD CONSTRAINT member_org_fk FOREIGN KEY (org_id) \
         REFERENCES org(id) DEFERRABLE INITIALLY IMMEDIATE",
    ] {
        sqlx::query(stmt).execute(pgpool).await.expect("pg ddl");
    }

    // Seed the cycle on the source (deferred there too).
    let mut tx = source.begin().await.unwrap();
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO org (id, name, lead_id) VALUES (1, 'Acme', 10)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (id, name, org_id) VALUES (10, 'Ada', 1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let src = umbral::db::DbPool::Sqlite(source);
    transfer(
        &src,
        &pg,
        vec![ModelMeta::for_::<Org>(), ModelMeta::for_::<Member>()],
        &TransferOptions::default(),
    )
    .await
    .expect("cyclic transfer to postgres must defer FK checks to commit");

    let org: (i64, Option<i64>) = sqlx::query_as("SELECT id, lead_id FROM org")
        .fetch_one(pgpool)
        .await
        .unwrap();
    let member: (i64, i64) = sqlx::query_as("SELECT id, org_id FROM member")
        .fetch_one(pgpool)
        .await
        .unwrap();
    assert_eq!(org, (1, Some(10)));
    assert_eq!(member, (10, 1));
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
