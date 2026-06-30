//! Template-facing data shapes — the structs handlers populate before
//! invoking `render`.
//!
//! Three concerns live here, all on the "structure the data so
//! minijinja can iterate it" side:
//!
//!   - `SidebarApp` / `SidebarModel` — the nav tree.
//!   - `FormField` + `form_fields_for` — one create/edit row per
//!     non-hidden column, with the input kind already chosen.
//!   - `ModelView` / `ColumnView` — the detail / read view's typed
//!     column list.
//!
//! Each function is pure: it reads `ModelMeta` (from the migration
//! registry) or `AdminConfig` (the developer-supplied per-model
//! tweaks) and returns plain data. No DB calls, no async, no I/O.

use std::collections::HashMap;

use serde::Serialize;
use umbral::migrate::ModelMeta;
use umbral::orm::SqlType;

use crate::AdminState;
use crate::config::AdminConfig;

// =========================================================================
// Sidebar context
// =========================================================================

/// Template-facing representation of one sidebar model link.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SidebarModel {
    pub table: String,
    pub label: String,
    pub icon: String,
}

/// Template-facing group of models for one plugin.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SidebarApp {
    pub plugin: String,
    pub label: String,
    pub models: Vec<SidebarModel>,
}

/// Build the per-plugin nav tree. Reads the registry's `apps()` cut
/// (which already honours `Plugin::admin_register` + the auto-discovered
/// fallback) and reshapes each entry for direct template consumption.
///
/// Permission gating (gaps2 #83): when `PermissionsPlugin` is installed,
/// loads the viewer's full codename set once and passes it to `apps()` so
/// each model entry is filtered against `"<plugin>.view_<table>"`. Superusers
/// and cases where the plugin is absent skip the load (codenames = `None`),
/// matching the no-op fallback that the rest of the admin uses.
pub(crate) async fn sidebar_apps(
    state: &AdminState,
    user: &umbral_auth::AuthUser,
) -> Vec<SidebarApp> {
    // Resolve the viewer's codenames once (one DB round-trip) so the
    // per-model filter in `apps()` is in-memory. Superusers see everything;
    // when PermissionsPlugin isn't installed the check is a no-op (None).
    let viewer_codenames: Option<std::collections::HashSet<String>> =
        if !crate::permcheck::permissions_installed() || user.is_superuser {
            None
        } else {
            let user_id = user.id.to_string();
            match umbral_permissions::user_perms(&user_id).await {
                Ok(set) => Some(set),
                Err(err) => {
                    tracing::warn!(
                        user_id = user_id.as_str(),
                        error = %err,
                        "sidebar_apps: failed to load viewer codenames; showing no models"
                    );
                    // Deny-by-default: an empty set means no model passes the
                    // view-codename check. Consistent with permcheck::check's
                    // "unwrap_or_else(|_| false)" policy.
                    Some(std::collections::HashSet::new())
                }
            }
        };

    state
        .registry
        .apps(user, viewer_codenames.as_ref())
        .into_iter()
        .map(|app| SidebarApp {
            plugin: app.plugin.clone(),
            label: app.label.clone(),
            models: app
                .models
                .into_iter()
                .map(|r| SidebarModel {
                    table: r.model.table.clone(),
                    label: r.label.clone(),
                    icon: r.icon.clone().unwrap_or_else(|| "database".to_string()),
                })
                .collect(),
        })
        .collect()
}

/// Build the sidebar's custom-view groups: non-hidden views the user may
/// see (codename-filtered), clustered by `.group()` (default "Pages"),
/// preserving registration order within a group and first-seen group order.
pub(crate) async fn view_groups(
    state: &AdminState,
    user: &umbral_auth::AuthUser,
) -> Vec<serde_json::Value> {
    let base = crate::branding::current().base_path;
    // Preserve first-seen group order with an explicit order vec +
    // HashMap so insertion order is stable without BTreeMap's sort.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for v in state.custom_views.iter() {
        if v.hidden() {
            continue;
        }
        if let Some(code) = v.permission() {
            if !crate::permcheck::has_codename(user, code).await {
                continue;
            }
        }
        let group = v.group().unwrap_or("Pages").to_string();
        if !groups.contains_key(&group) {
            order.push(group.clone());
        }
        groups.entry(group).or_default().push(serde_json::json!({
            "href":  format!("{}/{}", base, v.path()),
            "label": v.title(),
            "icon":  v.icon().unwrap_or("file-text"),
            "slug":  v.slug(),
        }));
    }
    order
        .into_iter()
        .map(|name| {
            let views = groups.remove(&name).unwrap_or_default();
            serde_json::json!({ "label": name, "views": views })
        })
        .collect()
}

// =========================================================================
// Form fields
// =========================================================================

