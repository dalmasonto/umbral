//! Classic CRUD handlers for the full-page admin: `detail`, `new_form`,
//! `create`, `edit_form`, `update`, `delete`, and the HTMX `delete`
//! variant (`htmx_delete`).
//!
//! The sheet variants for the same operations live in
//! `handlers::sheet`. `update` calls `sheet::edit_sheet_handler` when
//! the form posts `_save_continue` so the saved state re-renders into
//! the open sheet without a redirect.

use std::collections::HashMap;

use axum::extract::{Path, State};
use minijinja::context;
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

use umbra::orm::DynQuerySet;

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column, user_theme};
use crate::engine::render;
use crate::error::AdminError;
use crate::handlers::sheet::edit_sheet_handler;
use crate::rows::{fetch_rows_filtered, insert_row_in_tx, update_row_in_tx};
use crate::util::{apply_write_error_to_fields, is_htmx, parse_unique_violation_column,
    sanitise_form_error};
use crate::view::{
    form_fields_for, form_m2m_fields_for, model_for_template, sidebar_apps, validate_form,
};

/// Decode a request body into ordered `(field, value)` pairs, handling
/// both urlencoded and `multipart/form-data` admin POSTs (Wave 4).
///
/// For multipart bodies the file parts are stored through the ambient
/// Storage backend and their values become the returned storage keys —
/// the exact same flat shape `serde_urlencoded::from_str` yields, so the
/// caller's `HashMap` (last-wins) and `Vec` (repeated M2M values) builds
/// are unchanged. Empty file parts are skipped by
/// `parse_and_store_multipart`, so an edit that doesn't re-pick a file
/// simply omits that column (update preserves the existing key).
///
/// Returns an `AdminError` on a malformed body or a storage failure,
/// mirroring the urlencoded `BadInput` path the handlers used before.
pub(crate) async fn body_to_pairs(
    content_type: &str,
    body: axum::body::Bytes,
) -> Result<Vec<(String, String)>, AdminError> {
    if umbra::web::is_multipart(content_type) {
        umbra::web::parse_and_store_multipart(content_type, body)
            .await
            .map_err(|e| AdminError::BadInput(e.to_string()))
    } else {
        let s = String::from_utf8(body.to_vec())
            .map_err(|_| AdminError::BadInput("request body was not valid UTF-8".to_string()))?;
        serde_urlencoded::from_str(&s).map_err(|e| AdminError::BadInput(e.to_string()))
    }
}

/// Cross-module wrapper used by the sheet handler — keeps
/// `apply_m2m_selections` private to this file while still letting
/// `handlers::sheet::sheet_create` reuse the same write path.
pub(crate) async fn apply_m2m_selections_pub(
    parent: &umbra::migrate::ModelMeta,
    parent_pk_str: &str,
    multi_form: &[(String, String)],
) -> Result<(), sqlx::Error> {
    apply_m2m_selections(parent, parent_pk_str, multi_form).await
}

