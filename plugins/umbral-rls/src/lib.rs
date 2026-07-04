//! Postgres Row-Level Security plugin for umbral.
//!
//! Declares RLS policies once at `App::build()` time; the plugin's
//! `on_ready` hook applies them by walking the policy list, running
//! `ALTER TABLE ... ENABLE ROW LEVEL SECURITY` on each enabled table,
//! then `DROP POLICY IF EXISTS ...; CREATE POLICY ...` for each
//! registered policy. Idempotent: re-running an App boot applies the
//! same policies cleanly.
//!
//! ## Usage
//!
//! ```ignore
//! use umbral::prelude::*;
//! use umbral_rls::{Action, RlsPlugin};
//!
//! let app = App::builder()
//!     .plugin(
//!         RlsPlugin::new()
//!             .enable_on("post")
//!             .policy("post", "user_can_read", Action::Select,
//!                 "user_id = current_setting('app.user_id')::int")
//!             .policy("post", "user_can_create", Action::Insert,
//!                 "user_id = current_setting('app.user_id')::int"),
//!     )
//!     .build()?;
//! ```
//!
//! ## Backend gating
//!
//! Postgres-only. When the active backend is SQLite the plugin
//! logs a warning via `tracing` and returns `Ok(())` from `on_ready`
//! without running any DDL. The plugin doesn't refuse to boot
//! against SQLite — RLS policies are simply silently skipped, which
//! matches the behavior when a Postgres-only feature isn't
//! reachable. Users who want a hard failure on misconfiguration
//! check `crate::db::pool_dispatched()` in their own boot path.
//!
//! ## The sync-vs-async on_ready bridge
//!
//! `umbral::plugin::Plugin::on_ready` is a sync trait method, but
//! `sqlx::query(...).execute(pool)` is async. We bridge with
//! `umbral::plugin::block_on_ready(...)`, the shared helper that works
//! under multi-thread runtimes, `#[tokio::test]`'s current-thread
//! runtime, and bare (no-runtime) callers without panicking.

use umbral::plugin::{AppContext, Plugin, PluginError, block_on_ready};
use umbral::web::Router;

/// One row-level security policy.
///
/// Carries the policy name, the operation it applies to, the SQL
/// expression that gates row visibility (`USING` clause), and an
/// optional `WITH CHECK` clause (for INSERT/UPDATE). When
/// `with_check` is `None` and the action is INSERT or UPDATE,
/// Postgres uses `USING` for `WITH CHECK` automatically.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The SQL table the policy attaches to. Must match a real table
    /// at the time `on_ready` runs.
    pub table: String,
    /// The policy name (Postgres `CREATE POLICY <name>` identifier).
    /// Must be unique per table.
    pub name: String,
    /// The action the policy gates.
    pub action: Action,
    /// The `USING` clause — a SQL expression evaluated per row.
    /// The row is visible to the operation when the expression
    /// returns true. Refer to columns by name; refer to session
    /// state via `current_setting('app.user_id')` etc.
    ///
    /// # SQL injection warning
    ///
    /// This string is interpolated **verbatim** into the
    /// `CREATE POLICY ... USING (...)` DDL — Postgres DDL has no
    /// placeholder syntax for policy bodies, so no parameter binding
    /// is possible. Treat it as developer-authored SQL only. Sourcing
    /// any part of this from user input (e.g. an admin UI that
    /// reflects request bodies into policy definitions) gives every
    /// user `EXECUTE` on the server. If you genuinely need
    /// user-driven row-filtering, reach for `WHERE` clauses in
    /// application code, not RLS policies.
    pub using: String,
    /// Optional `WITH CHECK` clause. Required for INSERT to constrain
    /// new rows; defaults to the `USING` clause on UPDATE if unset.
    ///
    /// # SQL injection warning
    ///
    /// Same caveat as [`Self::using`] — verbatim interpolation into
    /// DDL, no parameter binding. Developer-authored SQL only.
    pub with_check: Option<String>,
}

/// The SQL operation a policy gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `SELECT` queries — read access.
    Select,
    /// `INSERT` — write access for new rows.
    Insert,
    /// `UPDATE` — write access for existing rows.
    Update,
    /// `DELETE` — row removal.
    Delete,
    /// Apply to every operation. Renders as `FOR ALL` in
    /// `CREATE POLICY`.
    All,
}

impl Action {
    /// The `FOR <action>` clause fragment for `CREATE POLICY`.
    fn sql_keyword(&self) -> &'static str {
        match self {
            Action::Select => "SELECT",
            Action::Insert => "INSERT",
            Action::Update => "UPDATE",
            Action::Delete => "DELETE",
            Action::All => "ALL",
        }
    }
}

