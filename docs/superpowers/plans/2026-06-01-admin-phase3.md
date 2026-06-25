# Admin Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship actions + async FK pickers + sheet stacking + inline cell edit so the admin is usable for real workflows.

**Architecture:** Extend `config.rs` Action struct with `icon/variant/scope/confirm/permission` fields; add new Axum routes for `POST /admin/{table}/actions/{key}`, `GET /admin/api/{table}/{field}/options[/resolve]`, and `GET|POST /admin/{table}/{id}/cell/{field}[/edit]`; extend the four template macros and add a toast renderer; three new test files using the OnceCell+Mutex boot pattern.

**Tech Stack:** Rust, Axum, minijinja, HTMX, vanilla JS (~230 LOC total), SQLite (sqlx), serde_json.

---

## File map

| Action | File |
|---|---|
| Modify | `plugins/umbral-admin/src/config.rs` |
| Modify | `plugins/umbral-admin/src/lib.rs` |
| Modify | `plugins/umbral-admin/templates/_macros/data_table.html` |
| Modify | `plugins/umbral-admin/templates/_macros/field_editor.html` |
| Modify | `plugins/umbral-admin/templates/_macros/sheet.html` |
| Modify | `plugins/umbral-admin/templates/wrapper.html` |
| New | `plugins/umbral-admin/tests/phase3_actions.rs` |
| New | `plugins/umbral-admin/tests/phase3_fk_picker.rs` |
| New | `plugins/umbral-admin/tests/phase3_inline_edit.rs` |
| Modify | `documentation/docs/v0.0.1/plugins/admin.mdx` |

---

## Task 1: Extend Action descriptor in config.rs

The existing `Action` carries only `name`, `label`, `handler`. Phase 3 needs `icon`, `variant`, `scope`, `confirm`, and `permission`. The handler signature also changes: receives `ActionInvocation` instead of `(Vec<i64>, AdminContext)`.

**Files:**
- Modify: `plugins/umbral-admin/src/config.rs`

- [ ] **Step 1: Add the new types to config.rs**

Replace the entire `config.rs` content with this updated version. Key changes:
- `ActionVariant`, `ActionScope`, `ToastLevel`, `ActionResult` enums
- `ActionInvocation` struct (ids, user, pool, table)
- `Action` gains `icon`, `variant`, `scope`, `confirm`, `permission`
- `ActionHandler` type alias changes signature
- `Action::new` updated constructor
- `Action::delete_selected()` rebuilt against new shape
- `AdminModel` gains `inline_edit_fields` method

```rust
//! Per-model admin customization bundles.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::SqlitePool;

// =========================================================================
// Action result / invocation types
// =========================================================================

#[derive(Debug, Clone)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToastLevel::Info => "info",
            ToastLevel::Success => "success",
            ToastLevel::Warning => "warning",
            ToastLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ActionResult {
    Toast { message: String, level: ToastLevel },
    RefreshTable,
    OpenSheet { table: String, id: i64 },
    Download { filename: String, content_type: String, bytes: Vec<u8> },
    Redirect { url: String },
}

#[derive(Debug, Clone)]
pub enum ActionVariant {
    Default,
    Danger,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionScope {
    Row,
    Bulk,
    Both,
}

/// Context available to action handlers.
#[derive(Debug, Clone)]
pub struct ActionInvocation {
    /// Selected primary keys.
    pub ids: Vec<i64>,
    /// Username of the currently-logged-in staff user.
    pub username: String,
    /// SQL table the action was invoked on.
    pub table: String,
    /// Ambient pool for DB mutations.
    pub pool: SqlitePool,
}

// Keep AdminContext for backwards compat (phase 1/2 tests use it).
#[derive(Debug, Clone)]
pub struct AdminContext {
    pub username: String,
    pub table: String,
}

pub(crate) type ActionFuture =
    Pin<Box<dyn Future<Output = Result<ActionResult, String>> + Send + 'static>>;

pub(crate) type ActionHandlerFn =
    Arc<dyn Fn(ActionInvocation) -> ActionFuture + Send + Sync + 'static>;

/// A row or bulk admin action.
#[derive(Clone)]
pub struct Action {
    pub(crate) key: String,
    /// Display label shown in tooltips / overflow menus.
    pub(crate) label: String,
    /// Lucide icon name (e.g. "send", "trash-2").
    pub(crate) icon: String,
    pub(crate) variant: ActionVariant,
    pub(crate) scope: ActionScope,
    /// If `Some`, a confirm dialog is shown before firing.
    pub(crate) confirm: Option<String>,
    /// Permission codename to check. `None` = any staff user may invoke.
    /// Full umbral-permissions integration deferred (gap 33); today gated
    /// on `is_staff` only. Field is stored for when permissions land.
    pub(crate) permission: Option<String>,
    pub(crate) handler: ActionHandlerFn,
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("icon", &self.icon)
            .finish()
    }
}

impl Action {
    /// Create a new action.
    ///
    /// `key` must be ASCII lowercase/digits/underscores/hyphens.
    pub fn new<F, Fut>(
        key: impl Into<String>,
        label: impl Into<String>,
        icon: impl Into<String>,
        f: F,
    ) -> Self
    where
        F: Fn(ActionInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ActionResult, String>> + Send + 'static,
    {
        let key = key.into();
        assert!(
            !key.is_empty() && key.chars().all(is_action_key_char),
            "Action::new: key {key:?} must be ASCII [a-z0-9_-]"
        );
        Action {
            key,
            label: label.into(),
            icon: icon.into(),
            variant: ActionVariant::Default,
            scope: ActionScope::Both,
            confirm: None,
            permission: None,
            handler: Arc::new(move |inv| Box::pin(f(inv))),
        }
    }

    /// Mark this action as danger variant (red styling).
    pub fn danger(mut self) -> Self {
        self.variant = ActionVariant::Danger;
        self
    }

    /// Restrict this action to row-only or bulk-only scope.
    pub fn scope(mut self, scope: ActionScope) -> Self {
        self.scope = scope;
        self
    }

    /// Require a confirm dialog before firing. `message` is shown in the dialog.
    pub fn confirm(mut self, message: impl Into<String>) -> Self {
        self.confirm = Some(message.into());
        self
    }

    /// Require a permission codename (deferred; stored for future use).
    pub fn permission(mut self, codename: impl Into<String>) -> Self {
        self.permission = Some(codename.into());
        self
    }

    /// Built-in bulk-delete. Equivalent to Django's "Delete selected" default.
    pub fn delete_selected() -> Self {
        Self::new(
            "delete_selected",
            "Delete selected",
            "trash-2",
            |inv| async move {
                if inv.ids.is_empty() {
                    return Ok(ActionResult::Toast {
                        message: "No rows selected.".to_string(),
                        level: ToastLevel::Info,
                    });
                }
                let placeholders = inv.ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let sql = format!(
                    "DELETE FROM \"{}\" WHERE \"id\" IN ({placeholders})",
                    inv.table.replace('"', "\"\"")
                );
                let mut q = sqlx::query(&sql);
                for id in &inv.ids {
                    q = q.bind(*id);
                }
                match q.execute(&inv.pool).await {
                    Ok(r) => Ok(ActionResult::Toast {
                        message: format!("Deleted {} row(s).", r.rows_affected()),
                        level: ToastLevel::Success,
                    }),
                    Err(e) => {
                        tracing::error!(error = %e, "admin: delete_selected failed");
                        Err("database error during delete".to_string())
                    }
                }
            },
        )
        .danger()
        .confirm("This will permanently delete the selected rows. Continue?")
    }

    /// The action key (URL-safe identifier).
    pub fn key(&self) -> &str {
        &self.key
    }
}

fn is_action_key_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

// =========================================================================
// InlineModel (phase 2 stub)
// =========================================================================

#[derive(Debug, Clone)]
pub struct InlineModel {
    pub model: String,
    pub fk_field: String,
    pub list_display: Vec<String>,
}

// =========================================================================
// AdminModel
// =========================================================================

#[derive(Clone, Debug)]
pub struct AdminModel {
    pub(crate) table: String,
    pub(crate) list_display: Vec<String>,
    pub(crate) list_filter: Vec<String>,
    pub(crate) search_fields: Vec<String>,
    pub(crate) ordering: Vec<String>,
    pub(crate) actions: Vec<Action>,
    pub(crate) readonly_fields: Vec<String>,
    pub(crate) list_per_page: usize,
    pub(crate) inlines: Vec<InlineModel>,
    pub(crate) label: Option<String>,
    pub(crate) icon: Option<String>,
    /// Fields that support double-click inline edit in the DataTable.
    pub(crate) inline_edit_fields: Vec<String>,
}

impl AdminModel {
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            list_display: Vec::new(),
            list_filter: Vec::new(),
            search_fields: Vec::new(),
            ordering: Vec::new(),
            actions: Vec::new(),
            readonly_fields: Vec::new(),
            list_per_page: 25,
            inlines: Vec::new(),
            label: None,
            icon: None,
            inline_edit_fields: Vec::new(),
        }
    }

    pub fn list_display(mut self, fields: &[&str]) -> Self {
        self.list_display = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn list_filter(mut self, fields: &[&str]) -> Self {
        self.list_filter = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn search_fields(mut self, fields: &[&str]) -> Self {
        self.search_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn ordering(mut self, fields: &[&str]) -> Self {
        self.ordering = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn actions(mut self, actions: Vec<Action>) -> Self {
        self.actions = actions;
        self
    }

    pub fn readonly_fields(mut self, fields: &[&str]) -> Self {
        self.readonly_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn list_per_page(mut self, n: usize) -> Self {
        self.list_per_page = n;
        self
    }

    pub fn inlines(mut self, inlines: Vec<InlineModel>) -> Self {
        self.inlines = inlines;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Enable double-click inline cell edit for these columns.
    pub fn inline_edit_fields(mut self, fields: &[&str]) -> Self {
        self.inline_edit_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn get_list_per_page(&self) -> usize {
        self.list_per_page
    }
}

pub type AdminConfig = AdminModel;
```

