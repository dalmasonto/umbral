//! gaps4 #39 — filtering a `#[umbral(choices)]` column accepts the enum
//! variant, not only a raw string.
//!
//! `ticket::STATUS.eq(TicketStatus::InProgress)` must compile and bind the
//! same DB string the write path uses (respecting `#[choices(rename_all)]`),
//! while `.eq("in_progress")` keeps working (non-breaking).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral_core::db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, umbral::orm::Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Todo,
    InProgress,
    Done,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "cf_ticket")]
pub struct Ticket {
    pub id: i64,
    pub title: String,
    #[umbral(choices)]
    pub status: TicketStatus,
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
            .model::<Ticket>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        // Seed via the TYPED write path — status is the enum, not a string.
        for (title, status) in [
            ("a", TicketStatus::Todo),
            ("b", TicketStatus::InProgress),
            ("c", TicketStatus::InProgress),
            ("d", TicketStatus::Done),
        ] {
            Ticket::objects()
                .create(Ticket {
                    id: 0,
                    title: title.to_string(),
                    status,
                })
                .await
                .expect("create ticket");
        }
    })
    .await;
}

#[tokio::test]
async fn eq_accepts_the_enum_variant() {
    boot().await;
    let n = Ticket::objects()
        .filter(ticket::STATUS.eq(TicketStatus::InProgress))
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "two tickets are in_progress");
}

#[tokio::test]
async fn enum_binds_the_same_string_as_a_raw_filter() {
    boot().await;
    // Non-breaking: the string form still compiles and selects the same rows.
    let by_enum = Ticket::objects()
        .filter(ticket::STATUS.eq(TicketStatus::InProgress))
        .count()
        .await
        .expect("count enum");
    let by_str = Ticket::objects()
        .filter(ticket::STATUS.eq("in_progress"))
        .count()
        .await
        .expect("count str");
    assert_eq!(
        by_enum, by_str,
        "enum and its db string select the same rows"
    );
    assert_eq!(by_str, 2);
}

#[tokio::test]
async fn ne_accepts_the_enum_variant() {
    boot().await;
    let n = Ticket::objects()
        .filter(ticket::STATUS.ne(TicketStatus::Done))
        .count()
        .await
        .expect("count");
    assert_eq!(n, 3, "todo + two in_progress are not done");
}

#[tokio::test]
async fn roundtrip_create_then_filter_by_enum() {
    boot().await;
    let rows = Ticket::objects()
        .filter(ticket::STATUS.eq(TicketStatus::Todo))
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, TicketStatus::Todo);
    assert_eq!(rows[0].title, "a");
}