/// The plugin itself. Built with [`Self::new`] and configured via
/// [`Self::enable_on`] + [`Self::policy`] builder chain, then handed
/// to `App::builder().plugin(...)`.
///
/// ## Setting the policy variable (the GUC) — REQUIRED
///
/// The plugin emits `FORCE ROW LEVEL SECURITY` at boot so policies apply to the
/// app's own DB role (audit_2 C2/R1). A policy that reads a session variable
/// like `current_setting('app.user_id')::int` needs that variable SET on the
/// connection each request uses — otherwise every RLS-enabled query errors.
///
/// Set it per request through the framework's route-context resolver, which
/// flows into the `RouteContext` the Postgres pool reads in its `before_acquire`
/// hook. The pool runs `set_config(name, value, false)` on acquire and resets
/// it on the next acquire, so a value never leaks to another request's
/// connection (audit_2 C2/R2):
///
/// ```ignore
/// use umbral::db::RouteContext;
///
/// App::builder()
///     .route_context(|req| {
///         // Resolve the user id from the AUTHENTICATED session — never a
///         // client-supplied header.
///         let uid = current_session_user_id(req.headers());
///         RouteContext::new().with_session_var("app.user_id", uid.to_string())
///     })
///     .plugin(RlsPlugin::new().policy("post", "own", Action::All,
///         "user_id = current_setting('app.user_id')::int"))
/// ```
#[derive(Debug, Clone, Default)]
pub struct RlsPlugin {
    /// Tables that should have `ENABLE ROW LEVEL SECURITY` applied.
    /// Order is preserved for deterministic DDL output.
    tables: Vec<String>,
    /// All declared policies. Walked in declaration order at
    /// on_ready time.
    policies: Vec<Policy>,
}

impl RlsPlugin {
    /// Create an empty RlsPlugin. Chain [`Self::enable_on`] /
    /// [`Self::policy`] to populate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable row-level security on a table. A subsequent
    /// `ALTER TABLE <table> ENABLE ROW LEVEL SECURITY` runs at
    /// `on_ready` time. Adding the same table twice is harmless
    /// — Postgres's ENABLE is idempotent.
    pub fn enable_on(mut self, table: impl Into<String>) -> Self {
        self.tables.push(table.into());
        self
    }

    /// Add a policy to the plugin. The plugin auto-enables RLS on
    /// the policy's table — users can skip the [`Self::enable_on`]
    /// call when every interesting table is already covered by at
    /// least one policy.
    ///
    /// # SQL injection warning
    ///
    /// `using` is **verbatim SQL** interpolated into a Postgres
    /// `CREATE POLICY` statement. There is no parameter binding —
    /// DDL doesn't support it. Pass developer-authored SQL only;
    /// never source any part of this from user input. See [`Policy::using`]
    /// for the full rationale.
    pub fn policy(
        mut self,
        table: impl Into<String>,
        name: impl Into<String>,
        action: Action,
        using: impl Into<String>,
    ) -> Self {
        let table = table.into();
        if !self.tables.iter().any(|t| t == &table) {
            self.tables.push(table.clone());
        }
        self.policies.push(Policy {
            table,
            name: name.into(),
            action,
            using: using.into(),
            with_check: None,
        });
        self
    }

    /// Add a policy with an explicit `WITH CHECK` clause. Useful
    /// when INSERT or UPDATE rules differ from the read predicate
    /// — e.g. users can read all rows but only insert ones owned
    /// by themselves.
    ///
    /// # SQL injection warning
    ///
    /// Both `using` and `with_check` are **verbatim SQL** strings
    /// interpolated into the `CREATE POLICY` DDL with no parameter
    /// binding. Developer-authored SQL only; see [`Policy::using`]
    /// for the full rationale.
    pub fn policy_with_check(
        mut self,
        table: impl Into<String>,
        name: impl Into<String>,
        action: Action,
        using: impl Into<String>,
        with_check: impl Into<String>,
    ) -> Self {
        let table = table.into();
        if !self.tables.iter().any(|t| t == &table) {
            self.tables.push(table.clone());
        }
        self.policies.push(Policy {
            table,
            name: name.into(),
            action,
            using: using.into(),
            with_check: Some(with_check.into()),
        });
        self
    }

    /// Borrow the registered tables. Exposed for tests; user code
    /// rarely needs to introspect the plugin.
    pub fn tables(&self) -> &[String] {
        &self.tables
    }

    /// Borrow the registered policies.
    pub fn policies(&self) -> &[Policy] {
        &self.policies
    }
}

