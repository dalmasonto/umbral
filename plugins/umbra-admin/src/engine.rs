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
        // on this: dev loads the CDN, prod expects /admin/static/admin.css.
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
        .render(ctx)
        .map_err(|e| AdminError::Render(e.to_string()))?;
    Ok(Html(body))
}