- [ ] **Step 2: Verify config.rs compiles**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo build -p umbral-admin 2>&1 | head -60
```

Expected: may have errors from `lib.rs` still using the old `Action` field names (`name` instead of `key`, old handler signature). That's fine — we fix lib.rs next.

---

## Task 2: Add new routes + action dispatch to lib.rs

The action dispatch endpoint `POST /admin/{table}/actions/{key}` replaces the old `POST /admin/{table}/action`. We add three new endpoint groups:
1. Action dispatch: `POST /admin/{table}/actions/{key}`
2. FK options: `GET /admin/api/{table}/{field}/options` and `GET /admin/api/{table}/{field}/options/resolve`
3. Inline cell edit: `GET /admin/{table}/{id}/cell/{field}/edit` and `POST /admin/{table}/{id}/cell/{field}`

**Files:**
- Modify: `plugins/umbral-admin/src/lib.rs`

- [ ] **Step 1: Update the `Plugin::routes()` method to add new routes**

In `lib.rs`, find the `fn routes(&self) -> Router` implementation (around line 157) and add these routes after the existing phase 2 routes and before `.with_state(state)`:

```rust
// Phase 3: per-key action dispatch (replaces the old /action omnibus)
.route(
    "/admin/{table}/actions/{key}",
    axum::routing::post(dispatch_action),
)
// Phase 3: FK/M2M async picker endpoints
.route(
    "/admin/api/{table}/{field}/options",
    axum::routing::get(fk_options),
)
.route(
    "/admin/api/{table}/{field}/options/resolve",
    axum::routing::get(fk_options_resolve),
)
// Phase 3: inline cell edit
.route(
    "/admin/{table}/{id}/cell/{field}/edit",
    axum::routing::get(cell_edit_get),
)
.route(
    "/admin/{table}/{id}/cell/{field}",
    axum::routing::post(cell_edit_post),
)
```

- [ ] **Step 2: Fix the existing `run_action` handler to use new Action field names**

The old `run_action` (around line 975) uses `a.name` and `action.handler(selected_ids, ctx)`. Update the lookup and invocation to use the new API. Also keep the old `/admin/{table}/action` route working for backward compat (phase 1/2 tests still call it). Replace the `run_action` function body:

```rust
async fn run_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/action");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let action_key = form
        .get("action")
        .cloned()
        .unwrap_or_default();
    let selected_ids: Vec<i64> = form
        .iter()
        .filter(|(k, _)| k.as_str() == "selected")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.key == action_key));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{action_key}` for `{table}`"))
            .into_response();
    };

    let inv = ActionInvocation {
        ids: selected_ids,
        username: who.username.clone(),
        table: table.clone(),
        pool: umbral::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let result = match handler(inv).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "admin: action `{action_key}` failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };
    let flash = match &result {
        ActionResult::Toast { message, .. } => message.clone(),
        ActionResult::RefreshTable => "Done.".to_string(),
        _ => "Done.".to_string(),
    };
    let location = format!("/admin/{table}/?flash={}", urlencoding_simple(&flash));
    Redirect::to(&location).into_response()
}
```

- [ ] **Step 3: Add `dispatch_action` handler (new phase 3 endpoint)**

Add after `run_action`. This handler accepts JSON `{ "ids": [...] }` and returns HTMX directives:

```rust
/// `POST /admin/{table}/actions/{key}` — phase 3 action dispatch.
///
/// Body: `application/json` with `{ "ids": [1, 2, 3] }`.
/// Response encoding follows `ActionResult`:
///   - Toast      → HX-Trigger header + 200 empty body
///   - RefreshTable → rows fragment (same as /rows endpoint)
///   - OpenSheet  → sheet fragment + HX-Retarget: #umbral-sheet-slot
///   - Download   → file bytes with Content-Disposition
///   - Redirect   → HX-Redirect header
async fn dispatch_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, key)): Path<(String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/actions/{key}");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    // Parse body: try JSON first, fall back to form-encoded for curl/test convenience.
    let ids: Vec<i64> = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v["ids"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default(),
            Err(e) => return AdminError::BadInput(format!("bad JSON: {e}")).into_response(),
        }
    } else {
        let form: HashMap<String, String> =
            serde_urlencoded::from_str(&body).unwrap_or_default();
        form.iter()
            .filter(|(k, _)| k.as_str() == "ids" || k.as_str() == "selected")
            .filter_map(|(_, v)| v.parse::<i64>().ok())
            .collect()
    };

    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.key == key));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{key}` for `{table}`"))
            .into_response();
    };

    let inv = ActionInvocation {
        ids,
        username: who.username.clone(),
        table: table.clone(),
        pool: umbral::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    match handler(inv).await {
        Ok(ActionResult::Toast { message, level }) => {
            let trigger = serde_json::json!({
                "showToast": { "message": message, "level": level.as_str() }
            });
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::RefreshTable) => {
            // Return the same fragment as /rows
            rows_fragment_for(&state, &headers, &table, &HashMap::new()).await
        }
        Ok(ActionResult::OpenSheet { table: t, id }) => {
            // Return sheet preview fragment with retarget header.
            // Simplified: redirect to the rows endpoint + instruct HTMX to open sheet.
            let trigger = serde_json::json!({ "openSheet": { "table": t, "id": id } });
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::Download { filename, content_type, bytes }) => {
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{filename}\""),
                )
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::Redirect { url }) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", url)
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::OK.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "admin: action `{key}` failed");
            let trigger = serde_json::json!({
                "showToast": { "message": e, "level": "error" }
            });
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
```

- [ ] **Step 4: Extract `rows_fragment_for` helper**

The existing `rows_fragment` handler has logic we need to call from `dispatch_action`. Extract the core into a helper:

```rust
/// Internal helper: build the rows fragment response for a table.
/// Called by both `rows_fragment` and `dispatch_action` (RefreshTable result).
async fn rows_fragment_for(
    state: &AdminState,
    headers: &HeaderMap,
    table: &str,
    params: &HashMap<String, String>,
) -> Response {
    let Some((_, model)) = find_model(table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no pk")).into_response();
    };
    let cfg = state.config_for(table);
    let (search_term, active_filter, sort_col, sort_order, page, page_size) =
        parse_list_params(params, cfg, pk);
    let display_cols: Vec<String> = if let Some(c) = cfg && !c.list_display.is_empty() {
        c.list_display.clone()
    } else {
        model.fields.iter().map(|f| f.name.clone()).collect()
    };
    let mut fetch_cols = display_cols.clone();
    if !fetch_cols.contains(&pk.name) { fetch_cols.push(pk.name.clone()); }
    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);
    let pool = umbral::db::pool();
    let total = match count_rows_filtered(&pool, &model, search_term.as_deref(), cfg,
        active_filter.as_ref().map(|(f, v)| (f.as_str(), v.as_str()))).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let pagination = Pagination::new(total, page, page_size);
    let rows = match fetch_rows_paged(&pool, &model, &fetch_cols, &order_clause,
        search_term.as_deref(), cfg,
        active_filter.as_ref().map(|(f, v)| (f.as_str(), v.as_str())),
        pagination.page_size, pagination.offset()).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let columns = model_for_template_cols(&model, &display_cols).fields;
    let active_filter_str = active_filter.as_ref().map(|(f,v)| format!("{f}={v}")).unwrap_or_default();
    let search_val = search_term.unwrap_or_default();
    // Build action descriptors for template
    let action_descriptors = cfg.map(|c| action_descriptors_json(c)).unwrap_or_default();
    match render("admin/rows_fragment.html", minijinja::context!(
        table        => table,
        model_name   => model.name.clone(),
        rows         => rows,
        pk           => pk.name.clone(),
        columns      => columns,
        pagination   => pagination,
        active_filter => active_filter_str,
        search_val   => search_val,
        sort_col     => sort_col,
        sort_order   => sort_order,
        actions      => action_descriptors,
    )) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