/// Persist M2M field selections for a parent row, one
/// `set_junction_dynamic` call per `Model::M2M_RELATIONS` entry.
///
/// Walks `parent.m2m_relations` (the ModelMeta mirror of
/// `Model::M2M_RELATIONS`), extracts every checked candidate from the
/// form's repeated `m2m_<field>` values, parses the parent + child
/// PKs through `form_str_to_sea_value` so the bind types match the
/// junction's column types, and replaces the junction's existing
/// rows for this parent inside a single transaction.
///
/// Called by `create` (after INSERT) and `update` (after UPDATE).
/// No-op when the parent has no M2M relations or the form has no
/// `m2m_*` entries.
async fn apply_m2m_selections(
    parent: &umbra::migrate::ModelMeta,
    parent_pk_str: &str,
    multi_form: &[(String, String)],
) -> Result<(), sqlx::Error> {
    if parent.m2m_relations.is_empty() {
        return Ok(());
    }
    let Some(parent_pk_col) = parent.fields.iter().find(|c| c.primary_key) else {
        return Ok(());
    };
    // Parse the parent PK once — same value binds against every
    // junction's `parent_id` column for this row.
    let parent_value = match umbra::orm::write::json_to_sea_value(
        parent_pk_col.ty,
        &serde_json::Value::String(parent_pk_str.to_string()),
        false,
        &parent_pk_col.name,
        None,
    ) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    for rel in &parent.m2m_relations {
        let field_key = format!("m2m_{}", rel.field_name);
        let raw_child_values: Vec<&str> = multi_form
            .iter()
            .filter_map(|(k, v)| {
                if k == &field_key {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .filter(|v| !v.is_empty())
            .collect();
        // Look up the child's PK column to dispatch the bind type.
        let Some(target) = umbra::migrate::registered_models()
            .into_iter()
            .find(|m| m.table == rel.target_table)
        else {
            continue;
        };
        let Some(child_pk_col) = target.fields.iter().find(|c| c.primary_key) else {
            continue;
        };
        let mut child_values = Vec::with_capacity(raw_child_values.len());
        for raw in raw_child_values {
            match umbra::orm::write::json_to_sea_value(
                child_pk_col.ty,
                &serde_json::Value::String(raw.to_string()),
                false,
                &child_pk_col.name,
                None,
            ) {
                Ok(v) => child_values.push(v),
                Err(_) => continue,
            }
        }
        let junction_table = format!("{}_{}", parent.table, rel.field_name);
        umbra::orm::set_junction_dynamic(
            &junction_table,
            parent_value.clone(),
            child_values,
            Some(parent.name.as_str()),
        )
        .await?;
    }
    Ok(())
}

/// `GET /admin/{table}/{id}` — read-only detail page.
pub(crate) async fn detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("{}/{table}/{id}", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
    let perms = crate::permcheck::AdminPerms::load(&user, &plugin_name, &table).await;
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&model, Some((&pk.name, &id)), &all_cols).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    let apps = sidebar_apps(&state, &user).await;
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("{}/{table}/{id}", crate::branding::current().base_path) }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/detail.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            row           => row,
            pk            => pk.name.clone(),
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
            perms         => perms,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/new` — empty create form.
pub(crate) async fn new_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Response {
    let path = format!("{}/{table}/new", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::Add).await
    {
        return r;
    }
    let perms = crate::permcheck::AdminPerms::load(&user, &plugin_name, &table).await;
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    // BUG-16 admin: empty M2M selection on the create form (no
    // parent PK yet — the junction rows get written post-INSERT by
    // the POST handler once the parent has an id).
    let m2m_fields = form_m2m_fields_for(&model, None).await;
    // Inlines: blank `extra` rows only (no parent PK yet).
    let inlines = match crate::inlines::build_inline_views(&model, None, cfg).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let apps = sidebar_apps(&state, &user).await;
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
        serde_json::json!({ "label": "Add", "url": format!("{}/{table}/new", crate::branding::current().base_path) }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/form.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            fields        => fields,
            m2m_fields    => m2m_fields,
            inlines       => inlines,
            verb          => "Create",
            action        => format!("{}/{}/new", crate::branding::current().base_path, model.table),
            error         => "",
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
            perms         => perms,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /admin/{table}/new` — full-page create. Audit-logs the insert
/// then redirects to the changelist; on failure re-renders the form
/// with the inline error.
pub(crate) async fn create(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let path = format!("{}/{table}/new", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::Add).await
    {
        return r;
    }
    // Decode the body once into ordered (field, value) pairs, storing any
    // uploaded files via the ambient Storage (Wave 4). The pairs then
    // feed the two views the write paths need:
    //   - HashMap collapses duplicates to "last wins" (scalar fields,
    //     including the file column's stored key).
    //   - Vec<(K, V)> preserves duplicates (`m2m_<field>` repeated
    //     entries — the checkbox list emits one pair per checked
    //     candidate).
    let content_type = headers
        .get(umbra::web::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let pairs = match body_to_pairs(content_type, body).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let form: HashMap<String, String> = pairs.iter().cloned().collect();
    let multi_form: Vec<(String, String)> = pairs;
    let cfg = state.config_for(&table);
    // gaps2 #43: validate every field up front and surface ALL failures
    // at once (each below its own input) rather than letting the user
    // discover them one DB error at a time.
    let field_errors = validate_form(&model, &form, cfg);
    if !field_errors.is_empty() {
        let mut fields = form_fields_for(&model, Some(&form), cfg);
        for f in &mut fields {
            if let Some(m) = field_errors.get(&f.name) {
                f.error = m.clone();
            }
        }
        let m2m_fields = form_m2m_fields_for(&model, None).await;
        let inlines = crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
        let apps = sidebar_apps(&state, &user).await;
        let breadcrumbs = vec![
            serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
            serde_json::json!({ "label": "Add", "url": format!("{}/{table}/new", crate::branding::current().base_path) }),
        ];
        let initial_theme = user_theme(&user).await;
        return match render(
            "admin/form.html",
            context!(
                user          => user.username.clone(),
                model         => model_for_template(&model),
                fields        => fields,
                m2m_fields    => m2m_fields,
                inlines       => inlines,
                verb          => "Create",
                action        => format!("{}/{}/new", crate::branding::current().base_path, model.table),
                error         => "",
                apps          => apps,
                active_table  => table,
                breadcrumbs   => breadcrumbs,
                initial_theme => initial_theme,
            ),
        ) {
            Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
            Err(e2) => e2.into_response(),
        };
    }
    // Atomic save: parent INSERT + inline children, one transaction.
    // Any child failure drops the tx so neither the parent nor any
    // child persists (the load-bearing rollback guarantee).
    let saved = save_parent_and_inlines(&model, None, &form, cfg, &multi_form).await;
    match saved {
        Ok(new_pk) => {
            // BUG-16 admin: with the parent committed, apply any M2M
            // selections from the form. M2M runs in its own
            // transaction (follow-up: fold it into the parent tx).
            if let Err(e) = apply_m2m_selections(&model, &new_pk, &multi_form).await {
                return AdminError::Sqlx(e).into_response();
            }
            crate::models::log(
                user.id,
                "create",
                &table,
                None,
                &format!("created {} (via form)", model.name),
            )
            .await;
            Redirect::to(&format!(
                "{}/{}/",
                crate::branding::current().base_path,
                model.table
            ))
            .into_response()
        }
        Err(e) => {
            // gaps2 #12 part 2: for WriteError, merge per-field messages
            // into each field's `.error` slot; non-field errors go to the
            // top banner. For Sqlx UNIQUE violations we also attribute to
            // the field when we can parse the column name.
            let mut fields = form_fields_for(&model, Some(&form), cfg);
            let banner_error = match &e {
                AdminError::Write(we) => {
                    sanitise_form_error(&e); // fires the tracing::error! log
                    apply_write_error_to_fields(we, &mut fields)
                }
                AdminError::Sqlx(sqlx_err) => {
                    let msg = sqlx_err.to_string();
                    if let Some(col) = parse_unique_violation_column(&msg) {
                        if let Some(f) = fields.iter_mut().find(|f| f.name == col) {
                            f.error = format!("A record with this `{col}` already exists.");
                            String::new()
                        } else {
                            sanitise_form_error(&e)
                        }
                    } else {
                        sanitise_form_error(&e)
                    }
                }
                // BadInput carries the inline row diagnostic verbatim.
                AdminError::BadInput(msg) => msg.clone(),
                _ => sanitise_form_error(&e),
            };
            let m2m_fields = form_m2m_fields_for(&model, None).await;
            let inlines =
                crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
            let apps = sidebar_apps(&state, &user).await;
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
                serde_json::json!({ "label": "Add", "url": format!("{}/{table}/new", crate::branding::current().base_path) }),
            ];
            let initial_theme = user_theme(&user).await;
            match render(
                "admin/form.html",
                context!(
                    user          => user.username.clone(),
                    model         => model_for_template(&model),
                    fields        => fields,
                    m2m_fields    => m2m_fields,
                    inlines       => inlines,
                    verb          => "Create",
                    action        => format!("{}/{}/new", crate::branding::current().base_path, model.table),
                    error         => banner_error,
                    apps          => apps,
                    active_table  => table,
                    breadcrumbs   => breadcrumbs,
                    initial_theme => initial_theme,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

/// Cross-module wrapper so `handlers::sheet::sheet_create` reuses the
/// exact same atomic parent-INSERT + inline-children save path the
/// full-page `create` uses, keeping `save_parent_and_inlines` private.
pub(crate) async fn create_parent_and_inlines_pub(
    model: &umbra::migrate::ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&crate::config::AdminConfig>,
    multi_form: &[(String, String)],
) -> Result<String, AdminError> {
    save_parent_and_inlines(model, None, form, cfg, multi_form).await
}

/// Save a parent row plus all its inline children atomically.
///
/// `existing_pk` is `Some((pk_col, pk_value))` for an UPDATE, `None` for
/// an INSERT. Opens one transaction, writes the parent, sets the FK on
/// each inline child to the (possibly just-allocated) parent PK, then
/// commits. On any error the transaction is dropped — rolling back the
/// parent write together with every child write. Returns the parent PK
/// as a string on success.
async fn save_parent_and_inlines(
    model: &umbra::migrate::ModelMeta,
    existing_pk: Option<(&umbra::migrate::Column, &str)>,
    form: &HashMap<String, String>,
    cfg: Option<&crate::config::AdminConfig>,
    multi_form: &[(String, String)],
) -> Result<String, AdminError> {
    let mut tx = umbra::db::begin().await.map_err(AdminError::Sqlx)?;

    let parent_pk = match existing_pk {
        Some((pk, pk_value)) => {
            update_row_in_tx(&mut tx, model, pk, pk_value, form, cfg).await?;
            pk_value.to_string()
        }
        None => insert_row_in_tx(&mut tx, model, form, cfg).await?,
    };

    crate::inlines::save_inlines_in_tx(&mut tx, model, &parent_pk, cfg, multi_form).await?;

    tx.commit().await.map_err(AdminError::Sqlx)?;
    Ok(parent_pk)
}

/// `GET /admin/{table}/{id}/edit` — prefilled edit form.
pub(crate) async fn edit_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("{}/{table}/{id}/edit", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &user,
        &plugin_name,
        &table,
        crate::permcheck::Action::Change,
    )
    .await
    {
        return r;
    }
    let perms = crate::permcheck::AdminPerms::load(&user, &plugin_name, &table).await;
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&model, Some((&pk.name, &id)), &all_cols).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    let row_strings: HashMap<String, String> =
        row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, Some(&row_strings), cfg);
    // BUG-16 admin: pre-check the current M2M selection for this
    // parent row. The PK comes from the URL path; the helper does
    // one `SELECT DISTINCT child_id FROM <junction> WHERE parent_id
    // = ?` per M2M field.
    let m2m_fields = form_m2m_fields_for(&model, Some(&id)).await;
    // Inlines: existing children (prefilled) + `extra` blank rows.
    let inlines = match crate::inlines::build_inline_views(&model, Some(&id), cfg).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let apps = sidebar_apps(&state, &user).await;
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("{}/{table}/{id}", crate::branding::current().base_path) }),
        serde_json::json!({ "label": "Edit", "url": format!("{}/{table}/{id}/edit", crate::branding::current().base_path) }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/form.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            fields        => fields,
            m2m_fields    => m2m_fields,
            inlines       => inlines,
            verb          => "Edit",
            action        => format!("{}/{}/{}/edit", crate::branding::current().base_path, model.table, id),
            row           => row,
            pk            => pk.name.clone(),
            error         => "",
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
            perms         => perms,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /admin/{table}/{id}/edit` — full-page or HTMX update.
///
/// Two HTMX cases:
///
///   1. `_save_continue` is present in the form: re-render the edit
///      sheet so the user can keep editing.
///   2. Plain Save: emit an `HX-Trigger` that closes the sheet and
///      refreshes the table, all without a full page nav.
pub(crate) async fn update(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let path = format!("{}/{table}/{id}/edit", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &user,
        &plugin_name,
        &table,
        crate::permcheck::Action::Change,
    )
    .await
    {
        return r;
    }
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    // Wave 4: same body decode as `create`. An empty file part is
    // skipped by `parse_and_store_multipart`, so the file column is
    // simply absent from `form` when the user didn't pick a new file —
    // `update_form` only writes columns present in the map, so the
    // existing stored key is preserved (never nulled).
    let content_type = headers
        .get(umbra::web::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let pairs = match body_to_pairs(content_type, body).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let form: HashMap<String, String> = pairs.iter().cloned().collect();
    let multi_form: Vec<(String, String)> = pairs;
    let cfg = state.config_for(&table);
    // gaps2 #43: same up-front, all-at-once field validation as `create`.
    let field_errors = validate_form(&model, &form, cfg);
    if !field_errors.is_empty() {
        let mut fields = form_fields_for(&model, Some(&form), cfg);
        for f in &mut fields {
            if let Some(m) = field_errors.get(&f.name) {
                f.error = m.clone();
            }
        }
        let m2m_fields = form_m2m_fields_for(&model, Some(&id)).await;
        let inlines = crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
        // Sheet-aware: an invalid field from the slide-over sheet
        // re-renders the sheet fragment (same shared handler).
        if is_htmx(&headers) {
            let password_field = cfg.and_then(|c| c.password_field.as_deref()).unwrap_or("");
            return match render(
                "admin/sheet_edit.html",
                context!(
                    model          => model_for_template(&model),
                    instance_id    => id,
                    fields         => fields,
                    m2m_fields     => m2m_fields,
                    inlines        => inlines,
                    error          => "",
                    password_field => password_field,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            };
        }
        let apps = sidebar_apps(&state, &user).await;
        let breadcrumbs = vec![
            serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
            serde_json::json!({ "label": format!("#{id}"), "url": format!("{}/{table}/{id}", crate::branding::current().base_path) }),
            serde_json::json!({ "label": "Edit", "url": format!("{}/{table}/{id}/edit", crate::branding::current().base_path) }),
        ];
        let initial_theme = user_theme(&user).await;
        return match render(
            "admin/form.html",
            context!(
                user          => user.username.clone(),
                model         => model_for_template(&model),
                fields        => fields,
                m2m_fields    => m2m_fields,
                inlines       => inlines,
                verb          => "Edit",
                action        => format!("{}/{}/{}/edit", crate::branding::current().base_path, model.table, id),
                error         => "",
                apps          => apps,
                active_table  => table,
                breadcrumbs   => breadcrumbs,
                initial_theme => initial_theme,
            ),
        ) {
            Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
            Err(e2) => e2.into_response(),
        };
    }
    // Atomic save: parent UPDATE + inline children, one transaction.
    let saved = save_parent_and_inlines(&model, Some((pk, &id)), &form, cfg, &multi_form).await;
    match saved {
        Ok(_) => {
            // BUG-16 admin: replace this parent's M2M selections in
            // each auto-generated junction table to match the form.
            if let Err(e) = apply_m2m_selections(&model, &id, &multi_form).await {
                return AdminError::Sqlx(e).into_response();
            }
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                user.id,
                "update",
                &table,
                object_id,
                &format!("updated {} #{}", model.name, id),
            )
            .await;

            if is_htmx(&headers) {
                if form.contains_key("_save_continue") {
                    return edit_sheet_handler(State(state), headers, Path((table, id))).await;
                }
                // gaps2 #13: success toast alongside closeSheet +
                // refreshTable. Symmetric with `sheet::sheet_create`.
                let trigger = serde_json::json!({
                    "closeSheet": {},
                    "refreshTable": {},
                    "showToast": {
                        "message": format!("{} updated", model.name),
                        "level": "success"
                    },
                });
                let mut resp = axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("HX-Trigger", trigger.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap();
                resp.headers_mut()
                    .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
                return resp;
            }

            Redirect::to(&format!(
                "{}/{}/{}",
                crate::branding::current().base_path,
                model.table,
                id
            ))
            .into_response()
        }
        Err(e) => {
            // gaps2 #12 part 2: same per-field attribution as `create`.
            let mut fields = form_fields_for(&model, Some(&form), cfg);
            let banner_error = match &e {
                AdminError::Write(we) => {
                    sanitise_form_error(&e); // fires the tracing::error! log
                    apply_write_error_to_fields(we, &mut fields)
                }
                AdminError::Sqlx(sqlx_err) => {
                    let msg = sqlx_err.to_string();
                    if let Some(col) = parse_unique_violation_column(&msg) {
                        if let Some(f) = fields.iter_mut().find(|f| f.name == col) {
                            f.error = format!("A record with this `{col}` already exists.");
                            String::new()
                        } else {
                            sanitise_form_error(&e)
                        }
                    } else {
                        sanitise_form_error(&e)
                    }
                }
                // BadInput carries the inline row diagnostic verbatim.
                AdminError::BadInput(msg) => msg.clone(),
                _ => sanitise_form_error(&e),
            };
            let m2m_fields = form_m2m_fields_for(&model, Some(&id)).await;
            let inlines =
                crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
            // Sheet-aware error re-render: a bad submit from the slide-over
            // sheet (HTMX, posting to this same shared handler) re-renders
            // the SHEET fragment — keeping the user's values + the error
            // in the open panel — rather than a full page. Mirrors the
            // `_save_continue` → `edit_sheet_handler` success precedent.
            if is_htmx(&headers) {
                let password_field =
                    cfg.and_then(|c| c.password_field.as_deref()).unwrap_or("");
                return match render(
                    "admin/sheet_edit.html",
                    context!(
                        model          => model_for_template(&model),
                        instance_id    => id,
                        fields         => fields,
                        m2m_fields     => m2m_fields,
                        inlines        => inlines,
                        error          => banner_error,
                        password_field => password_field,
                    ),
                ) {
                    Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                    Err(e2) => e2.into_response(),
                };
            }
            let apps = sidebar_apps(&state, &user).await;
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
                serde_json::json!({ "label": format!("#{id}"), "url": format!("{}/{table}/{id}", crate::branding::current().base_path) }),
                serde_json::json!({ "label": "Edit", "url": format!("{}/{table}/{id}/edit", crate::branding::current().base_path) }),
            ];
            let initial_theme = user_theme(&user).await;
            match render(
                "admin/form.html",
                context!(
                    user          => user.username.clone(),
                    model         => model_for_template(&model),
                    fields        => fields,
                    m2m_fields    => m2m_fields,
                    inlines       => inlines,
                    verb          => "Edit",
                    action        => format!("{}/{}/{}/edit", crate::branding::current().base_path, model.table, id),
                    error         => banner_error,
                    apps          => apps,
                    active_table  => table,
                    breadcrumbs   => breadcrumbs,
                    initial_theme => initial_theme,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

/// `POST /admin/{table}/{id}/delete` — legacy form-POST delete.
pub(crate) async fn delete(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!(
        "{}/{table}/{id}/delete",
        crate::branding::current().base_path
    );
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Delete)
            .await
    {
        return r;
    }
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .delete()
        .await
    {
        Ok(_) => {
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                who.id,
                "delete",
                &table,
                object_id,
                &format!("deleted {} #{}", model.name, id),
            )
            .await;
            Redirect::to(&format!(
                "{}/{}/",
                crate::branding::current().base_path,
                model.table
            ))
            .into_response()
        }
        // gaps2 #12: `e` is `DynError` now; route through the
        // `From<DynError>` impl so Write(WriteError) keeps its
        // structure instead of flattening to "database error".
        Err(e) => AdminError::from(e).into_response(),
    }
}

/// `DELETE /admin/{table}/{id}` — HTMX delete. Returns an empty body
/// with `HX-Trigger: closeSheet + refreshTable` so the changelist
/// swaps in place (matches the in-place refresh `update` does after
/// a save). Falls back to a full reload on the listener side when
/// the caller isn't on a changelist (detail page delete).
pub(crate) async fn htmx_delete(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("{}/{table}/{id}", crate::branding::current().base_path);
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Delete)
            .await
    {
        return r;
    }
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .delete()
        .await
    {
        Ok(_) => {
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                who.id,
                "delete",
                &table,
                object_id,
                &format!("deleted {} #{}", model.name, id),
            )
            .await;
            // gaps2 #13: success toast on delete too.
            let trigger = serde_json::json!({
                "closeSheet": {},
                "refreshTable": {},
                "showToast": {
                    "message": format!("{} #{} deleted", model.name, id),
                    "level": "success"
                },
            });
            let mut resp = axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| {
                    Redirect::to(&format!(
                        "{}/{}/",
                        crate::branding::current().base_path,
                        model.table
                    ))
                    .into_response()
                });
            resp.headers_mut()
                .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
            resp
        }
        // gaps2 #12: `e` is `DynError`; route via `From<DynError>`.
        Err(e) => AdminError::from(e).into_response(),
    }
}