/// Template-facing description of one form input row.
///
/// The handler builds a `Vec<FormField>` once per request; the
/// `field_editor` Jinja macro dispatches on `kind` to pick `<input>`,
/// `<textarea>`, an FK combobox, or the password pair.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FormField {
    pub name: String,
    pub kind: &'static str,
    pub value: String,
    pub nullable: bool,
    pub readonly: bool,
    /// For FK fields: the related table name. Empty string for non-FK fields.
    pub fk_table: String,
    /// When `true`, this is the synthetic "password" field emitted for
    /// models that have `password_field` set. The field editor renders
    /// two inputs (password + confirm) instead of a plain text input.
    pub is_password: bool,
    /// For choices fields: matching `(value, label)` pairs the
    /// `<select>` widget renders as `<option>`s. Empty for every
    /// non-choices field.
    pub choices: Vec<ChoiceOption>,
    /// `#[umbral(help = "...")]` text. Rendered as a hint line under
    /// the input. Empty string = no hint (the template skips the
    /// markup). Honors the `FieldSpec::help` doc-comment's long-
    /// standing claim that help reaches the admin form.
    pub help: String,
    /// `#[umbral(widget = "...")]` presentation hint (features.md #4),
    /// already folded into `kind` by `input_kind`. Kept on the struct
    /// too so the template / future client JS can branch on the raw
    /// widget name (e.g. load a markdown editor module). Empty = none.
    pub widget: String,
    /// For `file` / `image` fields: the resolved public URL for the
    /// currently-stored key, used by the template to render the
    /// "Current:" link / `<img>` thumbnail. `value` stays the raw
    /// storage key (the round-trip value the hidden form submits);
    /// `value_url` is purely presentational. Resolved through the
    /// ambient Storage backend, falling back to the raw key when no
    /// backend is wired. Empty string for non-file fields and for an
    /// empty value.
    pub value_url: String,
    /// Per-field validation message (gaps2 #43). Populated by the
    /// create / update handlers when `validate_form` rejects the
    /// submitted value for this field; rendered as a red hint line
    /// under the input by `form.html`. Empty string = no error (the
    /// template skips the markup), which is the default on every GET
    /// render and on a field that validated cleanly.
    pub error: String,
}

/// One `<option>` entry on a choices-field `<select>`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChoiceOption {
    pub value: String,
    pub label: String,
}

/// Build the form-field list for one model.
///
/// Three filters drop a column from the form:
///   1. it's the primary key (never editable directly);
///   2. the derive attribute `#[umbral(noform)]` is set;
///   3. the column is the model's `password_field` (handled separately).
///
/// On create forms (`prefill.is_none()`) we append one synthetic
/// password field so the create flow can capture an initial password.
/// Edit forms route password changes through the dedicated
/// `/change-password` endpoint instead.
pub(crate) fn form_fields_for(
    model: &ModelMeta,
    prefill: Option<&HashMap<String, String>>,
    cfg: Option<&AdminConfig>,
) -> Vec<FormField> {
    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let readonly_set: std::collections::HashSet<String> = if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        // No explicit config: still apply sensitive-column defaults.
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    };
    let mut result: Vec<FormField> = model
        .fields
        .iter()
        .filter(|c| {
            if c.primary_key {
                return false;
            }
            if c.noform {
                return false;
            }
            // auto_now / auto_now_add: the framework fills these on
            // INSERT (auto_now_add) and on every INSERT/UPDATE
            // (auto_now). Showing them on the form is wrong — an
            // empty value would 500 the write path with a NOT-NULL
            // violation, and a user-supplied value would defeat the
            // whole purpose of the annotation. Both shapes get
            // hidden from create AND edit forms; the dynamic write
            // path (DynQuerySet::insert_json / update_json) supplies
            // the timestamp without needing the body to carry it.
            if c.auto_now || c.auto_now_add {
                return false;
            }
            if let Some(c2) = cfg.and_then(|cfg| cfg.password_field.as_deref()) {
                if c.name == c2 {
                    return false;
                }
            }
            true
        })
        .map(|c| {
            let raw = prefill
                .and_then(|m| m.get(&c.name))
                .cloned()
                .unwrap_or_default();
            let fk_table = if matches!(c.ty, SqlType::ForeignKey) {
                c.fk_target
                    .clone()
                    .unwrap_or_else(|| c.name.trim_end_matches("_id").to_string())
            } else {
                String::new()
            };
            let is_readonly = readonly_set.contains(&c.name) || c.noedit;
            let choices: Vec<ChoiceOption> = c
                .choices
                .iter()
                .enumerate()
                .map(|(i, value)| ChoiceOption {
                    value: value.clone(),
                    label: c
                        .choice_labels
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| value.clone()),
                })
                .collect();
            let kind = input_kind(c);
            let value = format_for_input(&raw, c.ty);
            // For file/image fields resolve the stored key to a public
            // URL through the ambient Storage backend; fall back to the
            // raw key when no backend is wired (so the link still
            // renders something). Non-file fields and empty values get
            // an empty string (the template skips the markup).
            let value_url = if (kind == "file" || kind == "image") && !value.is_empty() {
                umbral::storage::storage_opt()
                    .map(|s| s.url(&value))
                    .unwrap_or_else(|| value.clone())
            } else {
                String::new()
            };
            FormField {
                name: c.name.clone(),
                kind,
                value,
                nullable: c.nullable,
                readonly: is_readonly,
                fk_table,
                is_password: false,
                choices,
                help: c.help.clone(),
                widget: c.widget.clone().unwrap_or_default(),
                value_url,
                error: String::new(),
            }
        })
        .collect();

    if let Some(c) = cfg {
        if let Some(ref pw_col) = c.password_field {
            if prefill.is_none() {
                result.push(FormField {
                    name: pw_col.clone(),
                    kind: "password",
                    value: String::new(),
                    nullable: false,
                    readonly: false,
                    fk_table: String::new(),
                    is_password: true,
                    choices: Vec::new(),
                    help: String::new(),
                    widget: String::new(),
                    value_url: String::new(),
                    error: String::new(),
                });
            }
        }
    }

    result
}

