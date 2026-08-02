// src/main.rs

use umbral::prelude::*; // brings the `Model` trait into scope for `table_name()`
use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, BearerAuthentication};
use umbral_graphql::GraphqlPlugin;
use umbral_openapi::OpenApiPlugin;
use umbral_playground::PlaygroundPlugin;
use umbral_rest::{IsAuthenticated, PageNumberPagination, ResourceConfig, RestPlugin};
use umbral_health::HealthPlugin;
use umbral_security::SecurityPlugin;
use umbral_sessions::SessionsPlugin;
use umbral_storage::StoragePlugin;
use umbral_tasks::TasksPlugin;
use umbral::web::SlashRedirect;

// Your app's models, so the registrations below can name them by
// `Project::table_name()` instead of a hardcoded string.
use projects::models::{Comment, Label, Project, Task};

// HTTP handlers for this binary (the home page). Referenced as
// `views::public::home` in the route table below.
mod views;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Install the tracing subscriber. Without it the server binds and serves
    // SILENTLY — `App::serve` logs "umbral serving on ..." via `tracing`, so
    // with no subscriber there is no output and it looks hung. Keep the guard
    // alive for the whole program (it flushes on drop).
    let _log = umbral_logs::observability::init(umbral_logs::ObservabilityConfig::from_env());

    let settings = Settings::from_env()?;
    let pool = umbral::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // ONE app-wide authentication backend (gaps4 #42): REST and
        // GraphQL below inherit it — no more pasting the same
        // `.authenticate(...)` into each plugin, where forgetting one copy
        // silently made that surface anonymous.
        .authentication(BearerAuthentication::default())
        // --- Built-in plugins -------------------------------------------
        .plugin(AuthPlugin::new().with_default_routes())
        .plugin(SessionsPlugin::default())
        // Liveness + readiness probes at /healthz and /ready — zero
        // config; a deploy's load balancer reads these.
        .plugin(HealthPlugin::default())
        // Background task queue. `#[umbral::task]` handlers (see
        // `celebrate_task` below) are DISCOVERED automatically — run the
        // worker alongside the server with `cargo run -- tasks-worker`.
        .plugin(TasksPlugin::default())
        // --- Your app ---------------------------------------------------
        .plugin(projects::ProjectsPlugin::default())
        // --- Admin (Step 8) ---------------------------------------------
        .plugin(
            AdminPlugin::default()
                .site_title("Tracker".to_string())
                .register(AdminModel::new(Project::table_name()).search_fields(&["name", "slug"]))
                .register(
                    AdminModel::new(Task::table_name())
                        .search_fields(&["title"])
                        .list_filter(&["status", "project", "assignee"]),
                )
                .register(AdminModel::new(Comment::table_name())),
        )
        // --- REST + OpenAPI + Playground (Step 9) -----------------------
        .plugin(
            RestPlugin::default()
                .paginate(PageNumberPagination::new(20))
                .resource(
                    ResourceConfig::new(Task::table_name()).permission(IsAuthenticated), // logged-in users may write tasks
                ),
        )
        .plugin(
            OpenApiPlugin::new()
                .at("/api/docs")
                .title("Tracker API")
                .version("0.1.0")
                .description("Task tracker — projects, tasks, and comments."),
        )
        // The console's default mount is `/playground/` — deliberately off
        // the `/api` namespace, where REST's `/api/{table}` route would
        // shadow the no-slash URL (gaps4 #43). `.at(...)` relocates it.
        .plugin(PlaygroundPlugin::new("tracker"))
        // --- GraphQL (Step 10) ------------------------------------------
        .plugin(
            GraphqlPlugin::new()
                // Identity comes from the app-wide `.authentication(...)`
                // above — the gates below see the same caller REST does.
                // Reads
                .expose(Project::table_name())
                .expose(Task::table_name())
                .expose(Label::table_name())
                .expose(Comment::table_name())
                // The user model, so `task { assignee { username } }` and
                // `comment { author { username } }` resolve to an OBJECT. An FK
                // whose target is not exposed degrades to a bare id String —
                // the field still exists, it just has no subfields to select.
                .expose(AuthUser::table_name())
                // ...but only `id` + `username` of it. `hide` is a denylist, so
                // every other column is named here explicitly. Adding a column
                // to AuthUser therefore EXPOSES it by default — a new field on
                // the user model needs a matching line below.
                //
                // `password_hash` is deliberately absent: it is denied in core
                // (`umbral::orm::HARD_DENIED_FIELDS`) and no `expose` here can
                // bring it back, on any transport.
                .hide(
                    AuthUser::table_name(),
                    [
                        "email",
                        "is_active",
                        "is_staff",
                        "is_superuser",
                        "date_joined",
                        "last_login",
                        "email_verified_at",
                    ],
                )
                // Table-level write gate: anyone may READ tasks, but only a
                // signed-in staff member may create / update / delete one.
                .mutable_if(Task::table_name(), |id| id.is_some_and(|i| i.is_staff))
                // Object-level write gate: comments are writable, but a caller
                // may only edit or delete their OWN.
                .mutable(Comment::table_name())
                .owned_by(Comment::table_name(), "author"),
        )
        // Token-authenticated JSON surfaces don't use CSRF cookies, and
        // GraphQL speaks POST — exempt both (gaps4 #41's shorthand).
        .plugin(SecurityPlugin::new().csrf_exempt(["/api", "/graphql"]))
        // Serve ./static at /static so the home page's stylesheet loads.
        .plugin(StoragePlugin::new().static_files("/static", "./static"))
        // --- Templates + routes -----------------------------------------
        .templates_dir("templates")
        // Redirect /foo → /foo/ (append trailing slash) so `/admin`,
        // `/api/task`, etc. resolve without the trailing slash too.
        .slash_redirect(SlashRedirect::Append)
        // A home page at `/` so the root isn't a 404. The admin, REST,
        // and GraphQL routes come from their plugins above.
        .routes(Routes::new().get("/", views::public::home))
        .build_deferred()?;

    umbral_cli::dispatch(app).await
}

// ---------------------------------------------------------------------------
// Background work (Step 11)
// ---------------------------------------------------------------------------

/// Payload for [`celebrate_task`]. Any `Serialize + Deserialize` type works.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CelebratePayload {
    pub task_title: String,
}

/// A background task. `#[umbral::task]` registers it in a link-time slice,
/// so the worker discovers it automatically — nothing to wire in `main`.
/// Enqueue from any handler:
///
/// ```ignore
/// CelebrateTask::enqueue(CelebratePayload { task_title: "ship it".into() }).await?;
/// ```
///
/// …and drain the queue with a worker process:
///
/// ```text
/// cargo run -- tasks-worker
/// ```
#[umbral::task]
async fn celebrate_task(payload: CelebratePayload) -> Result<(), String> {
    tracing::info!(title = %payload.task_title, "a task was completed — celebrate!");
    Ok(())
}

