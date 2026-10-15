//! Live-Postgres proof that `select_related` hydrates a related row with a
//! `DateTime<Utc>` column.
//!
//! Found on the bench rig (2026-10-14): on Postgres the `select_related`
//! prefetch decodes each related row to JSON through a blind
//! try-everything cascade (`row_to_json`): `i64 → f64 → bool → uuid →
//! String → null`. A `timestamptz` matches NONE of those, so every datetime
//! column came back as JSON `null`; the model's `Deserialize` then failed on
//! the missing value and `hydrate_fk`'s forgive-and-continue posture dropped
//! the whole hydration silently. Visible symptom on the bench app: feed and
//! post pages served fallback author names (`user_4538`) on Postgres, while
//! the naive per-row path — and the JOIN path, which decodes through the
//! metadata-aware `decode_pg_to_json` — resolved the real ones.
//!
//! SQLite cannot catch this: it stores every temporal as TEXT, so the String
//! arm of the cascade happens to carry a chrono-acceptable form (see the
//! SQLite twin in `tests/select_related.rs`, which passes before the fix).
//!
//! Gated on `UMBRAL_TEST_POSTGRES_URL`. Its own binary because `App::build`
//! publishes process-global state (one build per binary).

use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "sr_pg_member")]
pub struct Member {
    pub id: i64,
    pub username: String,
    pub joined_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "sr_pg_note")]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub member: ForeignKey<Member>,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn select_related_hydrates_a_datetime_bearing_related_row_on_postgres() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let db = umbral::db::connect(&url)
        .await
        .expect("connect to Postgres");
    let umbral::db::DbPool::Postgres(pool) = &db else {
        panic!("UMBRAL_TEST_POSTGRES_URL must be a Postgres URL");
    };
    let pool = pool.clone();

    // Clean slate for THIS test's tables only (they exist nowhere else).
    sqlx::query("DROP TABLE IF EXISTS sr_pg_note")
        .execute(&pool)
        .await
        .expect("drop prior note table");
    sqlx::query("DROP TABLE IF EXISTS sr_pg_member")
        .execute(&pool)
        .await
        .expect("drop prior member table");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", db)
        .model::<Member>()
        .model::<Note>()
        .build()
        .expect("App::build");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema on Postgres");

    let joined = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .expect("parse fixture timestamp")
        .with_timezone(&chrono::Utc);
    let member = Member::objects()
        .create(Member {
            id: 0,
            username: "ada".to_string(),
            joined_at: joined,
        })
        .await
        .expect("create member");
    Note::objects()
        .create(Note {
            id: 0,
            body: "hello".to_string(),
            member: ForeignKey::new(member.id),
        })
        .await
        .expect("create note");

    let note = Note::objects()
        .select_related("member")
        .get()
        .await
        .expect("get note");

    let resolved = note.member.__resolved().expect(
        "select_related must hydrate a related row containing a timestamptz column; \
         None here means the row→JSON decode dropped it and the typed deserialize \
         failed (hydrate_fk swallows that error by design)",
    );
    assert_eq!(resolved.username, "ada");
    assert_eq!(
        resolved.joined_at, joined,
        "the hydrated timestamp must survive the JSON round trip exactly"
    );
}
