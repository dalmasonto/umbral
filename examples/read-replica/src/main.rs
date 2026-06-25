//! Read-replica routing with a custom `DatabaseRouter`.
//!
//! A single trait impl is all it takes to send every READ to a replica pool
//! and every WRITE to the primary — the ORM, models, and handlers are written
//! exactly as a single-database app. Nothing here touches `umbral_core`
//! directly; everything is reachable through the `umbral` facade.
//!
//! Run it:
//! ```text
//! cd examples/read-replica
//! cargo run
//! # then, in another shell:
//! curl localhost:3000/notes/add   # WRITE -> primary
//! curl localhost:3000/notes/add
//! curl localhost:3000/notes        # READ  -> replica
//! ```
//! Watch the server log: each request prints which pool the router chose.
//!
//! In production, point `UMBRAL_REPLICA_URL` at a real streaming read replica.
//! Unset, it falls back to the primary URL so this demo runs against one DB
//! (reads see writes immediately); the routing code is identical either way.

use chrono::Utc;
use std::sync::atomic::{AtomicU64, Ordering};

use umbral::db::{Alias, DatabaseRouter, DbPool, RouteContext};
use umbral::migrate::ModelMeta;
use umbral::prelude::*;

/// One guestbook-style note. A plain umbral model — it has no idea routing
/// exists.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "note")]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Reads → the `"replica"` pool, writes → the `"default"` (primary) pool.
/// Every decision is logged so you can SEE the split in the server output.
struct ReplicaRouter;

impl DatabaseRouter for ReplicaRouter {
    fn db_for_read(&self, model: &ModelMeta, _ctx: &RouteContext) -> Alias {
        tracing::info!(table = %model.table, "router: READ  -> replica");
        Alias::new("replica")
    }

    fn db_for_write(&self, model: &ModelMeta, _ctx: &RouteContext) -> Alias {
        tracing::info!(table = %model.table, "router: WRITE -> default (primary)");
        Alias::new("default")
    }
}

static SEQ: AtomicU64 = AtomicU64::new(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let mut settings = Settings::from_env()?;

    // Default to a sqlite FILE so the primary and replica pools share one
    // database and reads see writes. The framework default is `sqlite::memory:`,
    // which would hand each pool its own separate in-memory DB — fine for tests,
    // confusing for this demo. Override either side with UMBRAL_DATABASE_URL /
    // UMBRAL_REPLICA_URL (e.g. two Postgres URLs for a real replica split).
    if settings.database_url == "sqlite::memory:" {
        settings.database_url = "sqlite://read_replica.db?mode=rwc".to_string();
    }

    // Primary pool (writes), registered under the framework-required "default"
    // alias.
    let primary = umbral::db::connect(&settings.database_url).await?;

    // Replica pool (reads). A real read replica in production; here it falls
    // back to the primary URL so the demo is self-contained.
    let replica_url =
        std::env::var("UMBRAL_REPLICA_URL").unwrap_or_else(|_| settings.database_url.clone());
    let replica = umbral::db::connect(&replica_url).await?;
    tracing::info!(primary = %settings.database_url, replica = %replica_url, "pools connected");

    // Demo shortcut: ensure the `note` table exists on both pools. A real app
    // declares the model and runs `makemigrations` + `migrate`; replication
    // then carries the schema to the replica.
    ensure_schema(&primary).await?;
    ensure_schema(&replica).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", primary)
        .database("replica", replica)
        .router(ReplicaRouter)
        .model::<Note>()
        .routes(
            Routes::new()
                .get("/", root)
                .get("/notes", list_notes)
                .get("/notes/add", add_note),
        )
        .build()?;

    app.serve("127.0.0.1:3000".parse::<std::net::SocketAddr>()?)
        .await?;
    Ok(())
}

async fn root() -> &'static str {
    "umbral read-replica demo\n\n\
     GET /notes/add  -> create a note  (WRITE routed to the primary pool)\n\
     GET /notes      -> list the notes (READ  routed to the replica pool)\n\n\
     Each request logs which pool the DatabaseRouter chose. The handlers below\n\
     use the ORM exactly as a single-database app would.\n"
}

/// WRITE path: `create` routes through `db_for_write` → the primary.
async fn add_note() -> String {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let note = Note {
        id: 0,
        body: format!("note #{n}"),
        created_at: Utc::now(),
    };
    match Note::objects().create(note).await {
        Ok(saved) => format!(
            "created note id={} body={:?}  (WRITE -> primary)\n",
            saved.id, saved.body
        ),
        Err(e) => format!("create failed: {e}\n"),
    }
}

/// READ path: `fetch` routes through `db_for_read` → the replica.
async fn list_notes() -> String {
    match Note::objects().fetch().await {
        Ok(notes) => {
            let mut out = format!("{} note(s)  (READ -> replica):\n", notes.len());
            for note in &notes {
                out.push_str(&format!("  [{}] {} @ {}\n", note.id, note.body, note.created_at));
            }
            out
        }
        Err(e) => format!("list failed: {e}\n"),
    }
}

/// Create the `note` table on a pool if it doesn't exist (demo bootstrap).
async fn ensure_schema(pool: &DbPool) -> Result<(), sqlx::Error> {
    match pool {
        DbPool::Sqlite(p) => {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS note (\
                     id INTEGER PRIMARY KEY AUTOINCREMENT,\
                     body TEXT NOT NULL,\
                     created_at TEXT NOT NULL\
                 )",
            )
            .execute(p)
            .await?;
        }
        DbPool::Postgres(p) => {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS note (\
                     id BIGSERIAL PRIMARY KEY,\
                     body TEXT NOT NULL,\
                     created_at TIMESTAMPTZ NOT NULL\
                 )",
            )
            .execute(p)
            .await?;
        }
    }
    Ok(())
}