/// Validate one create/edit form submission against the model's
/// editable fields, returning `field_name → message` for every
/// failure (gaps2 #43).
///
/// We show ALL field errors at once, each below its own input;
/// this is the server-side pass that produces them. It walks the SAME
/// `FormField` list the form renders (via `form_fields_for`) so the
/// exclusion logic — pk / noform / auto_now / auto_now_add / password —
/// is shared and can't drift from what the user actually sees.
///
/// The map is collected exhaustively (never break on the first
/// failure) so a single re-render surfaces every problem. An empty map
/// means "submit is clean as far as static validation can tell"; the
/// handler then proceeds to the DB write, where a UNIQUE / constraint
/// violation that validation can't predict still surfaces at the top
/// of the form.
pub(crate) fn validate_form(
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> std::collections::BTreeMap<String, String> {
    let mut errors = std::collections::BTreeMap::new();
    let fields = form_fields_for(model, Some(form), cfg);
    for field in &fields {
        // The synthetic password field runs its own confirm flow in
        // `insert_row`; static validation doesn't second-guess it.
        if field.is_password || field.kind == "password" {
            continue;
        }
        // Readonly fields are never written from the form — skip.
        if field.readonly {
            continue;
        }
        // File/image uploads aren't carried in `form` as a plain value
        // (multipart parts become storage keys, and an edit that keeps
        // the current file omits the column entirely). The template
        // already gates `required` on "no current value", so a
        // server-side required-check here would produce false
        // positives. Skip these kinds outright.
        if field.kind == "file" || field.kind == "image" {
            continue;
        }

        let raw = form.get(&field.name).map(|s| s.as_str()).unwrap_or("");
        let value = raw.trim();
        let col = model.fields.iter().find(|c| c.name == field.name);

        // --- Required ---------------------------------------------------
        // A field is required when its column is NOT nullable and has
        // no DB default. Bool checkboxes are exempt: an unchecked box
        // submits nothing, which the write path coerces to `false`.
        let required = match col {
            Some(c) => !c.nullable && c.default.is_empty(),
            None => false,
        };
        if required && value.is_empty() && field.kind != "bool" {
            errors.insert(field.name.clone(), "This field is required.".to_string());
            // Nothing more to validate on an empty required field.
            continue;
        }
        // A non-empty value is what the remaining checks inspect; an
        // empty optional field is fine.
        if value.is_empty() {
            continue;
        }

        // --- Type-shape checks -----------------------------------------
        match field.kind {
            "number" => {
                let is_float = matches!(
                    col.map(|c| c.ty),
                    Some(SqlType::Real) | Some(SqlType::Double) | Some(SqlType::Decimal)
                );
                let ok = if is_float {
                    value.parse::<f64>().is_ok()
                } else {
                    // Integer columns: accept an integer; fall back to
                    // f64 only when the SqlType is unknown.
                    value.parse::<i64>().is_ok()
                };
                if !ok {
                    errors.insert(field.name.clone(), "Enter a valid number.".to_string());
                }
            }
            "date" => {
                if chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
                    errors.insert(field.name.clone(), "Enter a valid date.".to_string());
                }
            }
            "time" => {
                if !valid_time(value) {
                    errors.insert(field.name.clone(), "Enter a valid time.".to_string());
                }
            }
            "datetime-local" => {
                if !valid_datetime_local(value) {
                    errors.insert(
                        field.name.clone(),
                        "Enter a valid date and time.".to_string(),
                    );
                }
            }
            "select" => {
                if !field.choices.iter().any(|c| c.value == value) {
                    errors.insert(field.name.clone(), "Select a valid option.".to_string());
                }
            }
            _ => {}
        }

        // --- max_length ------------------------------------------------
        if let Some(c) = col {
            if c.max_length > 0 && value.chars().count() > c.max_length as usize {
                // A type-shape error already populated this field — the
                // length error is secondary; only set it if nothing else
                // claimed the slot.
                errors
                    .entry(field.name.clone())
                    .or_insert_with(|| format!("Must be at most {} characters.", c.max_length));
            }
        }
    }
    errors
}

/// Accept `HH:MM` or `HH:MM:SS` (the shapes a `<input type="time">`
/// emits). Lenient: the DB layer parses the canonical form, so we only
/// reject values that are obviously not a time.
fn valid_time(value: &str) -> bool {
    chrono::NaiveTime::parse_from_str(value, "%H:%M:%S").is_ok()
        || chrono::NaiveTime::parse_from_str(value, "%H:%M").is_ok()
}

/// Accept `YYYY-MM-DDTHH:MM` or `…:SS` (what `<input
/// type="datetime-local">` emits). Lenient for the same reason as
/// `valid_time`.
fn valid_datetime_local(value: &str) -> bool {
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").is_ok()
        || chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").is_ok()
}

// =========================================================================
// M2M form context
// =========================================================================

/// Maximum number of selectable-option rows loaded for an M2M picker.
///
/// Matches the FK picker's default `page_size` (20 per page, up to
/// ~10 pages shown before HTMX search kicks in) at the same order of
/// magnitude. Selected items that fall outside this window are fetched
/// separately and appended so they always render as pre-checked.
///
/// Raise this if the typical catalogue is larger and the HTMX
/// chip-picker (gaps2 follow-up) hasn't landed yet. Do NOT remove the
/// cap: without it, a 100k-row target table materialises the whole
/// table into memory on every form render.
pub(crate) const M2M_OPTION_CAP: u64 = 200;