impl Plugin for RlsPlugin {
    fn name(&self) -> &'static str {
        "umbral-rls"
    }

    fn routes(&self) -> Router {
        Router::new()
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        let pool = umbral::db::pool_dispatched();
        match pool {
            umbral::db::DbPool::Sqlite(_) => {
                // RLS is Postgres-only: SQLite provides NO row isolation, so a
                // SQLite backend with RlsPlugin registered is a security
                // misconfiguration — the operator believes rows are isolated
                // when they aren't. Fail CLOSED in Prod (audit_2 C2/R3, H18);
                // loud-warn in Dev so a SQLite dev setup still boots.
                let is_prod = umbral::settings::get_opt()
                    .map(|s| matches!(s.environment, umbral::Environment::Prod))
                    .unwrap_or(false);
                if is_prod {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                        "umbral-rls: Row-Level Security is Postgres-only, but the active \
                         backend is SQLite — it provides NO row isolation. Refusing to boot in \
                         Environment::Prod with a false isolation guarantee. Use Postgres, or \
                         remove RlsPlugin.",
                    ) as PluginError);
                }
                tracing::warn!(
                    plugin = "umbral-rls",
                    "Row-Level Security is Postgres-only; the SQLite backend provides NO row \
                     isolation — skipping {} table(s) and {} policy/policies. This is a HARD \
                     ERROR under Environment::Prod.",
                    self.tables.len(),
                    self.policies.len()
                );
                Ok(())
            }
            umbral::db::DbPool::Postgres(pg_pool) => {
                // on_ready is sync; sqlx is async. Use the shared
                // block_on_ready helper which handles multi-thread
                // runtimes, current-thread runtimes (#[tokio::test]),
                // and the no-runtime case without panicking.
                block_on_ready(self.apply_policies(pg_pool))
                    .map_err(|e| -> PluginError { Box::new(e) })?;
                Ok(())
            }
        }
    }
}

impl RlsPlugin {
    /// Render the DDL for one policy. Public for testability — given
    /// a `Policy`, returns the `(drop_sql, create_sql)` pair that
    /// `on_ready` would execute.
    ///
    /// `DROP POLICY IF EXISTS` is the idempotency hatch: re-running
    /// the boot applies the latest definition without duplicate-name
    /// errors. `CREATE POLICY` is the new policy. Both statements
    /// run inside the same async iteration in `apply_policies`.
    pub fn render_policy_sql(&self, p: &Policy) -> (String, String) {
        let drop_sql = format!(
            "DROP POLICY IF EXISTS \"{}\" ON \"{}\"",
            escape_ident(&p.name),
            escape_ident(&p.table)
        );
        let mut create_sql = format!(
            "CREATE POLICY \"{}\" ON \"{}\" FOR {} USING ({})",
            escape_ident(&p.name),
            escape_ident(&p.table),
            p.action.sql_keyword(),
            p.using
        );
        if let Some(check) = &p.with_check {
            create_sql.push_str(&format!(" WITH CHECK ({check})"));
        }
        (drop_sql, create_sql)
    }

    /// Render the `ALTER TABLE ... ENABLE ROW LEVEL SECURITY`
    /// statement for one table.
    pub fn render_enable_sql(&self, table: &str) -> String {
        format!(
            "ALTER TABLE \"{}\" ENABLE ROW LEVEL SECURITY",
            escape_ident(table)
        )
    }

    /// Render the `ALTER TABLE ... FORCE ROW LEVEL SECURITY` statement for one
    /// table. FORCE makes the policies apply to the table **owner** as well —
    /// without it, the single-`DATABASE_URL` app (which connects as the owner)
    /// is exempt and RLS enforces nothing (audit_2 C2/R1).
    pub fn render_force_sql(&self, table: &str) -> String {
        format!(
            "ALTER TABLE \"{}\" FORCE ROW LEVEL SECURITY",
            escape_ident(table)
        )
    }

    /// Apply every ENABLE and policy to the given Postgres pool.
    /// Async because sqlx execution is async; called from on_ready
    /// via `tokio::runtime::Handle::current().block_on()`.
    async fn apply_policies(&self, pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        // First enable RLS on every registered table. Postgres
        // tolerates ENABLE on a table that already has RLS enabled,
        // so we don't bother with IF NOT EXISTS gymnastics.
        for table in &self.tables {
            let enable = self.render_enable_sql(table);
            tracing::info!(plugin = "umbral-rls", sql = %enable, "enabling RLS");
            sqlx::query(&enable).execute(pool).await?;
            // FORCE subjects the table OWNER to the policies too (audit_2 C2/R1).
            // umbral's single-`DATABASE_URL` app connects as the owner, whom
            // Postgres exempts from non-forced RLS — so ENABLE alone silently
            // enforces nothing on the common deployment.
            let force = self.render_force_sql(table);
            tracing::info!(plugin = "umbral-rls", sql = %force, "forcing RLS on owner");
            sqlx::query(&force).execute(pool).await?;
        }
        // Then apply each policy. DROP IF EXISTS + CREATE is the
        // idempotent shape — Postgres has no CREATE OR REPLACE for
        // policies.
        for policy in &self.policies {
            let (drop_sql, create_sql) = self.render_policy_sql(policy);
            tracing::info!(
                plugin = "umbral-rls",
                sql = %drop_sql,
                "dropping policy if exists"
            );
            sqlx::query(&drop_sql).execute(pool).await?;
            tracing::info!(
                plugin = "umbral-rls",
                sql = %create_sql,
                "creating policy"
            );
            sqlx::query(&create_sql).execute(pool).await?;
        }
        Ok(())
    }
}

