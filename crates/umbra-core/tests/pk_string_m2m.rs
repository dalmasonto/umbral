//! PK refactor — M2M on a String-PK PARENT. The owning model has a
//! `String` (code) primary key and an `M2M<Student, String>` field
//! (`P = String`). Exercises the macro's `set_m2m_parent_ids` /
//! `write_pending_m2m` and the junction-prefetch `__parent_id` read-back,
//! all of which were i64-bound before the lift.
//!
//! `M2M<T, P>` was already generic over the parent PK type `P`; the work
//! was lifting the macro glue + the junction prefetch off i64.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::M2M;
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkm2m_course")]
pub struct Course {
    #[umbra(primary_key)]
    pub code: String,
    pub title: String,
    /// `P = String` — the parent (Course) has a String PK.
    #[sqlx(skip)]
    #[serde(skip)]
    pub students: M2M<Student, String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkm2m_student")]
pub struct Student {
    pub id: i64,
    pub name: String,
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
            .model::<Course>()
            .model::<Student>()
            .build()
            .expect("App::build");

        for ddl in [
            "CREATE TABLE pkm2m_course (code TEXT PRIMARY KEY, title TEXT NOT NULL)",
            "CREATE TABLE pkm2m_student (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            // Junction: <table>_<field> with a TEXT parent_id (the course code).
            "CREATE TABLE pkm2m_course_students (
                parent_id TEXT NOT NULL,
                child_id INTEGER NOT NULL,
                PRIMARY KEY (parent_id, child_id)
            )",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }

        for (code, title) in &[("rust101", "Intro to Rust"), ("go101", "Intro to Go")] {
            sqlx::query("INSERT INTO pkm2m_course (code, title) VALUES (?, ?)")
                .bind(*code)
                .bind(*title)
                .execute(&pool)
                .await
                .expect("seed course");
        }
        for name in &["alice", "bob", "carol"] {
            sqlx::query("INSERT INTO pkm2m_student (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed student");
        }
    })
    .await;
}

#[tokio::test]
async fn m2m_add_and_prefetch_on_a_string_pk_parent() {
    boot().await;

    let students = Student::objects().fetch().await.expect("students");
    let by_name = |n: &str| students.iter().find(|s| s.name == n).unwrap().clone();
    let alice = by_name("alice");
    let bob = by_name("bob");

    // Fetch the course — `set_m2m_parent_ids` seeds the String parent PK
    // + junction table onto the M2M slot.
    let rust = Course::objects()
        .filter(course::CODE.eq("rust101"))
        .first()
        .await
        .expect("query")
        .expect("rust101 present");

    // Write junction rows: parent_id is the String code "rust101".
    rust.students.add(&alice).await.expect("add alice");
    rust.students.add(&bob).await.expect("add bob");

    // Prefetch: the junction-join reads __parent_id back as a String and
    // buckets PK-agnostically.
    let courses = Course::objects()
        .prefetch_related("students")
        .fetch()
        .await
        .expect("prefetch");

    let rust = courses.iter().find(|c| c.code == "rust101").unwrap();
    let mut names: Vec<&str> = rust
        .students
        .resolved()
        .expect("M2M hydrated for a String-PK parent")
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec!["alice", "bob"]);

    // go101 has no students → resolved is Some(&[]).
    let go = courses.iter().find(|c| c.code == "go101").unwrap();
    assert!(
        go.students.resolved().expect("hydrated (empty)").is_empty(),
        "go101 has no students"
    );
}
