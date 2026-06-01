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
            updated_at: Utc::now(),
        }
    }
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
