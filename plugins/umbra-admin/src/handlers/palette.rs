//! `⌘K` command palette — the menu shell + the global record search.
//!
//! The palette renders a fixed set of models (jump targets), a fixed
//! command list (toggle theme, logout), and — when the user types a
//! 2+ character query — a debounced HTMX call back to
//! `/admin/api/palette/search?q=...` whose results overlay the
//! "Records" section.

use std::collections::HashMap;

use axum::extract::{Query, State};
use minijinja::context;
use sqlx::Row;
use umbra::orm::SqlType;
use umbra::web::{HeaderMap, IntoResponse, Response, StatusCode};

use crate::auth::require_staff;
use crate::discovery::{discover_models, pk_column};
use crate::engine::render;
use crate::util::{html_escape, q};
use crate::view::sidebar_apps;
use crate::AdminState;

/// `GET /admin/api/palette` — returns the command palette HTML
/// fragment. Jump targets = registered models from the sidebar; fixed
/// commands = toggle theme + logout.
pub(crate) async fn palette_fragment(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Response {
    let user = match require_staff(&headers, "/admin/api/palette").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let sidebar = sidebar_apps(&state, &user);

    let models: Vec<serde_json::Value> = sidebar
        .into_iter()
        .flat_map(|app| app.models)
        .map(|r| {
            serde_json::json!({
                "table": r.table,
                "label": r.label,
                "icon": r.icon,
            })
        })
        .collect();

    let commands = vec![
        serde_json::json!({ "key": "toggle_theme", "label": "Toggle theme", "icon": "sun-moon" }),
        serde_json::json!({ "key": "logout",       "label": "Logout",       "icon": "log-out" }),
    ];

    match render(
        "admin/palette.html",
        context!(
            models   => models,
            commands => commands,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/api/palette/search?q=<term>` — search across all
/// registered models that have `search_fields` configured and return
/// up to 10 matching rows as palette items.
///
/// ⚠ Raw SQL. The ORM doesn't yet expose a runtime-typed LIKE-across-
/// columns terminal — the dynamic column set comes from `ModelMeta` at
/// request time. Migrating this to ORM means extending the ORM with
/// a `QuerySet::from_meta(...).filter_any_like(...)` shape; until then
/// this stays raw. Column / table names come from the registry, are
/// quoted via `q()`, and the user-supplied search term is the only
/// parameterized bind, so it's safe — just not the ORM target shape.
pub(crate) async fn palette_search(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = require_staff(&headers, "/admin/api/palette/search").await {
        return r;
    }
    let query_term = params.get("q").map(|s| s.as_str()).unwrap_or("").trim();
    if query_term.len() < 2 {
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::OK.into_response());
    }

    let pool = umbra::db::pool();
    let mut html = String::new();
    let mut total_found = 0usize;
    const MAX_RESULTS: usize = 10;

    for (_, model) in discover_models() {
        if total_found >= MAX_RESULTS {
            break;
        }
        let cfg = state.config_for(&model.table);
        let search_fields: Vec<String> = cfg
            .filter(|c| !c.search_fields.is_empty())
            .map(|c| c.search_fields.clone())
            .unwrap_or_default();
        if search_fields.is_empty() {
            continue;
        }

        let valid_names: std::collections::HashSet<&str> =
            model.fields.iter().map(|c| c.name.as_str()).collect();
        let pk = match pk_column(&model) {
            Some(p) => p,
            None => continue,
        };

        // Pick a human-readable label column: first non-pk text column.
        let label_col = model
            .fields
            .iter()
            .find(|c| !c.primary_key && matches!(c.ty, SqlType::Text))
            .map(|c| c.name.as_str())
            .unwrap_or(pk.name.as_str());

        let like_clauses: Vec<String> = search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if like_clauses.is_empty() {
            continue;
        }

        let where_sql = format!("WHERE ({})", like_clauses.join(" OR "));
        let sql = format!(
            "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{table}\" {where_sql} LIMIT ?",
            pk_col = q(&pk.name),
            label_col = q(label_col),
            table = q(&model.table),
        );
        let like_val = format!("%{query_term}%");
        let remaining = MAX_RESULTS - total_found;

        let mut qb = sqlx::query(&sql);
        for _ in &like_clauses {
            qb = qb.bind(like_val.clone());
        }
        qb = qb.bind(remaining as i64);

        if let Ok(rows) = qb.fetch_all(&pool).await {
            for row in rows {
                if total_found >= MAX_RESULTS {
                    break;
                }
                let id: String = row
                    .try_get::<i64, _>(0)
                    .map(|v| v.to_string())
                    .or_else(|_| row.try_get::<String, _>(0))
                    .unwrap_or_default();
                let label: String = row
                    .try_get::<String, _>(1)
                    .unwrap_or_else(|_| format!("#{id}"));
                let item_label = format!("{}: {}", model.name, label);
                let href = format!("/admin/{}/{}/sheet", model.table, id);
                html.push_str(&format!(
                    r#"<li role="option" data-palette-href="{href}" class="palette-item flex items-center gap-sm px-lg py-sm cursor-pointer hover:bg-surface-container-high transition-colors group" onclick="umbra._paletteGo(this)" tabindex="-1">
  <div class="w-8 h-8 rounded-xl bg-primary-container/10 border border-primary/20 flex items-center justify-center flex-shrink-0">
    <i data-lucide="file-search" class="w-4 h-4 text-primary"></i>
  </div>
  <span class="text-body-md text-on-surface">{label}</span>
  <span class="ml-auto text-label-sm text-outline opacity-0 group-hover:opacity-100 transition-opacity">Open</span>
</li>"#,
                    href = html_escape(&href),
                    label = html_escape(&item_label),
                ));
                total_found += 1;
            }
        }
    }

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}
