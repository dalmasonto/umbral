//! PK refactor — the Postgres-native-uuid edge. A model with a
//! `uuid::Uuid` primary key, exercised through `select_related` (forward
//! FK) AND reverse-FK prefetch against a REAL Postgres. The FK columns are
//! native `uuid` type there, so the relation IN-binder must bind a real
//! `Uuid` (not text) — which is exactly what `fetch_related_as_json_by_pk`
//! now does via the threaded PK `SqlType`.
//!
//! Self-skips unless `UMBRA_TEST_POSTGRES_URL` points at a server:
//!   UMBRA_TEST_POSTGRES_URL=postgres://… cargo test -p umbra-core \
//!     --test pk_uuid_postgres -- --ignored

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use umbra::orm::{ForeignKey, ReverseSet};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkuuid_org")]
pub struct Org {
    #[umbra(primary_key)]
    pub id: uuid::Uuid,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "org")]
    pub members: ReverseSet<Member>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkuuid_member")]
pub struct Member {
    pub id: i64,
    pub org: ForeignKey<Org>, // FK to a Uuid-PK target → native uuid column
    pub name: String,
}

#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn uuid_pk_relations_round_trip_on_postgres() {
    let Ok(url) = std::env::var("UMBRA_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRA_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect Postgres");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Org>()
        .model::<Member>()
        .build()
        .expect("App::build");

    // Fresh schema each run.
    for ddl in [
        "DROP TABLE IF EXISTS pkuuid_member",
        "DROP TABLE IF EXISTS pkuuid_org",
        "CREATE TABLE pkuuid_org (id UUID PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE pkuuid_member (
            id BIGSERIAL PRIMARY KEY,
            org UUID NOT NULL REFERENCES pkuuid_org(id),
            name TEXT NOT NULL
        )",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("ddl");
    }

    let org_id = uuid::Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
    sqlx::query("INSERT INTO pkuuid_org (id, name) VALUES ($1, $2)")
        .bind(org_id)
        .bind("Acme")
        .execute(&pool)
        .await
        .expect("seed org");
    for name in &["alice", "bob"] {
        sqlx::query("INSERT INTO pkuuid_member (org, name) VALUES ($1, $2)")
            .bind(org_id)
            .bind(*name)
            .execute(&pool)
            .await
            .expect("seed member");
    }

    // FORWARD: select_related on the uuid FK — binds the Uuid in the
    // org.id IN-list (native uuid column).
    let members = Member::objects()
        .select_related("org")
        .on_pg(&pool)
        .fetch()
        .await
        .expect("select_related on a uuid FK");
    assert_eq!(members.len(), 2);
    for m in &members {
        let org = m
            .org
            .resolved()
            .expect("uuid FK resolved via select_related on Postgres");
        assert_eq!(org.name, "Acme");
        assert_eq!(org.id, org_id);
    }

    // REVERSE: prefetch members of a uuid-PK parent — binds the Uuid
    // parent PK in the member.org IN-list.
    let orgs = Org::objects()
        .prefetch_related("members")
        .on_pg(&pool)
        .fetch()
        .await
        .expect("reverse-FK prefetch on a uuid-PK parent");
    let acme = orgs.iter().find(|o| o.id == org_id).expect("org present");
    let mut names: Vec<&str> = acme
        .members
        .resolved()
        .expect("ReverseSet hydrated for a uuid-PK parent on Postgres")
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec!["alice", "bob"]);

    // Clean up.
    for ddl in ["DROP TABLE pkuuid_member", "DROP TABLE pkuuid_org"] {
        sqlx::query(ddl).execute(&pool).await.expect("cleanup");
    }
}
