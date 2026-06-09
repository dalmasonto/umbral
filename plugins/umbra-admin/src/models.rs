//! Admin-owned models: user preferences and audit log.
//!
//! Registered via [`crate::AdminPlugin::models`] so they flow through the
//! framework's migration engine like any other plugin's models. No raw
//! `CREATE TABLE`, no `on_ready` bootstrap — the same path Django takes
//! for `django.contrib.admin.LogEntry`.
//!
//! ## AdminUserPref
//! One row per admin user. Created the first time a user lands on
//! `GET /admin/api/prefs`. Holds theme, density, sidebar-collapsed
//! state, and the serialized dashboard layout.
//!
//! ## AdminAuditLog
//! One row per write operation (create / update / delete / bulk action).
//! The actor is the `AuthUser` resolved from the session at call time;
//! `diff_summary` is a short human description synthesized from context
//! (no field-level diffing in v1).
//!
//! ## Why the model is `noedit`
//! Every field on both models is marked `#[umbra(noedit)]` so the admin
//! exposes them as read-only — users see preferences and audit history
//! in the UI but cannot mutate them through the form path. Writes flow
//! exclusively through this module's typed helpers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::orm::Model;

// =========================================================================
// AdminUserPref
// =========================================================================

/// Per-user admin preferences row.
///
/// One row per admin user, keyed by `user_id`. The framework cannot yet
/// express a UNIQUE constraint via `#[derive(Model)]`, so the
/// one-row-per-user invariant is enforced at the application layer in
/// [`fetch_or_default`] + [`upsert`]: a fetch-then-save flow with
/// last-write-wins semantics. When the macro grows `#[umbra(unique)]`,
/// `user_id` gets the attribute and the race window closes.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(display = "User preference", icon = "settings-2")]
pub struct AdminUserPref {
    pub id: i64,
    /// FK to `auth_user` (typed FK at the Model level is a follow-on;
    /// `i64` for now).
    #[umbra(noedit)]
    pub user_id: i64,
    /// One of "light" | "dark" | "system".
    #[umbra(noedit)]
    pub theme: String,
    /// One of "comfortable" | "compact".
    #[umbra(noedit)]
    pub density: String,
    /// Whether the sidebar is collapsed to the icon rail.
    #[umbra(noedit)]
    pub sidebar_collapsed: bool,
    /// Serialized `Vec<WidgetInstance>` JSON blob.
    #[umbra(noedit)]
    pub dashboard_layout: String,
    /// gaps2 #11 — free-form JSON map of per-table UI state. Shape:
    ///
    /// ```jsonc
    /// {
    ///   "tables": {
    ///     "product": {
    ///       "filters":  { "status": "active" },
    ///       "search":   "widget",
    ///       "sort":     "-price",
    ///       "per_page": 50
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// `Option<String>` so existing rows (NULL after the migration's
    /// ADD COLUMN) read as "no prefs yet" without a backfill pass.
    /// The first time a user visits a changelist, their current
    /// query string gets persisted; on a subsequent paramless visit,
    /// the changelist handler 303-redirects to the saved URL shape.
    /// Cross-tab / cross-device continuity for free.
    #[umbra(noedit)]
    pub preferences: Option<String>,
    #[umbra(noedit)]
    pub updated_at: DateTime<Utc>,
}

impl AdminUserPref {
    /// Default prefs for a brand-new admin user. The struct is returned
    /// with `id = 0` so a subsequent `.save()` becomes an INSERT.
    pub fn default_for(user_id: i64) -> Self {
        Self {
            id: 0,
            user_id,
            theme: "dark".to_string(),
            density: "comfortable".to_string(),
            sidebar_collapsed: false,
            dashboard_layout: "[]".to_string(),
            preferences: None,
            updated_at: Utc::now(),
        }
    }
}

