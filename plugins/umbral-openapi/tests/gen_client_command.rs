//! gaps3 #38 — the `gen-client` command end to end, through the real dispatcher.
//!
//! Builds an app with `RestPlugin` + `OpenApiPlugin` (so the REST/OpenAPI config
//! `OnceLock`s are published by `routes()`), then runs `umbral gen-client --out
//! <tmp>` exactly as `cargo run -- gen-client` would, and checks it wrote the two
//! files and honoured a `hide(...)` on a column. This proves the whole path:
//! plugin-contributed command → offline (no `on_ready`) → reads the live
//! registry + REST config → writes TypeScript.
//!
//! One `App::build*` per test binary (process-global `OnceLock`s), so this is the
//! only test here.

use serde::{Deserialize, Serialize};
use umbral_openapi::OpenApiPlugin;
use umbral_rest::RestPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gc_widget")]
pub struct GcWidget {
    pub id: i64,
    pub name: String,
    /// Hidden from the REST surface below — must not appear as a filter key.
    pub secret_token: String,
}

#[tokio::test]
async fn gen_client_writes_typed_files_and_respects_hide() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    // build_deferred: gen-client is an offline command; its `on_ready` must not
    // fire (it would seed against an empty DB). dispatch runs the command
    // without firing ready because `command_needs_ready("gen-client")` is false.
    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<GcWidget>()
        .plugin(RestPlugin::default().hide("gc_widget", "secret_token"))
        .plugin(OpenApiPlugin::default())
        .build_deferred()
        .expect("App::build_deferred");

    let out = tempfile::tempdir().expect("tempdir");
    let out_path = out.path().to_str().unwrap().to_string();

    let argv: Vec<std::ffi::OsString> = ["umbral", "gen-client", "--out", &out_path]
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    umbral_cli::dispatch_with_argv(app, argv)
        .await
        .expect("gen-client dispatch");

    let models = std::fs::read_to_string(out.path().join("models.ts")).expect("models.ts written");
    let client = std::fs::read_to_string(out.path().join("client.ts")).expect("client.ts written");

    // The row type is there, and the client keys off the exposed table.
    assert!(
        models.contains("export interface GcWidget {"),
        "got:\n{models}"
    );
    // `hide` is response-only: the hidden column is NOT in the row type...
    let row = models
        .split("export interface GcWidget {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("GcWidget interface");
    assert!(
        !row.contains("secret_token"),
        "a hidden column must not appear in the response row type; got:\n{row}",
    );
    assert!(
        row.contains("name"),
        "a visible column must be in the row; got:\n{row}"
    );
    // ...but it stays settable (a hidden field can be write-only).
    let create = client
        .split("export interface GcWidgetCreate {")
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .expect("GcWidgetCreate block");
    assert!(
        create.contains("secret_token"),
        "a hidden-but-writable column stays in the create DTO; got:\n{create}",
    );
    assert!(
        client.contains(r#"  "gc_widget": { row: GcWidget;"#),
        "the client must map the exposed table; got:\n{client}",
    );
    assert!(
        client.contains("Base path: /api"),
        "default REST base path; got:\n{client}"
    );

    // The hidden column must not be a filter key (it's stripped from the surface).
    let filters = client
        .split("export interface GcWidgetFilters {")
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .expect("GcWidgetFilters block");
    assert!(
        !filters.contains("secret_token"),
        "a hidden column must not be filterable; got:\n{filters}",
    );
    assert!(
        filters.contains(r#""name""#),
        "a visible column must be; got:\n{filters}"
    );
}
