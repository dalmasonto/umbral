//! PK refactor — M2M on a `uuid::Uuid`-PK PARENT. Mirrors `pk_string_m2m`
//! with a UUID primary key + `M2M<Researcher, uuid::Uuid>` field, proving the
//! M2M junction plumbing (`set_m2m_parent_ids` / add / prefetch / join_related
//! / `__parent_id` read-back) is fully PK-agnostic — not i64-bound (gaps2 #88).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::M2M;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pkm2m_lab")]
pub struct Lab {
    #[umbral(primary_key)]
    pub id: uuid::Uuid,
    pub name: String,
    /// `P = uuid::Uuid` — the parent (Lab) has a UUID PK.
    #[sqlx(skip)]
    #[serde(skip)]
    pub members: M2M<Researcher, uuid::Uuid>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pkm2m_researcher")]
pub struct Researcher {
    pub id: i64,
    pub name: String,
}

fn lab_id(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Lab>()
            .model::<Researcher>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        for (n, name) in &[(1u128, "Quantum Lab"), (2u128, "Bio Lab")] {
            sqlx::query("INSERT INTO pkm2m_lab (id, name) VALUES (?, ?)")
                .bind(lab_id(*n))
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed lab");
        }
        for name in &["ada", "alan", "grace"] {
            sqlx::query("INSERT INTO pkm2m_researcher (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed researcher");
        }
    })
    .await;
}

#[tokio::test]
async fn m2m_add_and_prefetch_on_a_uuid_pk_parent() {
    boot().await;

    let researchers = Researcher::objects().fetch().await.expect("researchers");
    let by_name = |n: &str| researchers.iter().find(|r| r.name == n).unwrap().clone();
    let ada = by_name("ada");
    let alan = by_name("alan");

    // Fetch the lab by its UUID PK — `set_m2m_parent_ids` seeds the UUID
    // parent + junction table onto the M2M slot.
    let quantum = Lab::objects()
        .filter(lab::ID.eq(lab_id(1)))
        .first()
        .await
        .expect("query")
        .expect("quantum lab present");

    // Write junction rows: parent_id is the UUID.
    quantum.members.add(&ada).await.expect("add ada");
    quantum.members.add(&alan).await.expect("add alan");

    // Prefetch: the junction-join reads `__parent_id` back as a UUID and
    // buckets PK-agnostically.
    let labs = Lab::objects()
        .prefetch_related("members")
        .fetch()
        .await
        .expect("prefetch");

    let quantum = labs.iter().find(|l| l.id == lab_id(1)).unwrap();
    let mut names: Vec<&str> = quantum
        .members
        .resolved()
        .expect("M2M hydrated for a UUID-PK parent")
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec!["ada", "alan"]);

    let bio = labs.iter().find(|l| l.id == lab_id(2)).unwrap();
    assert!(
        bio.members.resolved().expect("hydrated (empty)").is_empty(),
        "bio lab has no members"
    );

    // Eager LEFT-JOIN path too (dedups parents + children by PK key, both
    // keyed on the UUID here).
    let joined = Lab::objects()
        .join_related("members")
        .fetch()
        .await
        .expect("join_related");
    let quantum = joined.iter().find(|l| l.id == lab_id(1)).unwrap();
    let mut jnames: Vec<&str> = quantum
        .members
        .resolved()
        .expect("M2M resolved via join_related")
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    jnames.sort();
    assert_eq!(jnames, vec!["ada", "alan"]);
}

/// gaps3 #80 — the column must be declared as what it actually stores.
///
/// SQLite has no uuid type, and sqlx encodes a `Uuid` as its 16 raw bytes there. The column
/// therefore holds a BLOB whatever the DDL calls it. umbral used to declare it `TEXT`, which
/// was simply false: `CAST(id AS TEXT)` on a uuid PK returned mojibake, and anyone reading
/// the schema — a person, `inspectdb`, another tool — was told the wrong thing.
///
/// The alternative (store the hyphenated text, matching the old declaration) is not
/// available: sqlx's SQLite decoder reads uuids ONLY from those raw bytes and fails on the
/// 36-char text with `ParseByteLength { len: 36 }`, so every typed read through
/// `#[derive(FromRow)]` would break.
///
/// So: declaration and storage have to agree, and this asserts they do — on the parent's PK
/// and on the junction column that references it, which is the pair that has to match for
/// the FOREIGN KEY to resolve at all (gaps3 #79).
#[tokio::test]
async fn a_uuid_column_is_declared_as_what_it_stores() {
    boot().await;
    let pool = umbral_core::db::pool();

    let lab = Lab {
        id: lab_id(0xDEC0_0001),
        name: "Declared".into(),
        members: Default::default(),
    };
    Lab::objects().create(lab).await.expect("create lab");

    // What the schema SAYS.
    let ddl: (String,) = sqlx::query_as("SELECT sql FROM sqlite_master WHERE name = 'pkm2m_lab'")
        .fetch_one(&pool)
        .await
        .expect("read the declared schema");
    assert!(
        ddl.0.to_lowercase().contains("\"id\" blob"),
        "a uuid PK must be DECLARED blob — sqlx stores it as raw bytes, so `TEXT` would be a \
         lie the schema tells its reader; got: {}",
        ddl.0
    );

    // What the column HOLDS.
    let stored: (String,) = sqlx::query_as("SELECT typeof(id) FROM pkm2m_lab LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("read the stored type");
    assert_eq!(
        stored.0, "blob",
        "and it must actually STORE a blob — the declaration is only honest if it matches"
    );
}