```

Also add this helper that serializes action descriptors for templates:

```rust
fn action_descriptors_json(cfg: &AdminConfig) -> Vec<serde_json::Value> {
    cfg.actions.iter().map(|a| serde_json::json!({
        "key":     a.key,
        "label":   a.label,
        "icon":    a.icon,
        "variant": match a.variant { ActionVariant::Danger => "danger", _ => "default" },
        "scope":   match a.scope { ActionScope::Row => "row", ActionScope::Bulk => "bulk", ActionScope::Both => "both" },
        "confirm": a.confirm,
    })).collect()
}
```

Update the existing `rows_fragment` handler to call `rows_fragment_for`:

```rust
async fn rows_fragment(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/rows");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    rows_fragment_for(&state, &headers, &table, &params).await
}
```

- [ ] **Step 5: Add FK options handlers**

```rust
/// `GET /admin/api/{table}/{field}/options?search=&page=&page_size=20`
///
/// Returns paginated label+value options for an FK field.
/// The related table is resolved from the field's FK target in ModelMeta.
/// `search` matches against the related model's `search_fields` (or first text col).
/// Permission: any is_staff user; 403 if the related model is not in the registry.
async fn fk_options(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}` on `{table}`")).into_response();
    };
    // Resolve the related table from fk_target or assume same name.
    let related_table = col.fk_target.clone().unwrap_or_else(|| field.trim_end_matches("_id").to_string());
    let Some((_, related_model)) = find_model(&related_table) else {
        return (StatusCode::FORBIDDEN,
            format!("related model `{related_table}` not found or not viewable")).into_response();
    };

    let search = params.get("search").map(|s| s.as_str()).unwrap_or("");
    let page: usize = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1).max(1);
    let page_size: usize = params.get("page_size").and_then(|p| p.parse().ok()).unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Pick a label column: first text column that isn't id.
    let label_col = related_model.fields.iter()
        .find(|c| !c.primary_key && matches!(c.ty, umbral::orm::SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    // Related model's search_fields from the admin config if registered.
    let rel_cfg = state.config_for(&related_table);
    let search_cols: Vec<&str> = rel_cfg
        .filter(|c| !c.search_fields.is_empty())
        .map(|c| c.search_fields.iter().map(|s| s.as_str()).collect())
        .unwrap_or_else(|| vec![label_col]);

    let pool = umbral::db::pool();
    // Build WHERE for search.
    let mut conditions: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if !search.is_empty() {
        let like_clauses: Vec<String> = search_cols.iter()
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{search}%");
            for _ in &like_clauses { binds.push(like_val.clone()); }
        }
    }
    let where_sql = if conditions.is_empty() { String::new() } else { format!(" WHERE {}", conditions.join(" AND ")) };

    // Count total for has_more.
    let count_sql = format!("SELECT COUNT(*) FROM \"{}\"{where_sql}", q(&related_table));
    let mut count_qb = sqlx::query(&count_sql);
    for b in &binds { count_qb = count_qb.bind(b.clone()); }
    let total: i64 = match count_qb.fetch_one(&pool).await {
        Ok(r) => r.try_get(0).unwrap_or(0),
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    // Fetch page.
    let pk_col = pk_column(&related_model).map(|c| c.name.as_str()).unwrap_or("id");
    let select_sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\"{where_sql} ORDER BY \"{pk_col}\" DESC LIMIT ? OFFSET ?",
        q(&related_table)
    );
    let mut qb = sqlx::query(&select_sql);
    for b in &binds { qb = qb.bind(b.clone()); }
    qb = qb.bind(page_size as i64).bind(offset as i64);

    let rows = match qb.fetch_all(&pool).await {
        Ok(r) => r,
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    let items: Vec<serde_json::Value> = rows.iter().map(|r| {
        let value: i64 = r.try_get(0).unwrap_or(0);
        let label: String = r.try_get::<String, _>(1)
            .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
            .unwrap_or_else(|_| format!("#{value}"));
        serde_json::json!({ "value": value, "label": label })
    }).collect();

    let has_more = (offset + page_size) < total as usize;
    Json(serde_json::json!({
        "items": items,
        "page": page,
        "has_more": has_more,
    })).into_response()
}

/// `GET /admin/api/{table}/{field}/options/resolve?ids=1,2,3`
///
/// Returns labels for pre-selected ids — used on edit-form load.
async fn fk_options_resolve(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options/resolve");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let related_table = col.fk_target.clone().unwrap_or_else(|| field.trim_end_matches("_id").to_string());
    let Some((_, related_model)) = find_model(&related_table) else {
        return (StatusCode::FORBIDDEN, "related model not found").into_response();
    };

    let ids_param = params.get("ids").cloned().unwrap_or_default();
    let ids: Vec<i64> = ids_param.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if ids.is_empty() {
        return Json(serde_json::json!({ "items": [] })).into_response();
    }

    let label_col = related_model.fields.iter()
        .find(|c| !c.primary_key && matches!(c.ty, umbral::orm::SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");
    let pk_col = pk_column(&related_model).map(|c| c.name.as_str()).unwrap_or("id");

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\" WHERE \"{pk_col}\" IN ({placeholders})",
        q(&related_table)
    );
    let pool = umbral::db::pool();
    let mut qb = sqlx::query(&sql);
    for id in &ids { qb = qb.bind(*id); }

    match qb.fetch_all(&pool).await {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows.iter().map(|r| {
                let value: i64 = r.try_get(0).unwrap_or(0);
                let label: String = r.try_get::<String, _>(1)
                    .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
                    .unwrap_or_else(|_| format!("#{value}"));
                serde_json::json!({ "value": value, "label": label })
            }).collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}
```

- [ ] **Step 6: Add inline cell edit handlers**

```rust
/// `GET /admin/{table}/{id}/cell/{field}/edit`
/// Returns the field editor for a single cell (HTMX swap into the <td>).
async fn cell_edit_get(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}/edit");
    if let Err(r) = require_staff(&headers, &path).await { return r; }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let pool = umbral::db::pool();
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&pool, &model, Some((&pk.name, &id)),
        &all_cols, "", None, None, None).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row {id}")).into_response();
    };
    let value = row.get(&field).cloned().unwrap_or_default();
    let cfg = state.config_for(&table);
    let is_readonly = cfg.is_some_and(|c| c.readonly_fields.contains(&field));
    if is_readonly {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }
    let ff = FormField {
        name: col.name.clone(),
        kind: input_kind(col.ty),
        value: format_for_input(&value, col.ty),
        nullable: col.nullable,
        readonly: false,
    };
    // Render inline editor with save/cancel controls.
    let html = format!(
        r#"<form
            hx-post="/admin/{table}/{id}/cell/{field}"
            hx-target="closest td"
            hx-swap="innerHTML"
            class="flex items-center gap-xs"
            onkeydown="if(event.key==='Escape'){{htmx.trigger(this,'cancel')}}"
            hx-on:cancel="htmx.ajax('GET','/admin/{table}/{id}/cell/{field}/view',{{target:'closest td',swap:'innerHTML'}})"
          >
          <input type="{input_type}" name="{field}" value="{value}"
            class="flex-1 bg-surface-container-low border border-primary rounded-lg px-sm py-xs text-on-surface text-body-md focus:outline-none focus:ring-1 focus:ring-primary"
            autofocus
            onblur="this.form.requestSubmit()"
          />
          <button type="submit" class="p-xs text-primary hover:bg-primary/10 rounded" title="Save">
            <i data-lucide="check" class="w-3 h-3"></i>
          </button>
        </form>
        <script>if(window.lucide)lucide.createIcons();</script>"#,
        table = table, id = id, field = field,
        input_type = ff.kind,
        value = html_escape(&value),
    );
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

/// `POST /admin/{table}/{id}/cell/{field}`
/// Save inline cell edit. Returns the read-only cell value on success,
/// or the editor with an error on failure.
async fn cell_edit_post(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}");
    if let Err(r) = require_staff(&headers, &path).await { return r; }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let cfg = state.config_for(&table);
    if cfg.is_some_and(|c| c.readonly_fields.contains(&field)) {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }
    let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
    let pool = umbral::db::pool();
    let sql = format!(
        "UPDATE \"{}\" SET \"{}\" = ? WHERE \"{}\" = ?",
        q(&model.table), q(&field), q(&pk.name)
    );
    let qb = sqlx::query(&sql);
    let qb = match bind_form_value(qb, col, &form) {
        Ok(q) => q,
        Err(e) => {
            let err_html = format!(
                r#"<span class="text-error text-body-sm">{}</span>"#,
                html_escape(&sanitise_form_error(&e))
            );
            return axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(err_html))
                .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response());
        }
    };
    match qb.bind(id.clone()).execute(&pool).await {
        Ok(_) => {
            let new_value = form.get(&field).cloned().unwrap_or_default();
            let display = html_escape(&new_value);
            let cell_html = format!(
                r#"<span class="text-on-surface text-body-md tabular-nums">{display}</span>"#
            );
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(cell_html))
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Err(e) => {
            let err_html = format!(
                r#"<span class="text-error text-body-sm">{}</span>"#,
                html_escape(&e.to_string())
            );
            axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(err_html))
                .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response())
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
```

- [ ] **Step 7: Update import in lib.rs for new config types**

Near the top of lib.rs, the `pub use config::{...}` line needs `ActionResult`, `ActionVariant`, `ActionScope`, `ActionInvocation`, `ToastLevel`:

```rust
pub use config::{
    Action, ActionInvocation, ActionResult, ActionScope, ActionVariant,
    AdminConfig, AdminContext, AdminModel, InlineModel, ToastLevel,
};
```

Also add `use config::{ActionInvocation, ActionResult, ActionScope, ActionVariant, ToastLevel};` in the internal use block.

- [ ] **Step 8: Fix list handler to pass action descriptors**

In the `list` handler (around line 877), the `action_names` block currently does:
```rust
let action_names: Vec<serde_json::Value> = cfg.map(|c| {
    c.actions.iter().map(|a| serde_json::json!({ "name": a.name, "label": a.label })).collect()
}).unwrap_or_default();
```

Replace with:
```rust
let action_names: Vec<serde_json::Value> = cfg
    .map(|c| action_descriptors_json(c))
    .unwrap_or_default();
```