/// Postgres double-quote escaping for SQL identifiers. Doubles any
/// embedded `"` so a hostile name doesn't break out of the quoted
/// form. Used for table and policy names; the `using` / `with_check`
/// clauses are passed through verbatim (they're SQL expressions the
/// user wrote and is responsible for).
fn escape_ident(s: &str) -> String {
    s.replace('"', "\"\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_on_appends_table_in_order() {
        let p = RlsPlugin::new().enable_on("post").enable_on("comment");
        assert_eq!(p.tables(), &["post", "comment"]);
    }

    #[test]
    fn policy_auto_enables_table() {
        let p = RlsPlugin::new().policy("post", "read_own", Action::Select, "user_id = 1");
        assert_eq!(p.tables(), &["post"]);
        assert_eq!(p.policies().len(), 1);
        assert_eq!(p.policies()[0].name, "read_own");
    }

    // audit_2 C2/R1: ENABLE alone leaves the table OWNER exempt (the
    // single-DATABASE_URL app connects as owner), so RLS enforces nothing.
    // FORCE is the fix — it must be emitted alongside ENABLE.
    #[test]
    fn force_row_level_security_is_rendered_and_escaped() {
        let p = RlsPlugin::new().enable_on("post");
        assert_eq!(
            p.render_enable_sql("post"),
            "ALTER TABLE \"post\" ENABLE ROW LEVEL SECURITY"
        );
        assert_eq!(
            p.render_force_sql("post"),
            "ALTER TABLE \"post\" FORCE ROW LEVEL SECURITY"
        );
        // A hostile identifier is double-quote-escaped, not interpolated raw.
        assert_eq!(
            p.render_force_sql("a\"b"),
            "ALTER TABLE \"a\"\"b\" FORCE ROW LEVEL SECURITY"
        );
    }

    #[test]
    fn policy_does_not_duplicate_enable() {
        let p = RlsPlugin::new().enable_on("post").policy(
            "post",
            "read_own",
            Action::Select,
            "user_id = 1",
        );
        assert_eq!(p.tables().len(), 1, "post should not be duplicated");
    }

    #[test]
    fn render_enable_sql_quotes_identifier() {
        let p = RlsPlugin::new();
        let sql = p.render_enable_sql("post");
        assert_eq!(sql, "ALTER TABLE \"post\" ENABLE ROW LEVEL SECURITY");
    }

    #[test]
    fn render_policy_sql_emits_drop_then_create() {
        let plugin = RlsPlugin::new();
        let policy = Policy {
            table: "post".to_string(),
            name: "read_own".to_string(),
            action: Action::Select,
            using: "user_id = current_setting('app.user_id')::int".to_string(),
            with_check: None,
        };
        let (drop_sql, create_sql) = plugin.render_policy_sql(&policy);
        assert_eq!(drop_sql, "DROP POLICY IF EXISTS \"read_own\" ON \"post\"");
        assert!(
            create_sql.starts_with("CREATE POLICY \"read_own\" ON \"post\" FOR SELECT USING (")
        );
        assert!(create_sql.contains("user_id = current_setting('app.user_id')::int"));
        assert!(!create_sql.contains("WITH CHECK"));
    }

    #[test]
    fn render_policy_with_check_emits_with_check_clause() {
        let plugin = RlsPlugin::new().policy_with_check(
            "post",
            "owner_only_insert",
            Action::Insert,
            "user_id = current_setting('app.user_id')::int",
            "user_id = current_setting('app.user_id')::int AND status <> 'banned'",
        );
        let (_, create_sql) = plugin.render_policy_sql(&plugin.policies()[0]);
        assert!(create_sql.contains("FOR INSERT"));
        assert!(create_sql.contains("WITH CHECK"));
        assert!(create_sql.contains("status <> 'banned'"));
    }

    #[test]
    fn action_sql_keywords_round_trip() {
        assert_eq!(Action::Select.sql_keyword(), "SELECT");
        assert_eq!(Action::Insert.sql_keyword(), "INSERT");
        assert_eq!(Action::Update.sql_keyword(), "UPDATE");
        assert_eq!(Action::Delete.sql_keyword(), "DELETE");
        assert_eq!(Action::All.sql_keyword(), "ALL");
    }

    #[test]
    fn escape_ident_doubles_inner_quotes() {
        assert_eq!(escape_ident("plain"), "plain");
        assert_eq!(escape_ident("with\"quote"), "with\"\"quote");
    }
}
