//! Custom admin views — developer-registered widget pages mounted at
//! arbitrary paths under the admin base (e.g. `/admin/reports/sales/`).
//! A view renders the existing dashboard widget kinds inside the admin
//! chrome. See `docs/superpowers/specs/2026-07-01-admin-custom-views-design.md`.
//!
//! # Naming convention
//!
//! Rust does not permit two methods with the same name on the same type
//! even if one takes `self` (consuming) and the other `&self` (borrowing).
//! To avoid the `E0592` duplicate-definition error, setter/builder methods
//! use a `with_` prefix (`with_subtitle`, `with_icon`, …) while the
//! read-only accessors use the bare field name (`subtitle()`, `icon()`, …).
//! The one-shot "mark as hidden" setter is named `hide()` so `hidden()`
//! can remain the boolean accessor.

use crate::widgets::WidgetSection;

/// A registered admin page that is not tied to a model. Renders one or
/// more [`WidgetSection`]s (the same cards/charts the dashboard uses)
/// inside the admin chrome, mounted at `{admin_base}/{path}`.
#[derive(Debug, Clone)]
pub struct AdminView {
    path: String,
    title: String,
    subtitle: Option<String>,
    icon: Option<String>,
    group: Option<String>,
    permission: Option<String>,
    hidden: bool,
    sections: Vec<WidgetSection>,
}

/// Normalize a developer-supplied path to the canonical `a/b/c` form
/// (no leading/trailing slashes, no empty segments).
fn normalize_path(raw: &str) -> String {
    raw.split('/')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

impl AdminView {
    /// Start a view. `path` is the subpath under the admin base
    /// (`"reports/sales"` → `/admin/reports/sales/`); `title` is the page
    /// heading and the default sidebar label.
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            path: normalize_path(&path.into()),
            title: title.into(),
            subtitle: None,
            icon: None,
            group: None,
            permission: None,
            hidden: false,
            sections: Vec::new(),
        }
    }

    /// Optional caption under the page heading.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Lucide icon name for the sidebar entry.
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Sidebar group heading. Defaults to "Pages" when unset.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Permission codename gate (e.g. `"reports.view_sales"`). Unset = any staff.
    pub fn with_permission(mut self, codename: impl Into<String>) -> Self {
        self.permission = Some(codename.into());
        self
    }

    /// Keep the view routable but hide it from the sidebar.
    pub fn hide(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Append one widget section.
    pub fn section(mut self, section: WidgetSection) -> Self {
        self.sections.push(section);
        self
    }

    /// Append many widget sections.
    pub fn add_sections(mut self, sections: impl IntoIterator<Item = WidgetSection>) -> Self {
        self.sections.extend(sections);
        self
    }

    // --- accessors used by the crate (route mount, handler, sidebar) ---

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Stable key for the per-route handler + sidebar active-state. Equals the normalized path.
    pub(crate) fn slug(&self) -> &str {
        &self.path
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }

    pub(crate) fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    pub(crate) fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    pub(crate) fn permission(&self) -> Option<&str> {
        self.permission.as_deref()
    }

    pub(crate) fn hidden(&self) -> bool {
        self.hidden
    }

    pub(crate) fn sections(&self) -> &[WidgetSection] {
        &self.sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::WidgetSection;

    #[test]
    fn normalizes_path_and_slug() {
        let v = AdminView::new("/reports/sales/", "Sales");
        assert_eq!(
            v.path(),
            "reports/sales",
            "leading/trailing slashes stripped"
        );
        assert_eq!(
            v.slug(),
            "reports/sales",
            "slug mirrors the normalized path"
        );
        assert_eq!(v.title(), "Sales");

        let v2 = AdminView::new("reports//sales", "X");
        assert_eq!(v2.path(), "reports/sales", "double slashes collapsed");
    }

    #[test]
    fn defaults_are_sane() {
        let v = AdminView::new("tools/x", "X");
        assert!(v.subtitle().is_none());
        assert!(v.icon().is_none());
        assert!(
            v.group().is_none(),
            "group defaults to None (renders under 'Pages')"
        );
        assert!(v.permission().is_none(), "no permission = any staff");
        assert!(!v.hidden(), "shown in sidebar by default");
        assert!(v.sections().is_empty());
    }

    #[test]
    fn builders_populate_fields() {
        // Note: builder methods use `with_` prefix (e.g. `with_subtitle`) and `hide()` to
        // avoid Rust E0592 duplicate-definition conflicts with the same-named `&self` accessors.
        let v = AdminView::new("reports/sales", "Sales")
            .with_subtitle("Revenue")
            .with_icon("bar-chart")
            .with_group("Reports")
            .with_permission("reports.view_sales")
            .hide()
            .section(WidgetSection::new("This month"));
        assert_eq!(v.subtitle(), Some("Revenue"));
        assert_eq!(v.icon(), Some("bar-chart"));
        assert_eq!(v.group(), Some("Reports"));
        assert_eq!(v.permission(), Some("reports.view_sales"));
        assert!(v.hidden());
        assert_eq!(v.sections().len(), 1);
    }

    #[test]
    fn add_sections_appends() {
        let v2 = AdminView::new("x", "X")
            .add_sections(vec![WidgetSection::new("A"), WidgetSection::new("B")]);
        assert_eq!(v2.sections().len(), 2, "add_sections appends all");
    }
}