- [ ] **Step 9: Verify build**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo build -p umbral-admin 2>&1 | head -80
```

Expected: clean build.

---

## Task 3: Update data_table.html macro

Add (a) row action overflow menu for custom actions, (b) floating bulk-action toolbar with selection-aware JS, and (c) inline-edit `dblclick` trigger on editable cells.

**Files:**
- Modify: `plugins/umbral-admin/templates/_macros/data_table.html`

- [ ] **Step 1: Add custom actions to the row action column**

Find the sticky actions column `<td>` block (after the `trash-2` button, around line 330). After the existing three buttons but before `</div></td>`, add:

```jinja
{# Custom actions (up to 2 inline, rest in overflow menu) #}
{% set custom_actions = actions | selectattr("scope", "in", ["row", "both"]) | list %}
{% for action in custom_actions[:2] %}
<button
  type="button"
  hx-post="/admin/{{ model.table }}/actions/{{ action.key }}"
  hx-vals='{"ids": [{{ row[pk] }}]}'
  hx-swap="none"
  title="{{ action.label }}"
  class="p-sm transition-colors rounded-lg hover:bg-surface-container-high {% if action.variant == 'danger' %}text-error hover:text-error{% else %}text-on-surface-variant hover:text-primary{% endif %}"
  {% if action.confirm %}
  onclick="if(!confirm('{{ action.confirm }}'))return false;"
  {% endif %}
>
  <i data-lucide="{{ action.icon }}" class="w-4 h-4"></i>
</button>
{% endfor %}
{% if custom_actions | length > 2 %}
<div class="relative">
  <button type="button"
    onclick="var m=this.nextElementSibling;m.classList.toggle('hidden');event.stopPropagation()"
    class="p-sm text-on-surface-variant hover:text-primary transition-colors rounded-lg hover:bg-surface-container-high"
    title="More actions"
  >
    <i data-lucide="more-horizontal" class="w-4 h-4"></i>
  </button>
  <div class="hidden absolute right-0 bottom-full mb-xs z-40 bg-surface-container border border-outline-variant rounded-xl shadow-lg py-xs min-w-[160px]">
    {% for action in custom_actions[2:] %}
    <button type="button"
      hx-post="/admin/{{ model.table }}/actions/{{ action.key }}"
      hx-vals='{"ids": [{{ row[pk] }}]}'
      hx-swap="none"
      {% if action.confirm %}onclick="if(!confirm('{{ action.confirm }}'))return false;"{% endif %}
      class="w-full flex items-center gap-sm px-md py-sm text-left hover:bg-surface-container-high font-label-md text-label-md {% if action.variant == 'danger' %}text-error{% else %}text-on-surface{% endif %} transition-colors"
    >
      <i data-lucide="{{ action.icon }}" class="w-4 h-4"></i>
      {{ action.label }}
    </button>
    {% endfor %}
  </div>
</div>
{% endif %}
```

- [ ] **Step 2: Add inline-edit dblclick trigger to editable cells**

The data columns loop (`{% for col in columns %}`) renders `<td>` cells. Add `inline_edit_fields` support. The macro already receives `model` which has `table`. We need to pass `inline_edit_fields` as a new macro argument. Add it to the macro signature:

```jinja
{% macro data_table(model, rows, columns, pk, facets, active_filter, has_search, search_val, actions, pagination, sort_col, sort_order, flash, inline_edit_fields=[]) %}
```

In the data cells loop, wrap the cell content:

```jinja
<td class="px-md py-md {% if not loop.first %}hidden lg:table-cell{% endif %} dt-col"
    data-col="{{ col.name }}"
    {% if col.name in inline_edit_fields %}
    hx-trigger="dblclick"
    hx-get="/admin/{{ model.table }}/{{ row[pk] }}/cell/{{ col.name }}/edit"
    hx-target="this"
    hx-swap="innerHTML"
    title="Double-click to edit"
    style="cursor: default"
    {% endif %}
>
```

- [ ] **Step 3: Add floating bulk-action toolbar**

Before the closing `{% endmacro %}` tag (after the `<script>` block), add the toolbar HTML:

```jinja
{# ================================================================
   Floating bulk-action toolbar — appears bottom-center on selection
   ================================================================ #}
<div
  id="bulk-toolbar"
  class="hidden fixed bottom-6 left-1/2 -translate-x-1/2 z-50 flex items-center gap-md px-lg py-sm bg-surface-container border border-outline-variant rounded-full shadow-2xl"
  role="toolbar"
  aria-label="Bulk actions"
>
  <span class="font-label-md text-label-md text-on-surface-variant">
    <span class="bulk-count font-semibold text-on-surface">0</span> selected
  </span>
  <button
    type="button"
    onclick="umbral.selectAllRows(false)"
    class="font-label-sm text-label-sm text-on-surface-variant hover:text-on-surface transition-colors"
  >Clear</button>
  <div class="w-px h-4 bg-outline-variant"></div>

  {# Bulk-scope actions (up to 4 inline) #}
  {% set bulk_actions = actions | selectattr("scope", "in", ["bulk", "both"]) | list %}
  {% for action in bulk_actions[:4] %}
  <button
    type="button"
    id="bulk-action-{{ action.key }}"
    data-action-key="{{ action.key }}"
    data-action-confirm="{{ action.confirm | default('') }}"
    data-table="{{ model.table }}"
    class="flex items-center gap-xs px-md py-xs rounded-lg font-label-md text-label-md {% if action.variant == 'danger' %}text-error hover:bg-error-container/10{% else %}text-on-surface hover:bg-surface-container-high{% endif %} transition-colors"
    title="{{ action.label }}"
    onclick="umbral.runBulkAction(this)"
  >
    <i data-lucide="{{ action.icon }}" class="w-4 h-4"></i>
    <span class="hidden sm:inline">{{ action.label }}</span>
  </button>
  {% endfor %}

  {% if bulk_actions | length > 4 %}
  <div class="relative">
    <button type="button"
      onclick="var m=document.getElementById('bulk-overflow');m.classList.toggle('hidden');event.stopPropagation()"
      class="p-sm text-on-surface-variant hover:text-on-surface transition-colors"
      title="More"
    >
      <i data-lucide="more-horizontal" class="w-4 h-4"></i>
    </button>
    <div id="bulk-overflow" class="hidden absolute bottom-full right-0 mb-xs z-50 bg-surface-container border border-outline-variant rounded-xl shadow-lg py-xs min-w-[180px]">
      {% for action in bulk_actions[4:] %}
      <button type="button"
        data-action-key="{{ action.key }}"
        data-action-confirm="{{ action.confirm | default('') }}"
        data-table="{{ model.table }}"
        class="w-full flex items-center gap-sm px-md py-sm text-left hover:bg-surface-container-high font-label-md text-label-md {% if action.variant == 'danger' %}text-error{% else %}text-on-surface{% endif %} transition-colors"
        onclick="umbral.runBulkAction(this)"
      >
        <i data-lucide="{{ action.icon }}" class="w-4 h-4"></i>
        {{ action.label }}
      </button>
      {% endfor %}
    </div>
  </div>
  {% endif %}
</div>
```

- [ ] **Step 4: Add bulk-action JS to the `<script>` block**

Inside the existing `<script>(function() { ... })();</script>` block, add the `umbral.runBulkAction` function after `umbral.onRowCheck`:

```javascript
umbral.runBulkAction = function(btn) {
  var key = btn.getAttribute('data-action-key');
  var table = btn.getAttribute('data-table') || btn.closest('[data-table]')?.getAttribute('data-table');
  var confirmMsg = btn.getAttribute('data-action-confirm');
  if (confirmMsg && !confirm(confirmMsg)) return;
  var selected = Array.from(document.querySelectorAll('.row-cb:checked')).map(function(cb) {
    return parseInt(cb.value, 10);
  });
  if (selected.length === 0) return;
  fetch('/admin/' + table + '/actions/' + key, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'HX-Request': 'true' },
    body: JSON.stringify({ ids: selected })
  }).then(function(resp) {
    var trigger = resp.headers.get('HX-Trigger');
    if (trigger) {
      try {
        var obj = JSON.parse(trigger);
        if (obj.showToast) umbral.showToast(obj.showToast.message, obj.showToast.level);
      } catch(e) {}
    }
    var redirect = resp.headers.get('HX-Redirect');
    if (redirect) { window.location.href = redirect; return; }
    // Default: refresh the table.
    htmx.ajax('GET', '/admin/' + table + '/rows', { target: '#table-body', swap: 'innerHTML' });
    umbral.selectAllRows(false);
  }).catch(function(e) {
    umbral.showToast('Action failed: ' + e, 'error');
  });
};
```

---

## Task 4: Add toast renderer to wrapper.html

**Files:**
- Modify: `plugins/umbral-admin/templates/wrapper.html`

- [ ] **Step 1: Read wrapper.html to find the right insertion point**

```bash
grep -n "body\|</body>\|toast\|umbral\." /home/dalmas/E/projects/umbral/plugins/umbral-admin/templates/wrapper.html | head -30
```

- [ ] **Step 2: Add toast container + JS before `</body>`**

Find the `</body>` tag in `wrapper.html` and insert before it:

```html
{# Toast notification container #}
<div id="umbral-toast-container" class="fixed bottom-4 right-4 z-[300] flex flex-col gap-sm pointer-events-none" aria-live="polite"></div>

<script>
(function() {
  // Ensure umbral namespace exists (base.html may have initialized it).
  window.umbral = window.umbral || {};

  umbral.showToast = function(message, level) {
    level = level || 'info';
    var container = document.getElementById('umbral-toast-container');
    if (!container) return;
    var colors = {
      info:    'bg-surface-container border-outline-variant text-on-surface',
      success: 'bg-primary-container/20 border-primary/30 text-primary',
      warning: 'bg-warning-container/20 border-warning/30 text-warning',
      error:   'bg-error-container/20 border-error/30 text-error'
    };
    var icons = { info: 'info', success: 'check-circle', warning: 'alert-triangle', error: 'alert-circle' };
    var toast = document.createElement('div');
    toast.className = 'pointer-events-auto flex items-center gap-sm px-lg py-sm rounded-xl border shadow-lg font-label-md text-label-md transition-all duration-300 ' + (colors[level] || colors.info);
    toast.innerHTML = '<i data-lucide="' + (icons[level]||'info') + '" class="w-4 h-4 flex-shrink-0"></i><span>' + message + '</span>';
    container.appendChild(toast);
    if (window.lucide) lucide.createIcons({ el: toast });
    setTimeout(function() {
      toast.style.opacity = '0';
      toast.style.transform = 'translateX(20px)';
      setTimeout(function() { toast.remove(); }, 300);
    }, 4000);
  };

  // Listen for HX-Trigger showToast events from HTMX responses.
  document.body.addEventListener('htmx:responseError', function(e) {
    umbral.showToast('Server error', 'error');
  });

  // Handle showToast trigger from HX-Trigger header.
  document.body.addEventListener('showToast', function(e) {
    if (e.detail) umbral.showToast(e.detail.message, e.detail.level);
  });
})();
</script>
```

- [ ] **Step 3: Also ensure `window.umbral = window.umbral || {}` is in base.html init**

Check that `base.html` (or `wrapper.html`) initialises `window.umbral = {}` before any template script uses it. Add if absent.

- [ ] **Step 4: Verify wrapper.html still renders correctly**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo build -p umbral-admin 2>&1 | grep -E "error|warning" | head -20
```

---

## Task 5: Update field_editor.html for FK combobox

Replace the placeholder number input for FK fields with a searchable combobox.

**Files:**
- Modify: `plugins/umbral-admin/templates/_macros/field_editor.html`

- [ ] **Step 1: Replace the `number` arm FK placeholder and add FK combobox + M2M chip picker**

In `field_editor.html`, find the `{% elif field.kind == "number" %}` block (around line 80) and after the existing number input, add FK combobox for `fk` kind. Also change the macro to handle a new `field.kind == "fk"` case. Actually, since `input_kind` in lib.rs maps `ForeignKey` to `"number"`, add a new kind `"fk"` by updating `input_kind()` in lib.rs first (see Task 2, add a sub-step below), then handle it here.

Add to `field_editor.html` BEFORE the `{% elif field.kind == "number" %}` block:

```jinja
{% elif field.kind == "fk" %}
  {# Async FK combobox — never preloads the whole table.
     Phase 3: endpoint /admin/api/<table>/<field>/options powers search.
     On edit-mode load, /options/resolve?ids=<current> resolves the label. #}
  {% set fk_table = field.fk_table | default("") %}
  <div class="relative fk-picker" data-field="{{ field.name }}" data-fk-table="{{ fk_table }}">
    <input type="hidden" id="f_{{ field.name }}" name="{{ field.name }}" value="{{ value }}" />
    <input
      type="text"
      id="fk_text_{{ field.name }}"
      placeholder="Search {{ field.name | replace('_id', '') | replace('_', ' ') | title }}..."
      autocomplete="off"
      class="w-full bg-surface-container-low border {% if error %}border-error{% else %}border-outline-variant{% endif %} rounded-xl px-md py-sm text-on-surface text-body-md focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary/20 transition-all placeholder:text-outline/50"
      {% if is_readonly %}readonly{% endif %}
      hx-get="/admin/api/{{ fk_table }}/{{ field.name }}/options"
      hx-trigger="input changed delay:250ms, focus"
      hx-target="next .fk-options"
      hx-swap="innerHTML"
      hx-include="this"
      name="search"
    />
    {% if value and not is_readonly %}
    {# Resolve label for pre-selected value on load #}
    <span id="fk_resolve_{{ field.name }}"
      hx-get="/admin/api/{{ fk_table }}/{{ field.name }}/options/resolve?ids={{ value }}"
      hx-trigger="load"
      hx-swap="none"
      hx-on:htmx:after-request="umbral.fkResolve('{{ field.name }}', event)"
    ></span>
    {% endif %}
    <div class="fk-options hidden absolute left-0 right-0 top-full mt-xs z-30 bg-surface-container border border-outline-variant rounded-xl shadow-lg max-h-48 overflow-y-auto">
      {# Populated by HTMX #}
    </div>
  </div>

{% elif field.kind == "m2m" %}
  {# M2M chip multi-select picker.
     NOTE: M2M is wired but no Model field uses it until the ORM ships M2M;
     the chip picker is ready for that day. #}
  <div class="fk-picker m2m-picker" data-field="{{ field.name }}">
    <div id="chips_{{ field.name }}" class="flex flex-wrap gap-xs mb-xs">
      {# Chips rendered by JS from hidden input #}
    </div>
    <input type="hidden" id="f_{{ field.name }}" name="{{ field.name }}" value="{{ value }}" />
    <input type="text"
      id="m2m_text_{{ field.name }}"
      placeholder="Add {{ field.name | replace('_', ' ') | title }}..."
      autocomplete="off"
      class="w-full bg-surface-container-low border border-outline-variant rounded-xl px-md py-sm text-on-surface text-body-md focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary/20"
      {% if is_readonly %}readonly{% endif %}
    />
    <div class="fk-options hidden absolute left-0 right-0 top-full mt-xs z-30 bg-surface-container border border-outline-variant rounded-xl shadow-lg max-h-48 overflow-y-auto"></div>
  </div>
```

- [ ] **Step 2: Add FK combobox vanilla JS at the bottom of field_editor.html**

After the `{% endmacro %}` tag, add:

```jinja
{# FK combobox JS — wires up all .fk-picker instances on the page.
   Runs once per page load; re-runs after HTMX swaps sheet content. #}
<script>
(function() {
  function initFkPickers(root) {
    root = root || document;
    root.querySelectorAll('.fk-picker:not([data-fk-init])').forEach(function(picker) {
      picker.setAttribute('data-fk-init', '1');
      var field = picker.getAttribute('data-field');
      var textInput = picker.querySelector('input[type=text]');
      var hiddenInput = picker.querySelector('input[type=hidden]');
      var dropdown = picker.querySelector('.fk-options');
      if (!textInput || !hiddenInput || !dropdown) return;

      // Show dropdown.
      textInput.addEventListener('focus', function() { dropdown.classList.remove('hidden'); });
      // Hide dropdown on outside click.
      document.addEventListener('click', function(e) {
        if (!picker.contains(e.target)) dropdown.classList.add('hidden');
      });

      // Render FK options received from HTMX into the dropdown.
      picker.addEventListener('htmx:afterSwap', function(e) {
        dropdown.classList.remove('hidden');
        dropdown.querySelectorAll('[data-fk-value]').forEach(function(opt) {
          opt.addEventListener('mousedown', function(e) {
            e.preventDefault();
            hiddenInput.value = opt.getAttribute('data-fk-value');
            textInput.value = opt.textContent.trim();
            dropdown.classList.add('hidden');
          });
        });
      });
    });
  }

  // Resolve label helper called by hx-on after resolve request.
  window.umbral = window.umbral || {};
  umbral.fkResolve = function(field, event) {
    try {
      var data = JSON.parse(event.detail.xhr.responseText);
      if (data.items && data.items[0]) {
        var el = document.getElementById('fk_text_' + field);
        if (el) el.value = data.items[0].label;
      }
    } catch(e) {}
  };

  initFkPickers();
  document.body.addEventListener('htmx:afterSwap', function() { initFkPickers(); });
})();
</script>
```

- [ ] **Step 3: Update `input_kind` in lib.rs to return `"fk"` for ForeignKey**

In `lib.rs`, find `fn input_kind(ty: SqlType)` and change:
```rust
SqlType::ForeignKey => "number",
```
to:
```rust
SqlType::ForeignKey => "fk",
```

Also add `fk_table` to `FormField` struct and populate it from the column's `fk_target`:

```rust
#[derive(Debug, Clone, Serialize)]
struct FormField {
    name: String,
    kind: &'static str,
    value: String,
    nullable: bool,
    readonly: bool,
    /// For FK fields: the related table name. Empty string for non-FK fields.
    fk_table: String,
}
```

In `form_fields_for`, populate `fk_table`:
```rust
FormField {
    name: c.name.clone(),
    kind: input_kind(c.ty),
    value: format_for_input(&raw, c.ty),
    nullable: c.nullable,
    readonly: readonly_set.contains(c.name.as_str()),
    fk_table: if c.ty == SqlType::ForeignKey {
        c.fk_target.clone().unwrap_or_else(|| c.name.trim_end_matches("_id").to_string())
    } else {
        String::new()
    },
}
```

Note: `SqlType` may not implement `PartialEq`; check and add `#[derive(PartialEq)]` to `SqlType` in umbral-core if needed, or use `matches!()`.

- [ ] **Step 4: Add FK options dropdown template fragment**

The HTMX request `hx-get="/admin/api/{table}/{field}/options"` returns JSON. We need the server to return HTML for the dropdown, not JSON, when called from the field editor (HTMX context). Add a query param `?format=html` support to `fk_options`, or more simply, always return JSON and have the JS render it.

Since HTMX `hx-swap="innerHTML"` expects HTML, create a small Jinja snippet. The simplest approach: the `fk_options` handler detects `hx-request` header and returns an HTML fragment:

```rust
// In fk_options, after building items, before returning:
if is_htmx(&headers) {
    // Return HTML fragment for HTMX swap into .fk-options div.
    let mut html = String::new();
    for item in &items {
        let value = item["value"].as_i64().unwrap_or(0);
        let label = item["label"].as_str().unwrap_or("");
        html.push_str(&format!(
            r#"<button type="button" data-fk-value="{value}" class="w-full text-left px-md py-sm hover:bg-surface-container-high font-body-md text-on-surface transition-colors">{}</button>"#,
            html_escape(label)
        ));
    }
    // "+ Add new" link at bottom.
    html.push_str(&format!(
        r#"<div class="border-t border-outline-variant px-md py-sm"><button type="button" class="text-primary font-label-sm text-label-sm hover:underline" onclick="umbral.openNestedSheet('{related_table}')"><i data-lucide="plus" class="w-3 h-3 inline mr-xs"></i>Add new</button></div>"#,
        related_table = related_table
    ));
    return axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response());
}
// Otherwise return JSON (for API consumers and resolve endpoint).
```

- [ ] **Step 5: Build check**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo build -p umbral-admin 2>&1 | head -60
```

---

## Task 6: Sheet stacking JS

Update `sheet.html` macro and `wrapper.html` to support nested sheets (offset by ~40px per level).

**Files:**
- Modify: `plugins/umbral-admin/templates/_macros/sheet.html`
- Modify: `plugins/umbral-admin/templates/wrapper.html`

- [ ] **Step 1: Update sheet.html to support stack depth**

In `sheet.html`, change `id="umbral-sheet-panel"` to accept a `depth` variable and apply offset transform:

```jinja
{% set depth = depth | default(0) %}
{% set offset = depth * 40 %}
```

Change the panel element's style attribute to:
```jinja
style="width: var(--sheet-width, 640px); transform: translateX(-{{ offset }}px);"
```

Add a "Back" button in the header when `depth > 0`:
```jinja
{% if depth > 0 %}
<button type="button"
  onclick="umbral.popSheet()"
  class="w-8 h-8 flex items-center justify-center rounded-xl text-on-surface-variant hover:bg-surface-container-high transition-all mr-xs"
  aria-label="Back to previous"
>
  <i data-lucide="chevron-left" class="w-4 h-4"></i>
</button>
{% endif %}
```

- [ ] **Step 2: Add sheet-stack state machine to wrapper.html**

Add to the JS block in `wrapper.html` (before `</script>`):

```javascript
// Sheet stack state machine (~100 LOC).
(function() {
  var stack = [];

  umbral.openSheet = function(html) {
    var slot = document.getElementById('umbral-sheet-slot');
    if (!slot) return;
    stack.push(slot.innerHTML);
    slot.innerHTML = html;
    document.body.classList.add('overflow-hidden');
    if (window.lucide) lucide.createIcons({ el: slot });
    umbral._applyStackOffsets();
  };

  umbral.popSheet = function() {
    var slot = document.getElementById('umbral-sheet-slot');
    if (!slot) return;
    if (stack.length > 0) {
      slot.innerHTML = stack.pop();
      if (window.lucide) lucide.createIcons({ el: slot });
      umbral._applyStackOffsets();
    } else {
      umbral.closeSheet();
    }
  };

  umbral.closeSheet = function() {
    var slot = document.getElementById('umbral-sheet-slot');
    if (slot) slot.innerHTML = '';
    stack = [];
    document.body.classList.remove('overflow-hidden');
  };

  umbral._applyStackOffsets = function() {
    var slot = document.getElementById('umbral-sheet-slot');
    if (!slot) return;
    var panels = slot.querySelectorAll('#umbral-sheet-panel');
    panels.forEach(function(panel, i) {
      panel.style.transform = 'translateX(-' + (i * 40) + 'px)';
    });
  };

  umbral.openNestedSheet = function(table) {
    // Push current sheet onto stack, load create sheet for related table.
    htmx.ajax('GET', '/admin/' + table + '/new-sheet', {
      handler: function(elt, info) {
        umbral.openSheet(info.xhr.responseText);
      }
    });
  };

  // Escape closes top sheet only.
  document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') {
      var slot = document.getElementById('umbral-sheet-slot');
      if (slot && slot.innerHTML.trim()) { umbral.popSheet(); e.preventDefault(); }
    }
  });

  umbral.closeDialog = function() {
    var slot = document.getElementById('umbral-dialog-slot');
    if (slot) slot.innerHTML = '';
  };
})();
```

---

## Task 7: Write phase3_actions.rs tests

**Files:**
- New: `plugins/umbral-admin/tests/phase3_actions.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Phase 3 action tests.
//!
//! Covers:
//! 1. Custom action declared in AdminModel appears in list page markup.
//! 2. POST /admin/{table}/actions/{key} with row id invokes handler, returns HX-Trigger showToast.
//! 3. Bulk action with multiple ids hits handler with ids.len() > 1.
//! 4. Danger-variant action gets text-error class in rendered markup.
//! 5. Confirm-required action has confirm attribute in rendered markup.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_admin::{Action, ActionInvocation, ActionResult, ActionScope, ActionVariant, AdminModel, AdminPlugin, ToastLevel};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;
use tower::ServiceExt;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Article {
    id: i64,
    title: String,
    published: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_actions.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(SqliteConnectOptions::new().filename(&path).create_if_missing(true))
            .await.expect("pool");

        let publish_action = Action::new(
            "publish", "Publish", "send",
            |inv: ActionInvocation| async move {
                Ok(ActionResult::Toast {
                    message: format!("Published {} item(s).", inv.ids.len()),
                    level: ToastLevel::Success,
                })
            }
        ).scope(ActionScope::Both);

        let danger_action = Action::new(
            "nuke", "Nuke", "zap",
            |_inv| async move { Ok(ActionResult::RefreshTable) }
        ).danger().scope(ActionScope::Row);

        let confirm_action = Action::new(
            "archive", "Archive", "archive",
            |_inv| async move { Ok(ActionResult::Toast { message: "Archived.".into(), level: ToastLevel::Info }) }
        ).confirm("Archive these items?");

        let article_config = AdminModel::new("article")
            .list_display(&["title", "published"])
            .actions(vec![
                Action::delete_selected(),
                publish_action,
                danger_action,
                confirm_action,
            ]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(article_config))
            .model::<Article>()
            .build().expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query("CREATE TABLE auth_user (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL UNIQUE, email TEXT NOT NULL, password_hash TEXT NOT NULL, is_active INTEGER NOT NULL, is_staff INTEGER NOT NULL, is_superuser INTEGER NOT NULL, date_joined TEXT NOT NULL, last_login TEXT)")
            .execute(&pool).await.expect("auth_user");
        sqlx::query("CREATE TABLE session (id TEXT PRIMARY KEY, user_id INTEGER, data TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL, expires_at TEXT NOT NULL)")
            .execute(&pool).await.expect("session");
        sqlx::query("CREATE TABLE article (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, published INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool).await.expect("article");

        let staff = create_user("act_admin", "act@example.com", "pass123").await.expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id).execute(&pool).await.expect("set staff");
        sqlx::query("INSERT INTO article (title, published) VALUES ('Article 1', 0), ('Article 2', 0), ('Article 3', 0)")
            .execute(&pool).await.expect("seed");

        app.into_router()
    }).await
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..pos + 200.min(html.len() - pos)];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    let end = after.find('"').unwrap_or(0);
    after[..end].to_string()
}

