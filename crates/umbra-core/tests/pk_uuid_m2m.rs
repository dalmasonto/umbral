//! PK refactor — M2M on a `uuid::Uuid`-PK PARENT. Mirrors `pk_string_m2m`
//! with a UUID primary key + `M2M<Researcher, uuid::Uuid>` field, proving the
//! M2M junction plumbing (`set_m2m_parent_ids` / add / prefetch / join_related
//! / `__parent_id` read-back) is fully PK-agnostic — not i64-bound (gaps2 #88).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::M2M;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkm2m_lab")]
pub struct Lab {
    #[umbra(primary_key)]
    pub id: uuid::Uuid,
    pub name: String,
    /// `P = uuid::Uuid` — the parent (Lab) has a UUID PK.
    #[sqlx(skip)]
    #[serde(skip)]
    pub members: M2M<Researcher, uuid::Uuid>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkm2m_researcher")]
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
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Lab>()
            .model::<Researcher>()
            .build()
            .expect("App::build");

        for ddl in [
            "CREATE TABLE pkm2m_lab (id TEXT PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE pkm2m_researcher (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE pkm2m_lab_members (
                parent_id TEXT NOT NULL,
                child_id INTEGER NOT NULL,
                PRIMARY KEY (parent_id, child_id)
            )",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }

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
