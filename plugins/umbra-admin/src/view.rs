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
pub(crate) fn sidebar_apps(
    state: &AdminState,
    user: &umbra_auth::AuthUser,
) -> Vec<SidebarApp> {
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
            FormField {
                name: c.name.clone(),
                kind: input_kind(c.ty),
                value: format_for_input(&raw, c.ty),
                nullable: c.nullable,
                readonly: is_readonly,
                fk_table,
                is_password: false,
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
                });
            }
        }
    }

    result
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
            Ok(dt) => dt.format("%Y-%m-%dT%H:%M").to_string(),
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
pub(crate) fn input_kind(ty: SqlType) -> &'static str {
    match ty {
        SqlType::SmallInt
        | SqlType::Integer
        | SqlType::BigInt
        | SqlType::Real
        | SqlType::Double => "number",
        SqlType::Boolean => "bool",
        SqlType::Text => "string",
        SqlType::Uuid => "text",
        SqlType::Date => "date",
        SqlType::Time => "time",
        SqlType::Timestamptz => "datetime-local",
        SqlType::Json => "textarea",
        SqlType::Array(_) => "textarea",
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => "text",
        SqlType::FullText => "textarea",
        SqlType::ForeignKey => "fk",
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
pub(crate) fn model_for_template_cols(
    model: &ModelMeta,
    display_cols: &[String],
) -> ModelView {
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
    use super::format_for_input;
    use umbra::orm::SqlType;

    #[test]
    fn format_for_input_coerces_rfc3339_to_datetime_local() {
        let coerced = format_for_input("2026-05-30T12:00:00+00:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T12:00");
    }

    #[test]
    fn format_for_input_handles_rfc3339_with_offset() {
        let coerced = format_for_input("2026-05-30T17:00:00+05:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T17:00");
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