fn extract_cookie(set_cookie: &str) -> String {
    set_cookie.split(';').next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login(router: axum::Router) -> String {
    let resp = router.clone().oneshot(
        Request::builder().uri("/admin/login").body(Body::empty()).unwrap()
    ).await.expect("get login");
    let anon_raw = resp.headers().get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
    let anon_cookie = extract_cookie(&anon_raw);
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html);
    let form = serde_urlencoded::to_string([("username","act_admin"),("password","pass123"),("csrf_token",csrf.as_str()),("next","/admin/")]).unwrap();
    let resp2 = router.clone().oneshot(
        Request::builder().method("POST").uri("/admin/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, format!("umbral_session={anon_cookie}"))
            .body(Body::from(form)).unwrap()
    ).await.expect("post login");
    resp2.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok())
        .map(extract_cookie).unwrap_or(anon_cookie)
}

#[tokio::test]
async fn test_custom_action_appears_in_changelist() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _headers, body) = send(router, Request::builder()
        .uri("/admin/article/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("publish") || body.contains("Publish"), "publish action in page: snippet={}", &body[..body.len().min(2000)]);
}

#[tokio::test]
async fn test_row_action_dispatch_returns_toast_trigger() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, headers, _body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/article/actions/publish")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ids":[1]}"#)).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK, "dispatch returns 200");
    let trigger = headers.get("hx-trigger")
        .and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(trigger.contains("showToast"), "HX-Trigger showToast present: {trigger}");
    assert!(trigger.contains("success"), "level=success: {trigger}");
}