/// Template-facing description of one M2M field on the parent form.
///
/// The admin's form template loops over `m2m_fields` after the
/// regular `fields` and renders a checkbox list per entry: every
/// candidate child row from `candidates`, with the ones in
/// `selected_values` pre-checked. The form POST handler walks the
/// same set on submit and calls `set_junction_dynamic` to persist
/// the new selection.
///
/// The chip-picker template branch at `field_editor.html:m2m` is the
/// future-facing UX for large child sets (HTMX typeahead). The v1
/// checkbox list rendered against `m2m_fields` works for the
/// permissions-app shape (tens of permissions per content type).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct M2MFormField {
    /// Field ident on the parent struct (`"permissions"` for
    /// `Group.permissions`). Used as the form input name —
    /// the POST handler reads `m2m_<field_name>` multi-values.
    pub name: String,
    /// Display label — falls back to the field name. Title-cased
    /// at the template layer for the section heading.
    pub label: String,
    /// Junction-table name derived by the migration engine. Carried
    /// for the form POST so the handler doesn't have to re-look-up.
    /// The string is internal — application code reaches it via
    /// the macro-emitted `<Parent>::<field>_junction_table()`.
    pub junction_table: String,
    /// Candidate child rows for the picker.
    ///
    /// Bounded to [`M2M_OPTION_CAP`] rows so a large target table
    /// does not materialise into memory on every form render.
    /// Currently-selected items that fall outside the cap window are
    /// fetched separately and appended so they always appear as
    /// pre-checked entries in the list.
    pub candidates: Vec<M2MCandidate>,
    /// Currently-selected child PKs, in string form. The template
    /// pre-checks any candidate whose `value` matches an entry here.
    pub selected_values: Vec<String>,
}

/// One row in the M2M candidate list — the value the form binds
/// (the child's PK string) and a human-readable label.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct M2MCandidate {
    pub value: String,
    pub label: String,
}

