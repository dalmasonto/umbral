//! features.md #4 — `#[umbra(widget = "...")]` on a model field sets
//! `FieldSpec::widget`, the presentation hint the admin form (and any
//! plugin form) reads to pick a richer editor. It's metadata only:
//! the column's `SqlType` and DDL are unchanged.

use umbra::orm::{Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, umbra::orm::Model)]
#[umbra(table = "umbra_widget_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    #[umbra(
        widget = "markdown",
        help = "Markdown supported — headings, lists, code."
    )]
    pub body: String,
    pub plain: String,
}

#[test]
fn widget_attr_flows_into_field_spec() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <Doc as Model>::FIELDS.iter().map(|f| (f.name, f)).collect();

    let body = by_name.get("body").expect("body field");
    assert_eq!(body.widget, Some("markdown"));
    assert_eq!(body.help, "Markdown supported — headings, lists, code.");
    // The widget hint does not change the column type — still plain TEXT.
    assert_eq!(body.ty, SqlType::Text);

    let plain = by_name.get("plain").expect("plain field");
    assert_eq!(plain.widget, None, "no attr means no widget");
}