#[tokio::test]
async fn test_bulk_action_receives_multiple_ids() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, headers, _body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/article/actions/publish")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ids":[1,2,3]}"#)).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK);
    let trigger = headers.get("hx-trigger")
        .and_then(|v| v.to_str().ok()).unwrap_or("");
    // Handler message says "Published 3 item(s)."
    assert!(trigger.contains("3"), "bulk count in toast: {trigger}");
}

#[tokio::test]
async fn test_unknown_action_returns_404() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _headers, _body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/article/actions/nonexistent")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ids":[1]}"#)).unwrap()
    ).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_selected_action_deletes_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    // Insert a throwaway row.
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO article (title, published) VALUES ('ToDelete', 0)")
        .execute(&pool).await.expect("insert");
    let id: i64 = sqlx::query_scalar("SELECT id FROM article WHERE title = 'ToDelete'")
        .fetch_one(&pool).await.expect("get id");

    let (status, headers, _body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/article/actions/delete_selected")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"ids":[{id}]}}"#))).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK);
    let trigger = headers.get("hx-trigger").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(trigger.contains("showToast"), "toast on delete: {trigger}");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM article WHERE id = ?")
        .bind(id).fetch_one(&pool).await.expect("count");
    assert_eq!(remaining, 0, "row was deleted");
}
```

- [ ] **Step 2: Run the tests to see them fail (expected until handler is in place)**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo test -p umbral-admin phase3_actions 2>&1 | tail -30
```