/// Build the per-M2M-field form context for `parent`.
///
/// For each entry in `parent.m2m_relations`:
///   1. Look up the target model's `ModelMeta` from the registry.
///   2. Fetch every row in the target table (string-shaped via
///      `DynQuerySet::fetch_as_strings`).
///   3. If `parent_pk_value` is `Some(pk)` (edit form), query the
///      auto-generated junction for the current selection and
///      pre-check those entries; on create forms (`None`), the
///      selection starts empty.
///
/// Async because both queries hit the DB; called from the create /
/// edit / update handlers right before the template render.
pub(crate) async fn form_m2m_fields_for(
    parent: &ModelMeta,
    parent_pk_value: Option<&str>,
) -> Vec<M2MFormField> {
    if parent.m2m_relations.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(parent.m2m_relations.len());
    // Parent PK column — required to compute the junction
    // `parent_id` value and to load the current selection.
    let parent_pk_col = parent.fields.iter().find(|c| c.primary_key);
    for rel in &parent.m2m_relations {
        // 1) Find the target ModelMeta in the live registry.
        let Some(target) = umbral::migrate::registered_models()
            .into_iter()
            .find(|m| m.table == rel.target_table)
        else {
            // Target model isn't registered — silently skip rather
            // than panic; the admin shouldn't crash because of a
            // misconfigured app.
            continue;
        };
        let Some(child_pk_col) = target.fields.iter().find(|c| c.primary_key) else {
            continue;
        };
        // 2) Fetch up to M2M_OPTION_CAP candidate child rows, projected
        //    to (PK, str).  A second pass below appends any currently-
        //    selected items that fall outside the cap so they always
        //    render as pre-checked even when the target table is large.
        let label_col_name = target
            .fields
            .iter()
            .find(|c| c.is_string_repr)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| child_pk_col.name.clone());
        let select_cols = if label_col_name == child_pk_col.name {
            vec![child_pk_col.name.clone()]
        } else {
            vec![child_pk_col.name.clone(), label_col_name.clone()]
        };
        let candidate_rows = match umbral::orm::DynQuerySet::for_meta(&target)
            .select_cols(&select_cols)
            .limit(M2M_OPTION_CAP)
            .fetch_as_strings()
            .await
        {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        };
        let mut candidates: Vec<M2MCandidate> = candidate_rows
            .into_iter()
            .filter_map(|row| {
                let value = row.get(&child_pk_col.name).cloned()?;
                let label = row
                    .get(&label_col_name)
                    .cloned()
                    .unwrap_or_else(|| value.clone());
                Some(M2MCandidate { value, label })
            })
            .collect();
        // 3) Current selection — only on edit forms (where we have a
        //    parent PK). Junction queries `SELECT DISTINCT child_id
        //    FROM <junction> WHERE parent_id = <pk>` via the core
        //    helper that handles per-side SqlType binding + per-
        //    backend decoding.
        let selected_values: Vec<String> = match (parent_pk_col, parent_pk_value) {
            (Some(pk_col), Some(pk_str)) => {
                let junction_table = format!("{}_{}", parent.table, rel.field_name);
                let parent_value = match umbral::orm::write::json_to_sea_value(
                    pk_col.ty,
                    &serde_json::Value::String(pk_str.to_string()),
                    false,
                    &pk_col.name,
                    None,
                ) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match umbral::orm::load_junction_selection(
                    &junction_table,
                    parent_value,
                    child_pk_col.ty,
                    Some(parent.name.as_str()),
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        // 4) Ensure every currently-selected item appears in the
        //    candidates list even if it falls beyond M2M_OPTION_CAP.
        //    Build a set of PKs already in candidates; fetch any
        //    missing selected rows by their PKs and append them.
        if !selected_values.is_empty() {
            let in_candidates: std::collections::HashSet<&str> =
                candidates.iter().map(|c| c.value.as_str()).collect();
            let missing: Vec<String> = selected_values
                .iter()
                .filter(|v| !in_candidates.contains(v.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                let extra_rows = umbral::orm::DynQuerySet::for_meta(&target)
                    .select_cols(&select_cols)
                    .filter_in_strings(&child_pk_col.name, &missing)
                    .fetch_as_strings()
                    .await
                    .unwrap_or_default();
                for row in extra_rows {
                    let Some(value) = row.get(&child_pk_col.name).cloned() else {
                        continue;
                    };
                    let label = row
                        .get(&label_col_name)
                        .cloned()
                        .unwrap_or_else(|| value.clone());
                    candidates.push(M2MCandidate { value, label });
                }
            }
        }
        out.push(M2MFormField {
            name: rel.field_name.clone(),
            label: rel.field_name.clone(),
            junction_table: format!("{}_{}", parent.table, rel.field_name),
            candidates,
            selected_values,
        });
    }
    out
}

/// Coerce a stored DB value into the shape the corresponding HTML input
/// expects. Timestamptz strips to `yyyy-mm-ddThh:mm` (the format
/// `datetime-local` accepts); Time drops subseconds; everything else
/// passes through.
pub(crate) fn format_for_input(raw: &str, ty: SqlType) -> String {
    if raw.is_empty() {
        return String::new();
    }
    match ty {
        SqlType::Timestamptz => match chrono::DateTime::parse_from_rfc3339(raw) {
            // Gap 106: the stored value is UTC; convert to the
            // active project timezone before stripping to the
            // `datetime-local` wire form so users see local
            // wall-clock time in the form. Settings absent →
            // active_tz() returns UTC and this is a no-op.
            Ok(dt) => {
                let utc = dt.with_timezone(&chrono::Utc);
                let local = umbral::timezone::utc_to_naive_local(utc);
                local.format("%Y-%m-%dT%H:%M").to_string()
            }
            Err(_) => raw.to_string(),
        },
        SqlType::Time => {
            if let Some(dot) = raw.find('.') {
                raw[..dot].to_string()
            } else {
                raw.to_string()
            }
        }
        _ => raw.to_string(),
    }
}

/// Pick the input kind string the `field_editor` macro dispatches on.
///
/// String columns map to a small multi-line textarea by default —
/// SQL `TEXT` is unbounded in both backends so a single-line input
/// truncates long content visually; a 2-row textarea handles names
/// and bodies equally well. UUID / Inet / Cidr / MacAddr stay as
/// single-line inputs because their values are fixed-format and never
/// benefit from extra height.
///
/// `Text` columns split on `max_length`: a bounded `String` field
/// (`#[umbral(max_length = N)]`) is a single-line input with the cap
/// surfaced via the HTML `maxlength` attribute, mirroring a SQL
/// VARCHAR. An unbounded `Text` field gets a textarea — the column
/// is built for prose.
pub(crate) fn input_kind(col: &umbral::migrate::Column) -> &'static str {
    // An explicit `#[umbral(widget = "...")]` hint wins for the editor
    // kinds the field editor knows how to render (features.md #4).
    // An unrecognised widget name falls through to the SqlType-derived
    // kind — a soft no-op, so third-party widget names don't break the
    // form. Choices/multichoice/FK still win over a widget hint because
    // those are structural (closed set / relation), not presentation.
    if !col.is_multichoice && col.choices.is_empty() && !matches!(col.ty, SqlType::ForeignKey) {
        match col.widget.as_deref() {
            Some("markdown") => return "markdown",
            Some("rte") => return "rte",
            Some("code") => return "code",
            Some("textarea") => return "textarea",
            // File/image upload widgets (Wave 4). FileField/ImageField
            // set these by default; a plain Text column with
            // `#[umbral(widget = "file" | "image")]` opts in too. The
            // value stored in the column is the storage key; the form
            // renders a native `<input type="file">` and the POST is
            // multipart, with the upload stored via the ambient Storage.
            Some("file") => return "file",
            Some("image") => return "image",
            _ => {}
        }
    }
    // MultiChoice columns (CSV-encoded TEXT carrying multiple
    // ChoiceField variants) take precedence: the value is closed-set
    // but multi-valued, so the `<select>` widget can't represent it.
    // Render a checkbox-chip group instead.
    if col.is_multichoice {
        return "multiselect";
    }
    // Single-valued choices columns: stored as Text in the DB but
    // should render as a <select>, not an <input>.
    if !col.choices.is_empty() {
        return "select";
    }
    match col.ty {
        SqlType::SmallInt
        | SqlType::Integer
        | SqlType::BigInt
        | SqlType::Real
        | SqlType::Double => "number",
        SqlType::Boolean => "bool",
        SqlType::Text => {
            if col.max_length > 0 {
                "text"
            } else {
                "textarea"
            }
        }
        SqlType::Uuid => "text",
        SqlType::Date => "date",
        SqlType::Time => "time",
        SqlType::Timestamptz => "datetime-local",
        SqlType::Json => "json",
        SqlType::Array(_) => "json",
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => "text",
        SqlType::FullText => "textarea",
        // gaps2 #70: text-backed PG types. XML documents are typically
        // multi-line → textarea; ltree paths / bit strings are short →
        // a plain text input.
        SqlType::Xml => "textarea",
        SqlType::Ltree | SqlType::Bit => "text",
        SqlType::ForeignKey => "fk",
        // Bytes columns render as a plain text input today: the admin
        // doesn't yet ship a file-upload widget for raw byte payloads.
        // Users submit values as hex strings (the form parser accepts
        // the hex shape via `coerce_bytes`).
        SqlType::Bytes => "text",
        // BUG-10: Decimal columns render as plain text — the admin's
        // numeric input would lose precision via JavaScript's f64.
        // Users type the canonical string form ("19.95"); the REST
        // dynamic path parses it via `rust_decimal::Decimal::from_str`.
        SqlType::Decimal => "text",
    }
}

// =========================================================================
// Model view (detail page columns)
// =========================================================================

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ModelView {
    pub name: String,
    pub table: String,
    pub fields: Vec<ColumnView>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ColumnView {
    pub name: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// Lowercase SQL type name for template filter logic.
    pub sql_type: String,
    /// Form/display widget kind for this column, from `input_kind`:
    /// `"image"` / `"file"` for FileField/ImageField columns, otherwise
    /// the SqlType-derived kind (`"text"`, `"number"`, …). The
    /// changelist + preview templates branch on `"image"` / `"file"`
    /// to render a thumbnail / download link instead of the raw
    /// storage key.
    pub kind: String,
}

/// Lowercase SQL-type label rendered into the detail view's column
/// chips so templates can branch on type kind without inspecting
/// `SqlType` directly.
pub(crate) fn sql_type_name(ty: SqlType) -> &'static str {
    match ty {
        SqlType::SmallInt | SqlType::Integer => "integer",
        SqlType::BigInt => "bigint",
        SqlType::Real | SqlType::Double => "number",
        SqlType::Boolean => "boolean",
        SqlType::Text => "text",
        SqlType::Date => "date",
        SqlType::Time => "time",
        SqlType::Timestamptz => "datetime",
        SqlType::Uuid => "uuid",
        SqlType::Json => "json",
        SqlType::ForeignKey => "fk",
        SqlType::Array(_) => "array",
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => "text",
        SqlType::FullText => "text",
        // gaps2 #70: text-backed PG types display as text in the admin.
        SqlType::Xml | SqlType::Ltree | SqlType::Bit => "text",
        SqlType::Bytes => "bytes",
        SqlType::Decimal => "decimal",
    }
}

/// Template-facing model description — every column, in declaration
/// order. Used by the detail page.
pub(crate) fn model_for_template(model: &ModelMeta) -> ModelView {
    ModelView {
        name: model.name.clone(),
        table: model.table.clone(),
        fields: model
            .fields
            .iter()
            .map(|c| ColumnView {
                name: c.name.clone(),
                nullable: c.nullable,
                primary_key: c.primary_key,
                sql_type: sql_type_name(c.ty).to_string(),
                kind: input_kind(c).to_string(),
            })
            .collect(),
    }
}

/// Same as `model_for_template` but filtered to the configured
/// `display_cols`. Used by the changelist when the admin model
/// restricts which columns appear in the table.
pub(crate) fn model_for_template_cols(model: &ModelMeta, display_cols: &[String]) -> ModelView {
    let valid: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let fields: Vec<ColumnView> = display_cols
        .iter()
        .filter(|n| valid.contains(n.as_str()))
        .map(|n| {
            let col = model.fields.iter().find(|c| &c.name == n).unwrap();
            ColumnView {
                name: col.name.clone(),
                nullable: col.nullable,
                primary_key: col.primary_key,
                sql_type: sql_type_name(col.ty).to_string(),
                kind: input_kind(col).to_string(),
            }
        })
        .collect();
    ModelView {
        name: model.name.clone(),
        table: model.table.clone(),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::{form_fields_for, format_for_input, input_kind};
    use umbral::migrate::{Column, ModelMeta};
    use umbral::orm::{FkAction, SqlType};

    /// Reproduce ShowcaseEntry's `long_content` EXACTLY — Model + Form
    /// derives, `#[form(...)]` before `#[umbral(widget)]`, on an
    /// `Option<String>` — through the real macro → `ModelMeta::for_`
    /// → `form_fields_for`, and assert the rendered `field.kind` is
    /// `markdown` (not a bare `text` input). This is the end-to-end
    /// answer to "what is field.kind?" using the actually-compiled code.
    #[derive(Debug, Clone, Default, sqlx::FromRow, umbral::orm::Model, umbral::forms::Form)]
    #[umbral(table = "repro_showcase")]
    #[allow(dead_code, private_interfaces)]
    struct Repro {
        pub id: i64,
        #[form(required, length(min = 2, max = 120))]
        pub project_name: String,
        #[form(optional, length(max = 20_000))]
        #[umbral(widget = "markdown")]
        pub long_content: Option<String>,
    }

    #[test]
    fn showcase_long_content_renders_as_markdown_not_input() {
        let meta = ModelMeta::for_::<Repro>();

        // The column the admin reads must carry the widget.
        let col = meta
            .fields
            .iter()
            .find(|c| c.name == "long_content")
            .expect("long_content column");
        assert_eq!(
            col.widget.as_deref(),
            Some("markdown"),
            "widget lost through Model+Form derive / for_()"
        );
        assert_eq!(
            col.max_length, 0,
            "form length must NOT leak into max_length"
        );

        // input_kind — the exact function the form renderer calls.
        assert_eq!(input_kind(col), "markdown", "field.kind should be markdown");

        // And the full form-field build the template iterates.
        let fields = form_fields_for(&meta, None, None);
        let f = fields
            .iter()
            .find(|f| f.name == "long_content")
            .expect("long_content field");
        assert_eq!(f.kind, "markdown", "rendered field.kind must be markdown");
        assert_eq!(f.widget, "markdown");
    }

    /// Helper: build a `Column` carrying only the flags the
    /// form-field filter cares about. Every other field gets a
    /// neutral default. Inlined here instead of derived because
    /// `Column` has many fields without a `Default` impl + adding
    /// one would require Default impls all the way down (SqlType,
    /// FkAction); a single test isn't worth that surface bump.
    fn col(name: &str, auto_now: bool, auto_now_add: bool, primary_key: bool) -> Column {
        Column {
            name: name.to_string(),
            ty: SqlType::Timestamptz,
            primary_key,
            nullable: false,
            fk_target: None,
            noform: false,
            db_constraint: true,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: FkAction::NoAction,
            on_update: FkAction::NoAction,
            index: false,
            auto_now_add,
            auto_now,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: None,
            slug_from: None,
        }
    }

    fn meta(table: &str, fields: Vec<Column>) -> ModelMeta {
        ModelMeta {
            name: table.to_string(),
            table: table.to_string(),
            fields,
            display: table.to_string(),
            icon: "database".to_string(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
            soft_delete: false,
            app_label: "app".to_string(),
        }
    }

    /// Regression test: a model with `auto_now` / `auto_now_add`
    /// timestamp columns (e.g. `Customer.created_at`,
    /// `Customer.updated_at`) must NOT surface those columns on the
    /// admin create / edit form. The dynamic write path fills them
    /// at INSERT / UPDATE time; rendering them as user-editable
    /// inputs would either ask the user to fill server-managed
    /// data (NOT-NULL violation on empty submit) or let them
    /// override the timestamp (defeats the annotation).
    ///
    /// Reported 2026-06-09: a Customer create form was asking the
    /// user to enter `created_at` and `updated_at` despite both
    /// being annotated `#[umbral(auto_now_add)]` / `#[umbral(auto_now)]`.
    #[test]
    fn form_excludes_auto_now_columns() {
        let model = meta(
            "customer",
            vec![
                col("id", false, false, true),
                col("phone", false, false, false),
                col("created_at", false, true, false),
                col("updated_at", true, false, false),
            ],
        );
        let fields = form_fields_for(&model, None, None);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"phone"), "regular fields still surface");
        assert!(
            !names.contains(&"created_at"),
            "auto_now_add column hidden from form; got {names:?}"
        );
        assert!(
            !names.contains(&"updated_at"),
            "auto_now column hidden from form; got {names:?}"
        );
        assert!(!names.contains(&"id"), "PK already excluded (sanity)");
    }

    /// features.md #4: `#[umbral(widget = "markdown")]` drives the form
    /// input kind, and `#[umbral(help = "...")]` reaches the form field
    /// (the FieldSpec::help doc-comment has long claimed it does).
    #[test]
    fn widget_and_help_reach_the_form_field() {
        let mut body = col("body", false, false, false);
        body.ty = SqlType::Text;
        body.widget = Some("markdown".to_string());
        body.help = "Markdown supported — headings, lists, code.".to_string();

        let model = meta("post", vec![col("id", false, false, true), body]);
        let fields = form_fields_for(&model, None, None);
        let f = fields
            .iter()
            .find(|f| f.name == "body")
            .expect("body field present");
        assert_eq!(f.kind, "markdown", "widget drives the input kind");
        assert_eq!(f.widget, "markdown", "raw widget name carried for JS");
        assert_eq!(f.help, "Markdown supported — headings, lists, code.");
    }

    /// An unrecognised widget name is a soft no-op — the field falls
    /// back to its SqlType-derived kind so a third-party widget name
    /// never breaks the form.
    #[test]
    fn unknown_widget_falls_back_to_type_kind() {
        let mut c = col("blob", false, false, false);
        c.ty = SqlType::Text; // no max_length => textarea by type
        c.widget = Some("some-future-editor".to_string());
        assert_eq!(input_kind(&c), "textarea");
    }

    /// A nullable `Option<String>` (nullable Text) with a widget hint
    /// still resolves to the editor kind — nullability is not part of
    /// the widget gate. Mirrors `ShowcaseEntry.long_content`.
    #[test]
    fn widget_applies_to_nullable_text_field() {
        let mut c = col("long_content", false, false, false);
        c.ty = SqlType::Text;
        c.nullable = true;
        c.widget = Some("markdown".to_string());
        assert_eq!(input_kind(&c), "markdown");
    }

    /// `widget = "code"` selects the CodeMirror editor kind — on a JSON
    /// column (the prime case) and on a plain String column alike.
    #[test]
    fn code_widget_selects_code_kind() {
        let mut j = col("payload", false, false, false);
        j.ty = SqlType::Json;
        j.widget = Some("code".to_string());
        assert_eq!(input_kind(&j), "code");

        let mut s = col("config", false, false, false);
        s.ty = SqlType::Text;
        s.widget = Some("code".to_string());
        assert_eq!(input_kind(&s), "code");

        // Without the widget, a JSON column keeps its default editor.
        let mut plain = col("payload2", false, false, false);
        plain.ty = SqlType::Json;
        assert_eq!(input_kind(&plain), "json");
    }

    #[test]
    fn format_for_input_coerces_rfc3339_to_datetime_local() {
        let coerced = format_for_input("2026-05-30T12:00:00+00:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T12:00");
    }

    #[test]
    fn format_for_input_handles_rfc3339_with_offset() {
        // Gap 106: stored values normalize to UTC before display.
        // The input is `17:00+05:00`, i.e. UTC `12:00`; the admin
        // form shows the UTC wall-clock since no project time_zone
        // is configured in the test process.
        let coerced = format_for_input("2026-05-30T17:00:00+05:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T12:00");
    }

    #[test]
    fn format_for_input_empty_stays_empty() {
        assert_eq!(format_for_input("", SqlType::Timestamptz), "");
        assert_eq!(format_for_input("", SqlType::Time), "");
        assert_eq!(format_for_input("", SqlType::Text), "");
    }

    #[test]
    fn format_for_input_passes_through_simple_types() {
        assert_eq!(format_for_input("2026-05-30", SqlType::Date), "2026-05-30");
        assert_eq!(format_for_input("hello", SqlType::Text), "hello");
        assert_eq!(format_for_input("42", SqlType::BigInt), "42");
    }

    #[test]
    fn format_for_input_trims_subsecond_time() {
        assert_eq!(format_for_input("12:34:56.789", SqlType::Time), "12:34:56");
        assert_eq!(format_for_input("12:34:56", SqlType::Time), "12:34:56");
        assert_eq!(format_for_input("12:34", SqlType::Time), "12:34");
    }

    #[test]
    fn format_for_input_passes_through_bad_rfc3339_unchanged() {
        let bad = "not-a-valid-timestamp";
        assert_eq!(format_for_input(bad, SqlType::Timestamptz), bad);
    }

    // ---------------------------------------------------------------
    // gaps2 #43: form validation surfaces ALL field errors at once.
    // ---------------------------------------------------------------

    /// A submission with two distinct violations — a required text
    /// field left blank AND a number field given a non-numeric value —
    /// returns a 2-entry map keyed by each field, NOT just the first.
    /// This is the unit-level proof of the "show everything at once"
    /// contract; the template renders each message under its input.
    #[test]
    fn validate_form_collects_all_field_errors() {
        let mut name = col("name", false, false, false);
        name.ty = SqlType::Text;
        name.nullable = false; // required

        let mut age = col("age", false, false, false);
        age.ty = SqlType::Integer;
        age.nullable = false; // required, but supplied (just invalid)

        let model = meta("person", vec![col("id", false, false, true), name, age]);

        let mut form = std::collections::HashMap::new();
        form.insert("name".to_string(), "".to_string()); // blank required text
        form.insert("age".to_string(), "abc".to_string()); // not a number

        let errors = super::validate_form(&model, &form, None);
        assert_eq!(errors.len(), 2, "both fields must report, got {errors:?}");
        assert_eq!(
            errors.get("name").map(String::as_str),
            Some("This field is required.")
        );
        assert_eq!(
            errors.get("age").map(String::as_str),
            Some("Enter a valid number.")
        );
    }

    /// A clean submission produces an empty map (the handler then
    /// proceeds straight to the DB write).
    #[test]
    fn validate_form_accepts_valid_submission() {
        let mut name = col("name", false, false, false);
        name.ty = SqlType::Text;
        name.nullable = false;

        let mut age = col("age", false, false, false);
        age.ty = SqlType::Integer;
        age.nullable = true; // optional

        let model = meta("person", vec![col("id", false, false, true), name, age]);

        let mut form = std::collections::HashMap::new();
        form.insert("name".to_string(), "Ada".to_string());
        form.insert("age".to_string(), "42".to_string());

        let errors = super::validate_form(&model, &form, None);
        assert!(errors.is_empty(), "valid form should pass, got {errors:?}");
    }

    /// An out-of-set choice value is rejected; a valid one passes.
    #[test]
    fn validate_form_rejects_invalid_choice() {
        let mut status = col("status", false, false, false);
        status.ty = SqlType::Text;
        status.nullable = false;
        status.choices = vec!["draft".to_string(), "published".to_string()];
        status.choice_labels = vec!["Draft".to_string(), "Published".to_string()];

        let model = meta("post", vec![col("id", false, false, true), status]);

        let mut bad = std::collections::HashMap::new();
        bad.insert("status".to_string(), "archived".to_string());
        let errors = super::validate_form(&model, &bad, None);
        assert_eq!(
            errors.get("status").map(String::as_str),
            Some("Select a valid option.")
        );

        let mut good = std::collections::HashMap::new();
        good.insert("status".to_string(), "draft".to_string());
        assert!(super::validate_form(&model, &good, None).is_empty());
    }

    /// A non-nullable column WITH a DB default is not required (the DB
    /// fills it), so a blank submission validates clean.
    #[test]
    fn validate_form_default_satisfies_required() {
        let mut flag = col("flag", false, false, false);
        flag.ty = SqlType::Text;
        flag.nullable = false;
        flag.default = "active".to_string();

        let model = meta("thing", vec![col("id", false, false, true), flag]);
        let mut form = std::collections::HashMap::new();
        form.insert("flag".to_string(), "".to_string());
        assert!(
            super::validate_form(&model, &form, None).is_empty(),
            "a column with a DEFAULT is not required"
        );
    }
}
