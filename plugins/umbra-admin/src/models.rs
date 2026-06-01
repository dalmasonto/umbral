//! Admin-owned models: user preferences and audit log.
//!
//! These are registered via `AdminPlugin::models()` so they land in
//! migrations automatically. No special-casing — same path as any plugin.
//!
//! ## AdminUserPref
//! One row per admin user. Created on first `GET /admin/api/prefs` with
//! defaults. Persists theme, density, sidebar-collapsed state, and the
//! serialized dashboard layout (JSON blob).
//!
//! ## AdminAuditLog
//! One row per write operation (create / update / delete / bulk action).
//! The actor is the `AuthUser` resolved from the session at call time.
//! `diff_summary` is a short human description synthesized from context;
//! no field-level diffing in v1 (that is deferred).

use serde::{Deserialize, Serialize};

/// Per-user admin preferences row.
///
/// Stored in `admin_user_pref`. Created with defaults on first access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUserPref {
    pub id: i64,
    /// FK to the auth_user table. Plain `i64` until M2M / typed FK lands.
    pub user_id: i64,
    /// One of "light" | "dark" | "system".
    pub theme: String,
    /// One of "comfortable" | "compact".
    pub density: String,
    /// Whether the sidebar is collapsed to the icon rail.
    pub sidebar_collapsed: bool,
    /// Serialized `Vec<WidgetInstance>` JSON blob.
    pub dashboard_layout: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl AdminUserPref {
    /// Default prefs for a brand-new admin user.
    pub fn default_for(user_id: i64) -> Self {
        Self {
            id: 0,
            user_id,
            theme: "dark".to_string(),
            density: "comfortable".to_string(),
            sidebar_collapsed: false,
            dashboard_layout: "[]".to_string(),
            updated_at: chrono::Utc::now(),
        }
    }
}

/// One entry in the admin audit trail.
///
/// Stored in `admin_audit_log`. Written by CRUD handlers after every
/// successful mutating operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAuditLog {
    pub id: i64,
    /// FK to auth_user. Plain `i64` until typed FK lands.
    pub actor_user_id: i64,
    /// One of: "create" | "update" | "delete" | "action:<key>".
    pub action: String,
    /// SQL table name the operation touched.
    pub model: String,
    /// PK of the affected row, NULL for bulk/non-row operations.
    pub object_id: Option<i64>,
    /// Short human description, e.g. "created Post #42".
    pub diff_summary: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// =========================================================================
// DB helpers — raw SQL against the ambient pool.
//
// The ORM isn't used here because the admin plugin must bootstrap its own
// tables before the ORM's table-existence checks run. Raw sqlx is safe
// because the table names are constants.
// =========================================================================

/// Ensure the admin tables exist. Called from `AdminPlugin::on_ready`.
///
/// Uses `CREATE TABLE IF NOT EXISTS` so it is idempotent and safe to run
/// on every boot before a proper migration engine is in place for this
/// plugin's tables.
pub async fn ensure_tables(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS admin_user_pref (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id           INTEGER NOT NULL UNIQUE,
            theme             TEXT    NOT NULL DEFAULT 'dark',
            density           TEXT    NOT NULL DEFAULT 'comfortable',
            sidebar_collapsed INTEGER NOT NULL DEFAULT 0,
            dashboard_layout  TEXT    NOT NULL DEFAULT '[]',
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

/// Fetch the prefs row for `user_id`, or return a struct with defaults
/// (the row is NOT inserted; the caller decides whether to persist).
pub async fn get_prefs(
    pool: &sqlx::SqlitePool,
    user_id: i64,
) -> Result<AdminUserPref, sqlx::Error> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, user_id, theme, density, sidebar_collapsed, dashboard_layout, updated_at
         FROM admin_user_pref WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => {
            let updated_raw: String = r.try_get("updated_at").unwrap_or_default();
            AdminUserPref {
                id: r.try_get("id").unwrap_or(0),
                user_id: r.try_get("user_id").unwrap_or(user_id),
                theme: r.try_get("theme").unwrap_or_else(|_| "dark".to_string()),
                density: r
                    .try_get("density")
                    .unwrap_or_else(|_| "comfortable".to_string()),
                sidebar_collapsed: r.try_get::<bool, _>("sidebar_collapsed").unwrap_or(false),
                dashboard_layout: r
                    .try_get("dashboard_layout")
                    .unwrap_or_else(|_| "[]".to_string()),
                updated_at: updated_raw
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap_or_else(|_| chrono::Utc::now()),
            }
        }
        None => AdminUserPref::default_for(user_id),
    })
}

/// Upsert the prefs row for `user_id`.
///
/// SQLite `INSERT OR REPLACE` will reuse the existing row's `id` when
/// the `user_id` UNIQUE constraint matches.
pub async fn upsert_prefs(
    pool: &sqlx::SqlitePool,
    prefs: &AdminUserPref,
) -> Result<(), sqlx::Error> {
    let sidebar_int: i64 = if prefs.sidebar_collapsed { 1 } else { 0 };
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO admin_user_pref
            (user_id, theme, density, sidebar_collapsed, dashboard_layout, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET
            theme             = excluded.theme,
            density           = excluded.density,
            sidebar_collapsed = excluded.sidebar_collapsed,
            dashboard_layout  = excluded.dashboard_layout,
            updated_at        = excluded.updated_at",
    )
    .bind(prefs.user_id)
    .bind(&prefs.theme)
    .bind(&prefs.density)
    .bind(sidebar_int)
    .bind(&prefs.dashboard_layout)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append one audit log row. Fire-and-forget: log errors but don't fail
/// the originating request if the audit insert fails.
pub async fn log_audit(
    pool: &sqlx::SqlitePool,
    actor_user_id: i64,
    action: &str,
    model: &str,
    object_id: Option<i64>,
    diff_summary: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let res = sqlx::query(
        "INSERT INTO admin_audit_log
            (actor_user_id, action, model, object_id, diff_summary, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(actor_user_id)
    .bind(action)
    .bind(model)
    .bind(object_id)
    .bind(diff_summary)
    .bind(&now)
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, "admin: audit log insert failed");
    }
}

/// Fetch the last `limit` audit entries for a specific object, newest first.
pub async fn audit_for_object(
    pool: &sqlx::SqlitePool,
    model: &str,
    object_id: i64,
    limit: i64,
) -> Result<Vec<AuditEntry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, actor_user_id, action, model, object_id, diff_summary, created_at
         FROM admin_audit_log
         WHERE model = ? AND object_id = ?
         ORDER BY created_at DESC
         LIMIT ?",
    )
    .bind(model)
    .bind(object_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    use sqlx::Row;
    Ok(rows
        .into_iter()
        .map(|r| AuditEntry {
            id: r.try_get("id").unwrap_or(0),
            actor_user_id: r.try_get("actor_user_id").unwrap_or(0),
            action: r.try_get("action").unwrap_or_default(),
            model: r.try_get("model").unwrap_or_default(),
            object_id: r.try_get("object_id").ok(),
            diff_summary: r.try_get("diff_summary").unwrap_or_default(),
            created_at: r.try_get("created_at").unwrap_or_default(),
        })
        .collect())
}

/// Template-friendly audit entry (all fields are strings for minijinja).
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
