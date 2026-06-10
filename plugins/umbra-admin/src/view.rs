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
use umbra::migrate::ModelMeta;
use umbra::orm::SqlType;

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
pub(crate) fn sidebar_apps(state: &AdminState, user: &umbra_auth::AuthUser) -> Vec<SidebarApp> {
    state
        .registry
        .apps(user)
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
    /// `#[umbra(help = "...")]` text. Rendered as a hint line under
    /// the input. Empty string = no hint (the template skips the
    /// markup). Honors the `FieldSpec::help` doc-comment's long-
    /// standing claim that help reaches the admin form.
    pub help: String,
    /// `#[umbra(widget = "...")]` presentation hint (features.md #4),
    /// already folded into `kind` by `input_kind`. Kept on the struct
    /// too so the template / future client JS can branch on the raw
    /// widget name (e.g. load a markdown editor module). Empty = none.
    pub widget: String,
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
///   2. the derive attribute `#[umbra(noform)]` is set;
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
            FormField {
                name: c.name.clone(),
                kind: input_kind(c),
                value: format_for_input(&raw, c.ty),
                nullable: c.nullable,
                readonly: is_readonly,
                fk_table,
                is_password: false,
                choices,
                help: c.help.clone(),
                widget: c.widget.clone().unwrap_or_default(),
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
                });
            }
        }
    }

    result
}

// =========================================================================
// M2M form context
// =========================================================================

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
    /// Candidate child rows for the picker. One entry per existing
    /// row in the target table. v1 loads every row — fine for the
    /// permissions-app scale; large catalogues should switch to the
    /// HTMX chip-picker variant when it lands.
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
        let Some(target) = umbra::migrate::registered_models()
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
        // 2) Fetch every candidate child row, projected to (PK, str).
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
        let candidate_rows = match umbra::orm::DynQuerySet::for_meta(&target)
            .select_cols(&select_cols)
            .fetch_as_strings()
            .await
        {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        };
        let candidates: Vec<M2MCandidate> = candidate_rows
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
                let parent_value = match umbra::orm::write::json_to_sea_value(
                    pk_col.ty,
                    &serde_json::Value::String(pk_str.to_string()),
                    false,
                    &pk_col.name,
                ) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match umbra::orm::load_junction_selection(
                    &junction_table,
                    parent_value,
                    child_pk_col.ty,
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
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
                let local = umbra::timezone::utc_to_naive_local(utc);
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
/// (`#[umbra(max_length = N)]`) is a single-line input with the cap
/// surfaced via the HTML `maxlength` attribute, mirroring a SQL
/// VARCHAR. An unbounded `Text` field gets a textarea — the column
/// is built for prose.
pub(crate) fn input_kind(col: &umbra::migrate::Column) -> &'static str {
    // An explicit `#[umbra(widget = "...")]` hint wins for the editor
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
    use umbra::migrate::{Column, ModelMeta};
    use umbra::orm::{FkAction, SqlType};

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
    /// being annotated `#[umbra(auto_now_add)]` / `#[umbra(auto_now)]`.
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

    /// features.md #4: `#[umbra(widget = "markdown")]` drives the form
    /// input kind, and `#[umbra(help = "...")]` reaches the form field
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
}