/// gaps2 #11 — per-table changelist UI state. Persisted as a nested
/// entry under `preferences.tables.<table>` in the JSON blob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TablePref {
    /// Map of `field_name → string-value` for active facet filters.
    /// Empty map omits the `?filter_*=...` params on redirect.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub filters: std::collections::HashMap<String, String>,
    /// Current search term (becomes `?search=...`). Empty string is
    /// dropped from the URL.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub search: String,
    /// Sort directive in `[-]col_name` shape — empty = no override
    /// (falls through to the model's default ordering).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sort: String,
    /// Page size override. `None` falls through to the configured
    /// admin default. Stored as `u32` because some callers cast to
    /// `usize` and some to `i64`; `u32` round-trips through both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_page: Option<u32>,
    /// Hidden columns on this table. Round-2 follow-up to the
    /// initial gaps2 #11 ship. Render path filters
    /// `display_cols` against this list; the toggle endpoint
    /// `POST /admin/{table}/columns/{column}/toggle` flips
    /// membership and returns an HX-Trigger to refresh the table.
    /// Empty vec = every column visible (the default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_cols: Vec<String>,
}

/// gaps2 #11 — read the persisted UI state for `(user_id, table)`.
///
/// Returns `None` when:
/// - the user has no prefs row yet (NULL `preferences` column);
/// - the JSON blob is present but missing `tables.<table>`;
/// - the JSON blob is malformed (treated as "no prefs" rather than
///   surfacing a parse error — the next write overwrites with a
///   valid shape).
pub async fn get_table_pref(user_id: i64, table: &str) -> Result<Option<TablePref>, sqlx::Error> {
    let prefs = fetch_or_default(user_id).await?;
    let Some(raw) = prefs.preferences.as_deref() else {
        return Ok(None);
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    let Some(table_obj) = root.get("tables").and_then(|t| t.get(table)) else {
        return Ok(None);
    };
    let Ok(pref) = serde_json::from_value::<TablePref>(table_obj.clone()) else {
        return Ok(None);
    };
    Ok(Some(pref))
}

/// gaps2 #11 — merge a new `TablePref` into `preferences.tables.<table>`.
///
/// Read-modify-write rather than a JSON_SET / json_replace SQL pass:
/// the shape lives in user code, and the v1 single-tab usage doesn't
/// race. When two tabs CAN race (the gap's `hx-trigger="change
/// delay:500ms"` follow-up), the merge moves to the SQL layer; the
/// in-memory merge here is forward-compatible because the JSON
/// structure is the same either way.
pub async fn set_table_pref(
    user_id: i64,
    table: &str,
    pref: &TablePref,
) -> Result<(), sqlx::Error> {
    let existing = fetch_or_default(user_id).await?;
    let mut root: serde_json::Value = existing
        .preferences
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let pref_value = serde_json::to_value(pref).unwrap_or(serde_json::Value::Null);
    root.as_object_mut()
        .expect("root is always an object")
        .entry("tables")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("tables is always an object")
        .insert(table.to_string(), pref_value);
    let mut next = existing;
    next.preferences = Some(root.to_string());
    upsert(next).await?;
    Ok(())
}

/// gaps2 #11 round 2 — read the "last viewed admin URL" for
/// `user_id`. Used by the admin index handler to redirect
/// `/admin/` → the user's last working changelist.
///
/// Returns `None` when no prefs row yet, when `preferences.last_path`
/// is missing, or when the value isn't a string.
pub async fn get_last_path(user_id: i64) -> Result<Option<String>, sqlx::Error> {
    let prefs = fetch_or_default(user_id).await?;
    let Some(raw) = prefs.preferences.as_deref() else {
        return Ok(None);
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    Ok(root
        .get("last_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// gaps2 #11 round 2 — write `last_path` to `preferences.last_path`.
/// Read-modify-write through the JSON blob, same pattern as
/// `set_table_pref`.
pub async fn set_last_path(user_id: i64, path: &str) -> Result<(), sqlx::Error> {
    let existing = fetch_or_default(user_id).await?;
    let mut root: serde_json::Value = existing
        .preferences
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    root.as_object_mut()
        .expect("root is always an object")
        .insert(
            "last_path".to_string(),
            serde_json::Value::String(path.to_string()),
        );
    let mut next = existing;
    next.preferences = Some(root.to_string());
    upsert(next).await?;
    Ok(())
}

/// gaps2 #11 round 2 — read a saved widget-period override for
/// `widget_key` on `preferences.dashboard.widget_periods.<key>`.
///
/// Returns `None` when no override is set. The dashboard's widget-
/// data handler treats `None` as "fall through to the widget's
/// registration-time `default_period`."
pub async fn get_widget_period(
    user_id: i64,
    widget_key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let prefs = fetch_or_default(user_id).await?;
    let Some(raw) = prefs.preferences.as_deref() else {
        return Ok(None);
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    Ok(root
        .get("dashboard")
        .and_then(|d| d.get("widget_periods"))
        .and_then(|p| p.get(widget_key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// gaps2 #11 round 2 — persist a widget-period override at
/// `preferences.dashboard.widget_periods.<widget_key>`. Same
/// read-modify-write merge as `set_table_pref` / `set_last_path`.
pub async fn set_widget_period(
    user_id: i64,
    widget_key: &str,
    period: &str,
) -> Result<(), sqlx::Error> {
    let existing = fetch_or_default(user_id).await?;
    let mut root: serde_json::Value = existing
        .preferences
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    root.as_object_mut()
        .expect("root is always an object")
        .entry("dashboard")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("dashboard is always an object")
        .entry("widget_periods")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("widget_periods is always an object")
        .insert(
            widget_key.to_string(),
            serde_json::Value::String(period.to_string()),
        );
    let mut next = existing;
    next.preferences = Some(root.to_string());
    upsert(next).await?;
    Ok(())
}

/// gaps2 #11 round 2 — flip a column's visibility on
/// `preferences.tables.<table>.hidden_cols`. Idempotent toggle:
/// already-hidden → visible, already-visible → hidden. Returns
/// the new visibility (`true` = now visible, `false` = now hidden)
/// so the caller can emit a precise HX-Trigger payload.
pub async fn toggle_table_col(
    user_id: i64,
    table: &str,
    column: &str,
) -> Result<bool, sqlx::Error> {
    let mut pref = get_table_pref(user_id, table).await?.unwrap_or_default();
    let now_visible = if let Some(pos) = pref.hidden_cols.iter().position(|c| c == column) {
        pref.hidden_cols.remove(pos);
        true
    } else {
        pref.hidden_cols.push(column.to_string());
        false
    };
    set_table_pref(user_id, table, &pref).await?;
    Ok(now_visible)
}

/// Fetch the prefs row for `user_id`, or return a struct filled with
/// defaults (the row is **not** inserted; the caller decides whether to
/// persist). `id == 0` distinguishes the unsaved-default case.
pub async fn fetch_or_default(user_id: i64) -> Result<AdminUserPref, sqlx::Error> {
    let existing = AdminUserPref::objects()
        .filter(admin_user_pref::USER_ID.eq(user_id))
        .first()
        .await?;
    Ok(existing.unwrap_or_else(|| AdminUserPref::default_for(user_id)))
}

/// Insert or update the prefs row.
///
/// Uses [`umbra::orm::Manager::save`] which dispatches by primary key:
/// `id == 0` → INSERT, otherwise UPDATE. The caller is responsible for
/// loading the row via [`fetch_or_default`] before mutating + persisting
/// so the `id` round-trips correctly.
pub async fn upsert(prefs: AdminUserPref) -> Result<AdminUserPref, sqlx::Error> {
    let mut prefs = prefs;
    prefs.updated_at = Utc::now();
    AdminUserPref::objects()
        .save(prefs)
        .await
        .map_err(|e| match e {
            umbra::orm::SaveError::Write(umbra::orm::WriteError::Sqlx(e)) => e,
            other => sqlx::Error::Protocol(other.to_string()),
        })
}

// =========================================================================
// AdminAuditLog
// =========================================================================

/// One entry in the admin audit trail.
///
/// Append-only via [`log`]. The admin surfaces the table read-only;
/// every column carries `#[umbra(noedit)]` so the form path can't mutate
/// rows even if someone navigates directly to the edit URL.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(display = "Audit log", icon = "scroll-text")]
pub struct AdminAuditLog {
    pub id: i64,
    /// FK to `auth_user`.
    #[umbra(noedit)]
    pub actor_user_id: i64,
    /// One of: `"create"` | `"update"` | `"delete"` | `"action:<key>"`.
    #[umbra(noedit)]
    pub action: String,
    /// SQL table name the operation touched.
    #[umbra(noedit)]
    pub model: String,
    /// PK of the affected row, NULL for bulk / non-row operations.
    #[umbra(noedit)]
    pub object_id: Option<i64>,
    /// Short human description, e.g. `"created Post #42"`.
    #[umbra(noedit)]
    pub diff_summary: String,
    #[umbra(noedit)]
    pub created_at: DateTime<Utc>,
}

/// Append one audit entry. Fire-and-forget: errors are logged but never
/// surfaced to the caller, so a CRUD handler that succeeds at its real
/// work isn't undone by an audit-write hiccup.
pub async fn log(
    actor_user_id: i64,
    action: &str,
    model: &str,
    object_id: Option<i64>,
    diff_summary: &str,
) {
    let entry = AdminAuditLog {
        id: 0,
        actor_user_id,
        action: action.to_string(),
        model: model.to_string(),
        object_id,
        diff_summary: diff_summary.to_string(),
        created_at: Utc::now(),
    };
    if let Err(e) = AdminAuditLog::objects().save(entry).await {
        tracing::error!(error = %e, "admin: audit log insert failed");
    }
}

/// Fetch the last `limit` audit entries for one object, newest first.
/// Returned as template-friendly [`AuditEntry`] values (timestamps
/// formatted as strings) for direct rendering by minijinja.
pub async fn audit_for_object(
    model: &str,
    object_id: i64,
    limit: u64,
) -> Result<Vec<AuditEntry>, sqlx::Error> {
    let rows = AdminAuditLog::objects()
        .filter(admin_audit_log::MODEL.eq(model.to_string()))
        .filter(admin_audit_log::OBJECT_ID.eq(object_id))
        .order_by(admin_audit_log::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    Ok(rows.into_iter().map(AuditEntry::from).collect())
}

/// Template-friendly audit entry — `created_at` rendered as RFC 3339
/// for minijinja, which has no `DateTime` codec.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub actor_user_id: i64,
    pub action: String,
    pub model: String,
    pub object_id: Option<i64>,
    pub diff_summary: String,
    pub created_at: String,
}

impl From<AdminAuditLog> for AuditEntry {
    fn from(row: AdminAuditLog) -> Self {
        Self {
            id: row.id,
            actor_user_id: row.actor_user_id,
            action: row.action,
            model: row.model,
            object_id: row.object_id,
            diff_summary: row.diff_summary,
            created_at: row.created_at.to_rfc3339(),
        }
    }
}

// =========================================================================
// Test-fixture helper
// =========================================================================

/// Create the admin tables on a raw pool, bypassing the migration engine.
///
/// Production code never calls this — `AdminPlugin::models()` exposes the
/// two models to the framework and the migration engine creates the
/// schema on `migrate run` like everything else. The helper exists for
/// integration tests that boot `App::builder()` without running
/// `umbra::migrate::run()` (creating migration files inside `target/`
/// every test run is the wrong tradeoff).
///
/// Idempotent — `CREATE TABLE IF NOT EXISTS` so repeated calls within a
/// single test process are safe.
#[doc(hidden)]
pub async fn ensure_tables_for_tests(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS admin_user_pref (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id           INTEGER NOT NULL,
            theme             TEXT    NOT NULL DEFAULT 'dark',
            density           TEXT    NOT NULL DEFAULT 'comfortable',
            sidebar_collapsed INTEGER NOT NULL DEFAULT 0,
            dashboard_layout  TEXT    NOT NULL DEFAULT '[]',
            preferences       TEXT,
            updated_at        TEXT    NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS admin_audit_log (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            actor_user_id INTEGER NOT NULL,
            action        TEXT    NOT NULL,
            model         TEXT    NOT NULL,
            object_id     INTEGER,
            diff_summary  TEXT    NOT NULL,
            created_at    TEXT    NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    Ok(())
}