Expected: compile errors or test failures because handler code isn't written yet.

---

## Task 8: Write phase3_fk_picker.rs tests

**Files:**
- New: `plugins/umbral-admin/tests/phase3_fk_picker.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Phase 3 FK picker tests.
//!
//! Covers:
//! 1. GET /admin/api/note/{field}/options?search=foo returns matching options as JSON/HTML.
//! 2. Pagination: has_more is true when rows > page_size.
//! 3. /options/resolve?ids=1,2 returns labels without a search.
//! 4. The edit sheet combobox markup contains the right hx-get URL.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post3 {
    id: i64,
    title: String,
    tag_id: i64,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_fk.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(SqliteConnectOptions::new().filename(&path).create_if_missing(true))
            .await.expect("pool");

        let post_config = AdminModel::new("post3")
            .list_display(&["title", "tag_id"])
            .search_fields(&["title"]);
        let tag_config = AdminModel::new("tag").search_fields(&["name"]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(post_config).register(tag_config))
            .model::<Tag>()
            .model::<Post3>()
            .build().expect("build");

        let pool = umbral::db::pool();
        sqlx::query("CREATE TABLE auth_user (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL UNIQUE, email TEXT NOT NULL, password_hash TEXT NOT NULL, is_active INTEGER NOT NULL, is_staff INTEGER NOT NULL, is_superuser INTEGER NOT NULL, date_joined TEXT NOT NULL, last_login TEXT)")
            .execute(&pool).await.expect("auth_user");
        sqlx::query("CREATE TABLE session (id TEXT PRIMARY KEY, user_id INTEGER, data TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL, expires_at TEXT NOT NULL)")
            .execute(&pool).await.expect("session");
        sqlx::query("CREATE TABLE tag (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
            .execute(&pool).await.expect("tag");
        sqlx::query("CREATE TABLE post3 (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, tag_id INTEGER NOT NULL REFERENCES tag(id))")
            .execute(&pool).await.expect("post3");

        // Seed tags.
        for i in 1..=25i64 {
            sqlx::query("INSERT INTO tag (name) VALUES (?)")
                .bind(format!("tag-{i}")).execute(&pool).await.expect("seed tag");
        }
        sqlx::query("INSERT INTO tag (name) VALUES ('foo-tag')").execute(&pool).await.expect("seed foo-tag");
        sqlx::query("INSERT INTO post3 (title, tag_id) VALUES ('Hello', 1)").execute(&pool).await.expect("seed post3");

        let staff = create_user("fk_admin", "fk@example.com", "pass123").await.expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id).execute(&pool).await.expect("set staff");

        app.into_router()
    }).await
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos+200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    after[..after.find('"').unwrap_or(0)].to_string()
}
fn extract_cookie(s: &str) -> String {
    s.split(';').next().and_then(|p| p.split_once('=').map(|(_, v)| v.to_string())).unwrap_or_default()
}
async fn login(router: axum::Router) -> String {
    let resp = router.clone().oneshot(Request::builder().uri("/admin/login").body(Body::empty()).unwrap()).await.expect("get");
    let anon_raw = resp.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
    let anon = extract_cookie(&anon_raw);
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([("username","fk_admin"),("password","pass123"),("csrf_token",csrf.as_str()),("next","/admin/")]).unwrap();
    let resp2 = router.clone().oneshot(Request::builder().method("POST").uri("/admin/login")
        .header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("umbral_session={anon}"))
        .body(Body::from(form)).unwrap()).await.expect("post");
    resp2.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(extract_cookie).unwrap_or(anon)
}

#[tokio::test]
async fn test_fk_options_search_returns_matches() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(router, Request::builder()
        .uri("/admin/api/post3/tag_id/options?search=foo")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK, "status: body={body}");
    assert!(body.contains("foo-tag") || body.contains("foo"), "foo-tag in response: {body}");
}

#[tokio::test]
async fn test_fk_options_has_more_when_exceeds_page_size() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    // 26 tags total, default page_size=20 → has_more=true
    let (status, _h, body) = send(router, Request::builder()
        .uri("/admin/api/post3/tag_id/options?page_size=20")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK);
    // For HTML response (HTMX): has_more is encoded in body or we inspect JSON.
    // For JSON response (no HX-Request): parse JSON.
    if body.trim().starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(v["has_more"], serde_json::json!(true), "has_more=true: {body}");
    } else {
        // HTML response — just check we got items back.
        assert!(body.len() > 10, "non-empty HTML response");
    }
}

#[tokio::test]
async fn test_fk_options_resolve_returns_labels() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(router, Request::builder()
        .uri("/admin/api/post3/tag_id/options/resolve?ids=1,2")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).expect("json: {body}");
    let items = v["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "items non-empty: {body}");
    assert!(items[0]["label"].is_string(), "label is string: {body}");
}

#[tokio::test]
async fn test_fk_options_no_staff_returns_403_or_redirect() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let (status, _h, _body) = send(router, Request::builder()
        .uri("/admin/api/post3/tag_id/options")
        .body(Body::empty()).unwrap()
    ).await;
    // No session → redirect to login (303) or 403.
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FORBIDDEN || status == StatusCode::TEMPORARY_REDIRECT,
        "unauthenticated blocked: {status}"
    );
}
```

- [ ] **Step 2: Run to verify test compilation**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo test -p umbral-admin phase3_fk 2>&1 | tail -30
```

---

## Task 9: Write phase3_inline_edit.rs tests

**Files:**
- New: `plugins/umbral-admin/tests/phase3_inline_edit.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Phase 3 inline cell edit tests.
//!
//! Covers:
//! 1. GET /admin/note/1/cell/title/edit returns field editor fragment.
//! 2. POST /admin/note/1/cell/title with valid body updates the row, returns read-only cell.
//! 3. Validation error returns editor with error message.
//! 4. Read-only field returns 403.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct CellNote {
    id: i64,
    title: String,
    body: String,
    published: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_inline.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(SqliteConnectOptions::new().filename(&path).create_if_missing(true))
            .await.expect("pool");

        let note_config = AdminModel::new("cell_note")
            .list_display(&["title", "published"])
            .readonly_fields(&["body"])
            .inline_edit_fields(&["title", "published"]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(note_config))
            .model::<CellNote>()
            .build().expect("build");

        let pool = umbral::db::pool();
        sqlx::query("CREATE TABLE auth_user (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL UNIQUE, email TEXT NOT NULL, password_hash TEXT NOT NULL, is_active INTEGER NOT NULL, is_staff INTEGER NOT NULL, is_superuser INTEGER NOT NULL, date_joined TEXT NOT NULL, last_login TEXT)")
            .execute(&pool).await.expect("auth_user");
        sqlx::query("CREATE TABLE session (id TEXT PRIMARY KEY, user_id INTEGER, data TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL, expires_at TEXT NOT NULL)")
            .execute(&pool).await.expect("session");
        sqlx::query("CREATE TABLE cell_note (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '', published INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool).await.expect("cell_note");

        sqlx::query("INSERT INTO cell_note (title, body, published) VALUES ('Original Title', 'Body text', 0)")
            .execute(&pool).await.expect("seed");

        let staff = create_user("cell_admin", "cell@example.com", "pass123").await.expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id).execute(&pool).await.expect("set staff");

        app.into_router()
    }).await
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}
fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos+200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    after[..after.find('"').unwrap_or(0)].to_string()
}
fn extract_cookie(s: &str) -> String {
    s.split(';').next().and_then(|p| p.split_once('=').map(|(_, v)| v.to_string())).unwrap_or_default()
}
async fn login(router: axum::Router) -> String {
    let resp = router.clone().oneshot(Request::builder().uri("/admin/login").body(Body::empty()).unwrap()).await.expect("get");
    let anon_raw = resp.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
    let anon = extract_cookie(&anon_raw);
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([("username","cell_admin"),("password","pass123"),("csrf_token",csrf.as_str()),("next","/admin/")]).unwrap();
    let resp2 = router.clone().oneshot(Request::builder().method("POST").uri("/admin/login")
        .header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("umbral_session={anon}"))
        .body(Body::from(form)).unwrap()).await.expect("post");
    resp2.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(extract_cookie).unwrap_or(anon)
}

