//! Minijinja environment + the `render` helper every handler uses.
//!
//! The environment is built once on first call (`OnceLock`) and includes
//! every admin template via `include_str!` so the binary ships
//! self-contained — no on-disk templates needed at runtime.
//!
//! `environment` (dev/test/prod) is exposed as a template global so
//! wrapper.html can gate the Tailwind CDN vs the precompiled admin.css.

use minijinja::Environment;
use umbra::web::Html;

use crate::AdminError;

static ENGINE: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();

pub(crate) fn engine() -> &'static Environment<'static> {
    ENGINE.get_or_init(|| {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        // Percent-encode for safe embedding in query strings — used by
        // the active-filter chip links and pagination URLs in
        // data_table.html. Reuses the existing `urlencoding_simple`
        // helper so we don't pull a new crate just to expose a filter.
        env.add_filter("urlencode", |s: String| -> String {
            crate::util::urlencoding_simple(&s)
        });
        // Serialise a value to JSON for safe embedding in an inline
        // <script> block. Minijinja stable doesn't ship `tojson` by
        // default; we route through serde_json so every kind a
        // template might pass (string, array, object, number) lands as
        // a valid JS literal. Falls back to `null` if serialisation
        // refuses the input.
        //
        // Returns a `from_safe_string` Value so the auto-escape
        // callback skips it — otherwise `{"foo":"bar"}` would land in
        // a <script> block as `{&quot;foo&quot;:&quot;bar&quot;}` and
        // the JS parser would die on the first `&`. That cascade
        // killed every JS function defined later in the same block,
        // which is why the FK search input's oninput handler couldn't
        // find `umbra._filterFkSearch`.
        env.add_filter("tojson", |v: minijinja::Value| -> minijinja::Value {
            let json = serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string());
            // WEB-3: this output is dropped into inline <script> blocks via
            // `from_safe_string` (autoescape skipped). serde_json leaves
            // `<`, `>`, `&` raw, so a filter value containing `</script>`
            // would break out of the script element and run attacker JS in
            // the admin origin. Escape those to their `\uXXXX` JSON forms
            // (still valid JSON, parses to the same value) plus the U+2028/
            // U+2029 line separators that terminate JS strings. Mirrors
            // Django's `json_script`.
            let safe = json
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
                .replace('&', "\\u0026")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029");
            minijinja::Value::from_safe_string(safe)
        });
        // Django-style date/datetime humanizers. Templates render
        // raw RFC3339 / SQL-shaped timestamps through one of these
        // instead of dumping the unreadable `2026-06-08T21:23:20.619...`
        // straight at the user:
        //
        //   {{ item.at | humanize_date }}  → "Jun 8, 2026 at 9:23 PM"
        //   {{ item.at | naturaltime }}    → "2 hours ago"
        //
        // Both accept anything `chrono::DateTime::parse_from_rfc3339`
        // or `NaiveDateTime::parse_from_str` can handle (the SQLite
        // `datetime('now')` shape `2026-06-08 21:23:20` included).
        // On parse failure the original value is returned untouched —
        // a "couldn't humanize" template should still render legible
        // text rather than throw.
        env.add_filter("humanize_date", |s: String| -> String {
            crate::util::humanize_date(&s)
        });
        env.add_filter("naturaltime", |s: String| -> String {
            crate::util::naturaltime(&s)
        });
        env.add_template(
            "admin/wrapper.html",
            include_str!("../templates/wrapper.html"),
        )
        .expect("admin/wrapper.html parses");
        env.add_template("admin/base.html", include_str!("../templates/base.html"))
            .expect("admin/base.html parses");
        env.add_template("admin/login.html", include_str!("../templates/login.html"))
            .expect("admin/login.html parses");
        env.add_template("admin/index.html", include_str!("../templates/index.html"))
            .expect("admin/index.html parses");
        env.add_template("admin/list.html", include_str!("../templates/list.html"))
            .expect("admin/list.html parses");
        env.add_template(
            "admin/detail.html",
            include_str!("../templates/detail.html"),
        )
        .expect("admin/detail.html parses");
        env.add_template("admin/form.html", include_str!("../templates/form.html"))
            .expect("admin/form.html parses");
        env.add_template(
            "admin/changelist.html",
            include_str!("../templates/changelist.html"),
        )
        .expect("admin/changelist.html parses");
        env.add_template(
            "admin/sheet_preview.html",
            include_str!("../templates/sheet_preview.html"),
        )
        .expect("admin/sheet_preview.html parses");
        env.add_template(
            "admin/sheet_edit.html",
            include_str!("../templates/sheet_edit.html"),
        )
        .expect("admin/sheet_edit.html parses");
        env.add_template(
            "admin/sheet_create.html",
            include_str!("../templates/sheet_create.html"),
        )
        .expect("admin/sheet_create.html parses");
        env.add_template(
            "admin/confirm_delete.html",
            include_str!("../templates/confirm_delete.html"),
        )
        .expect("admin/confirm_delete.html parses");
        env.add_template(
            "admin/rows_fragment.html",
            include_str!("../templates/rows_fragment.html"),
        )
        .expect("admin/rows_fragment.html parses");
        env.add_template(
            "admin/_macros/data_table.html",
            include_str!("../templates/_macros/data_table.html"),
        )
        .expect("admin/_macros/data_table.html parses");
        env.add_template(
            "admin/_macros/sheet.html",
            include_str!("../templates/_macros/sheet.html"),
        )
        .expect("admin/_macros/sheet.html parses");
        env.add_template(
            "admin/_macros/field_editor.html",
            include_str!("../templates/_macros/field_editor.html"),
        )
        .expect("admin/_macros/field_editor.html parses");
        env.add_template(
            "admin/_macros/inlines.html",
            include_str!("../templates/_macros/inlines.html"),
        )
        .expect("admin/_macros/inlines.html parses");
        env.add_template(
            "admin/_macros/confirm_dialog.html",
            include_str!("../templates/_macros/confirm_dialog.html"),
        )
        .expect("admin/_macros/confirm_dialog.html parses");
        env.add_template(
            "admin/_macros/filter_dialog.html",
            include_str!("../templates/_macros/filter_dialog.html"),
        )
        .expect("admin/_macros/filter_dialog.html parses");
        env.add_template(
            "admin/filter_dialog_fragment.html",
            include_str!("../templates/filter_dialog_fragment.html"),
        )
        .expect("admin/filter_dialog_fragment.html parses");

        // Phase 4 templates
        env.add_template(
            "admin/_macros/audit_timeline.html",
            include_str!("../templates/_macros/audit_timeline.html"),
        )
        .expect("admin/_macros/audit_timeline.html parses");
        env.add_template(
            "admin/history.html",
            include_str!("../templates/history.html"),
        )
        .expect("admin/history.html parses");
        env.add_template(
            "admin/dashboard.html",
            include_str!("../templates/dashboard.html"),
        )
        .expect("admin/dashboard.html parses");
        env.add_template(
            "admin/widget_data.html",
            include_str!("../templates/widget_data.html"),
        )
        .expect("admin/widget_data.html parses");
        env.add_template(
            "admin/palette.html",
            include_str!("../templates/palette.html"),
        )
        .expect("admin/palette.html parses");
        // Widget macros
        env.add_template(
            "admin/_macros/widgets/kpi.html",
            include_str!("../templates/_macros/widgets/kpi.html"),
        )
        .expect("admin/_macros/widgets/kpi.html parses");
        env.add_template(
            "admin/_macros/widgets/bar.html",
            include_str!("../templates/_macros/widgets/bar.html"),
        )
        .expect("admin/_macros/widgets/bar.html parses");
        env.add_template(
            "admin/_macros/widgets/card.html",
            include_str!("../templates/_macros/widgets/card.html"),
        )
        .expect("admin/_macros/widgets/card.html parses");
        env.add_template(
            "admin/_macros/widgets/donut.html",
            include_str!("../templates/_macros/widgets/donut.html"),
        )
        .expect("admin/_macros/widgets/donut.html parses");
        env.add_template(
            "admin/_macros/widgets/line.html",
            include_str!("../templates/_macros/widgets/line.html"),
        )
        .expect("admin/_macros/widgets/line.html parses");
        env.add_template(
            "admin/_macros/widgets/feed.html",
            include_str!("../templates/_macros/widgets/feed.html"),
        )
        .expect("admin/_macros/widgets/feed.html parses");
        env.add_template(
            "admin/_macros/widgets/table.html",
            include_str!("../templates/_macros/widgets/table.html"),
        )
        .expect("admin/_macros/widgets/table.html parses");
        env.add_template(
            "admin/_macros/widgets/radial.html",
            include_str!("../templates/_macros/widgets/radial.html"),
        )
        .expect("admin/_macros/widgets/radial.html parses");
        env.add_template(
            "admin/_macros/widgets/heatmap.html",
            include_str!("../templates/_macros/widgets/heatmap.html"),
        )
        .expect("admin/_macros/widgets/heatmap.html parses");
        env.add_template(
            "admin/_macros/widgets/progress.html",
            include_str!("../templates/_macros/widgets/progress.html"),
        )
        .expect("admin/_macros/widgets/progress.html parses");
        // Preview macros
        env.add_template(
            "admin/_macros/previews/image.html",
            include_str!("../templates/_macros/previews/image.html"),
        )
        .expect("admin/_macros/previews/image.html parses");
        env.add_template(
            "admin/_macros/previews/pdf.html",
            include_str!("../templates/_macros/previews/pdf.html"),
        )
        .expect("admin/_macros/previews/pdf.html parses");
        env.add_template(
            "admin/_macros/previews/video_audio.html",
            include_str!("../templates/_macros/previews/video_audio.html"),
        )
        .expect("admin/_macros/previews/video_audio.html parses");
        env.add_template(
            "admin/_macros/previews/code_text.html",
            include_str!("../templates/_macros/previews/code_text.html"),
        )
        .expect("admin/_macros/previews/code_text.html parses");
        env.add_template(
            "admin/_macros/previews/download.html",
            include_str!("../templates/_macros/previews/download.html"),
        )
        .expect("admin/_macros/previews/download.html parses");

        // Expose the runtime environment ("dev" / "test" / "prod") as a
        // template global. wrapper.html gates the Tailwind CDN script
        // on this: dev loads the CDN, prod expects /static/admin/admin.css.
        // Read from `umbra::settings::get_opt()` (Optional) so the engine
        // builds even before `App::build` ran — tests bypass App::build
        // and would otherwise panic in `settings::get()`.
        let env_name: &'static str = umbra::settings::get_opt()
            .map(|s| match s.environment {
                umbra::Environment::Dev => "dev",
                umbra::Environment::Test => "test",
                umbra::Environment::Prod => "prod",
            })
            .unwrap_or("dev");
        env.add_global("environment", minijinja::Value::from(env_name));

        // Developer-supplied branding. Plugin::routes() seals
        // AdminPlugin::site_title / site_description / brand_color
        // into a static cell before the first render; we read it
        // here and inject the values as template globals so every
        // template can reference `site_title` / `brand_color` etc.
        // without the handler having to thread them through context.
        let branding = crate::branding::current();
        env.add_global("site_title", minijinja::Value::from(branding.site_title));
        env.add_global(
            "site_description",
            minijinja::Value::from(branding.site_description),
        );
        env.add_global("brand_color", minijinja::Value::from(branding.brand_color));
        // Gap 107: the admin base path. Templates reference this
        // via `{{ admin_base }}` so cross-page links and HTMX
        // targets resolve under whatever prefix the developer
        // configured. Defaults to `/admin`. Registered as a safe
        // string so inline-script contexts (e.g.
        // `htmx.ajax('GET', '{{ admin_base }}/api/...')`) don't
        // HTML-entity-escape the leading slash. The value is a
        // URL path under the framework's control, never user input.
        env.add_global(
            "admin_base",
            minijinja::Value::from_safe_string(branding.base_path),
        );
        // gaps2 #33: expose the flag so the "Home" breadcrumb link in
        // base.html can append `?dashboard=1` when the feature is on.
        env.add_global(
            "restore_last_path",
            minijinja::Value::from(branding.restore_last_path),
        );

        // Unified static pipeline — register the `static()` global so
        // admin templates resolve assets through the same `static_url`
        // the core engine uses: `{{ static("admin/admin.css") }}` →
        // `<static_url>admin/admin.css` (default `/static/admin/admin.css`).
        // The admin engine builds its own minijinja `Environment`, so the
        // core engine's `static()` function isn't inherited; we register an
        // equivalent here, routing through `umbra::templates::resolve_static_url`
        // so the resolution logic lives in one place.
        env.add_function("static", |path: String| -> String {
            umbra::templates::resolve_static_url(&path)
        });

        // Media-key resolver — mirrors `static()` but routes a stored
        // file/image KEY through the ambient Storage backend's `url()`
        // so changelist + preview templates render the public URL
        // (`/media/<key>`) instead of the opaque key. An empty key
        // yields an empty string (the template skips the markup); with
        // no Storage backend wired the raw key falls through unchanged.
        env.add_function("media_url", |key: String| -> String {
            if key.is_empty() {
                return String::new();
            }
            umbra::storage::storage_opt()
                .map(|s| s.url(&key))
                .unwrap_or(key)
        });

        env
    })
}

