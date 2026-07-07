//! Minimal umbral app for an apples-to-apples framework benchmark: one
//! model, four endpoints (plain text, JSON, DB read, DB write). NO session
//! or auth plugins — just the core + the ORM, so the numbers reflect the
//! framework and ORM, not a maximalist app's middleware stack.
//!
//! DB defaults to a WAL file (`hello_bench.db`) via umbral's real
//! `connect_sqlite` (WAL + synchronous=NORMAL + 5s busy_timeout). Override
//! with `UMBRAL_DATABASE_URL`.

use umbral::prelude::*;
use umbral::web::JsonResponse;

#[derive(
    Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "hello_note")]
struct Note {
    id: i64,
    title: String,
}

#[derive(serde::Serialize)]
struct BenchJson {
    ok: bool,
    name: &'static str,
    items: [i32; 4],
}

async fn bench_text() -> &'static str {
    "hello from umbral-hello"
}

async fn bench_json() -> JsonResponse<BenchJson> {
    JsonResponse(BenchJson {
        ok: true,
        name: "hello",
        items: [1, 2, 3, 4],
    })
}

// DB read: newest 25 rows + a total count (mirrors the cot/shop read).
async fn bench_read() -> JsonResponse<Vec<Note>> {
    let notes = Note::objects()
        .order_by(note::ID.desc())
        .limit(25)
        .fetch()
        .await
        .expect("read");
    let _total = Note::objects().count().await.expect("count");
    JsonResponse(notes)
}

async fn bench_write() -> &'static str {
    Note::objects()
        .create(Note {
            id: 0,
            title: "ApacheBench note".to_string(),
        })
        .await
        .expect("write");
    "ok"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = Settings::from_env()?;
    let db_url = std::env::var("UMBRAL_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://hello_bench.db?mode=rwc".to_string());

    // umbral's genuine SQLite pool (WAL + synchronous=NORMAL + busy_timeout).
    let pool = umbral::db::connect_sqlite(&db_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Note>()
        .routes(
            Routes::new()
                .get("/bench/text", bench_text)
                .get("/bench/json", bench_json)
                .get("/bench/notes/read", bench_read)
                .get("/bench/notes/write", bench_write),
        )
        .build()?;

    umbral_cli::dispatch(app).await
}