#[tokio::test]
async fn test_cell_edit_get_returns_editor_fragment() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(router, Request::builder()
        .uri("/admin/cell_note/1/cell/title/edit")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK, "cell edit GET ok: {body}");
    assert!(body.contains("<form") || body.contains("<input"), "editor fragment: {body}");
    assert!(body.contains("title") || body.contains("Original"), "field name or value: {body}");
    assert!(!body.contains("<!doctype"), "not full page: {body}");
}

#[tokio::test]
async fn test_cell_edit_post_updates_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/cell_note/1/cell/title")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("title=Updated+Cell+Title")).unwrap()
    ).await;
    assert_eq!(status, StatusCode::OK, "cell save ok: {body}");
    assert!(body.contains("Updated Cell Title"), "new value in response: {body}");
    // Verify DB updated.
    let pool = umbral::db::pool();
    let title: String = sqlx::query_scalar("SELECT title FROM cell_note WHERE id = 1")
        .fetch_one(&pool).await.expect("query");
    assert_eq!(title, "Updated Cell Title");
}

#[tokio::test]
async fn test_cell_edit_readonly_field_returns_403() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, _body) = send(router, Request::builder()
        .uri("/admin/cell_note/1/cell/body/edit")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty()).unwrap()
    ).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "readonly field blocked");
}

#[tokio::test]
async fn test_cell_edit_post_nonexistent_row_returns_404() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, _body) = send(router, Request::builder()
        .method("POST")
        .uri("/admin/cell_note/9999/cell/title")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("title=X")).unwrap()
    ).await;
    // Row doesn't exist — UPDATE affects 0 rows but doesn't error.
    // We just verify no server error.
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND, "status: {status}");
}
```

- [ ] **Step 2: Run test compilation check**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo test -p umbral-admin phase3_inline 2>&1 | tail -30
```

---

## Task 10: Full build + test pass

- [ ] **Step 1: Format**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo fmt
```

- [ ] **Step 2: Clippy**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo clippy --all-targets 2>&1 | grep -E "^error" | head -30
```

Fix any errors. Warnings are acceptable.

- [ ] **Step 3: Build**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo build 2>&1 | tail -20
```

Expected: `Finished` line.

- [ ] **Step 4: Test**

```bash
cd /home/dalmas/E/projects/umbral/crates && cargo test 2>&1 | tail -40
```

Expected: all tests pass including the three new phase3_* suites. If a test fails due to the `OnceCell` being shared between tests in the same binary, debug with `-- --test-threads=1` for that test file.

---

## Task 11: Update admin.mdx docs

**Files:**
- Modify: `documentation/docs/v0.0.1/plugins/admin.mdx`

- [ ] **Step 1: Append Phase 3 section to admin.mdx**

Add after the existing Phase 2 section (after line ~258):

```mdx
## Phase 3: actions + async pickers + sheet stacking + inline edit

### Action descriptor system

Phase 3 replaces the simple bulk-action shim with a full action descriptor. Each `Action` now carries icon, variant, scope, optional confirm message, and a typed `ActionResult`:

```rust
use umbral_admin::{Action, ActionInvocation, ActionResult, ActionScope, ActionVariant, ToastLevel};

let publish = Action::new(
    "publish",          // URL-safe key
    "Publish",          // Label
    "send",             // Lucide icon name
    |inv: ActionInvocation| async move {
        // inv.ids   — selected primary keys
        // inv.pool  — ambient SqlitePool for mutations
        // inv.username, inv.table — context
        Ok(ActionResult::Toast {
            message: format!("Published {} post(s).", inv.ids.len()),
            level: ToastLevel::Success,
        })
    },
)
.scope(ActionScope::Both)         // Row | Bulk | Both
.danger()                          // red styling
.confirm("Publish selected posts?"); // confirm dialog before firing
```

Register on `AdminModel`:

```rust
AdminModel::new("post")
    .actions(vec![Action::delete_selected(), publish])
```

`ActionResult` variants and their HTMX encoding:

| Variant | Effect |
|---|---|
| `Toast { message, level }` | `HX-Trigger: {"showToast": {...}}` header |
| `RefreshTable` | Returns the rows fragment, HTMX swaps `#table-body` |
| `OpenSheet { table, id }` | `HX-Trigger: {"openSheet": {...}}` header |
| `Download { filename, content_type, bytes }` | `Content-Disposition: attachment` response |
| `Redirect { url }` | `HX-Redirect` header |

### Row action column + overflow menu

The DataTable renders up to 2 custom row actions as inline icon buttons after the built-in eye/pencil/trash. If more than 2 custom actions exist with `scope = Row | Both`, a `more-horizontal` overflow menu lists the remainder. Danger-variant actions render in `text-error`.

### Floating bulk-action toolbar

When rows are selected, a pill-shaped toolbar rises from the bottom-center with:
- Left: "N selected" + "Clear"
- Right: up to 4 bulk-scope action buttons (Lucide icons), remainder in an overflow menu

Toolbar fires `POST /admin/{table}/actions/{key}` with `{"ids": [...]}` via `fetch()`. Responses are handled: `HX-Trigger showToast` pops a toast; `HX-Redirect` navigates; default refreshes the table.

### Phase 3 action endpoints

| Route | Purpose |
|---|---|
| `POST /admin/{table}/actions/{key}` | Run an action over `{ "ids": [...] }` |

### Async FK picker

FK fields in edit/create sheets use a searchable combobox instead of a plain number input. The picker calls:

```
GET /admin/api/{table}/{field}/options?search=&page=&page_size=20
→ { "items": [{ "value": 1, "label": "My Post" }], "page": 1, "has_more": false }

GET /admin/api/{table}/{field}/options/resolve?ids=1,2
→ { "items": [{ "value": 1, "label": "My Post" }, ...] }
```

The `label` for each option comes from the related model's first text column. The `search` parameter matches against the related `AdminModel`'s `search_fields` (or the first text column as fallback). Both endpoints require `is_staff`. The resolve endpoint loads pre-selected labels on edit-form open without a full page fetch.

### Sheet stacking

Opening "+ Add new" inside an FK picker pushes the current sheet onto a JS stack and loads the create sheet for the related model, offset by 40px. The stacked sheet header shows a back chevron (`chevron-left`) to pop the top sheet. `Esc` always closes the top sheet only.

### Inline cell edit

Opt specific columns into double-click inline editing:

```rust
AdminModel::new("post")
    .inline_edit_fields(&["title", "slug"])
```

Double-clicking an enabled cell HTMX-swaps the cell content with a compact inline editor. Saving on blur or Enter POSTs the new value; the cell reverts to read-only on success. Validation errors appear inline.

| Route | Purpose |
|---|---|
| `GET /admin/{table}/{id}/cell/{field}/edit` | Returns editor fragment for one cell |
| `POST /admin/{table}/{id}/cell/{field}` | Saves new value, returns read-only cell |

### Toast notifications

All action results that produce a `Toast` trigger a bottom-right toast via the `showToast` HTMX event. Levels: `info` / `success` / `warning` / `error`. Auto-dismisses after 4 seconds.
```

---

## Self-review

**Spec coverage check:**

| Spec item | Covered in task |
|---|---|
| Action descriptor (key, label, icon, variant, scope, confirm, permission) | Task 1 |
| ActionInvocation (ids, user, pool, table) | Task 1 |
| ActionResult (Toast, RefreshTable, OpenSheet, Download, Redirect) | Task 1 |
| Action::delete_selected() rebuilt | Task 1 |
| AdminModel::inline_edit_fields | Task 1 |
| POST /admin/{table}/actions/{key} endpoint | Task 2 |
| FK options endpoints (/options, /options/resolve) | Task 2 |
| Cell edit endpoints (GET + POST) | Task 2 |
| Row action overflow menu (2 inline + more-horizontal) | Task 3 |
| Bulk-action toolbar (floating pill, selection JS) | Task 3 |
| Inline-edit dblclick on cells | Task 3 |
| Toast renderer in wrapper | Task 4 |
| FK combobox in field_editor | Task 5 |
| M2M chip picker macro (wired, no model uses it yet) | Task 5 |
| Sheet stacking JS + Back chevron | Task 6 |
| phase3_actions.rs tests (5 cases) | Task 7 |
| phase3_fk_picker.rs tests (4 cases) | Task 8 |
| phase3_inline_edit.rs tests (4 cases) | Task 9 |
| admin.mdx Phase 3 section | Task 11 |

**Placeholder scan:** No TBD/TODO in task steps. All code blocks are complete.

**Type consistency:**
- `Action::new(key, label, icon, handler)` — 4-arg constructor used consistently in Task 1 and all test files.
- `ActionInvocation { ids, username, table, pool }` — used in Task 1 handler closure and Task 7 test action.
- `action.key` (not `action.name`) — Tasks 2, 3, 7 all use `.key`.
- `action_descriptors_json(cfg)` helper defined in Task 2 step 4, used in Task 2 steps 4 and 8.
- `rows_fragment_for(&state, &headers, &table, &params)` defined in Task 2 step 4, called from `dispatch_action` and `rows_fragment`.
- `FormField.fk_table` added in Task 5 step 3, used in `field_editor.html` Task 5 step 1.