/// Render a named template against the supplied context. Errors are
/// wrapped in `AdminError::Render` so handlers can early-return with
/// `?` like any other database/IO call.
pub(crate) fn render(name: &str, ctx: minijinja::Value) -> Result<Html<String>, AdminError> {
    let tmpl = engine()
        .get_template(name)
        .map_err(|e| AdminError::Render(e.to_string()))?;
    let body = tmpl
        .render(umbra::templates::merge_ambient_value(ctx))
        .map_err(|e| AdminError::Render(e.to_string()))?;
    Ok(Html(body))
}

#[cfg(test)]
mod tojson_xss_tests {
    /// WEB-3 regression: `tojson` output is interpolated into inline
    /// `<script>` blocks (via `from_safe_string`, so autoescape is off).
    /// A filter value containing `</script>` must be escaped to its
    /// `\uXXXX` JSON form so it can't terminate the script element and
    /// inject attacker JS into the admin origin.
    #[test]
    fn tojson_escapes_script_breakout_sequences() {
        let env = super::engine();
        let out = env
            .render_str(
                "{{ v | tojson }}",
                minijinja::context! { v => "</script><script>alert(1)</script>" },
            )
            .expect("render tojson");
        assert!(
            !out.contains("</script>"),
            "a raw </script> must not survive tojson: {out}"
        );
        assert!(
            !out.contains("<script>"),
            "a raw <script> must not survive tojson: {out}"
        );
        assert!(
            out.contains("\\u003c"),
            "`<` should be escaped to \\u003c: {out}"
        );
        // It must still be valid JSON that round-trips to the same string.
        let parsed: String = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(parsed, "</script><script>alert(1)</script>");
    }
}
