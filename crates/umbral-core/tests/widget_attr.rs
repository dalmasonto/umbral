//! features.md #4 — `#[umbral(widget = "...")]` on a model field sets
//! `FieldSpec::widget`, the presentation hint the admin form (and any
//! plugin form) reads to pick a richer editor. It's metadata only:
//! the column's `SqlType` and DDL are unchanged.

use umbral::orm::{Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_widget_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    #[umbral(
        widget = "markdown",
        help = "Markdown supported — headings, lists, code."
    )]
    pub body: String,
    pub plain: String,
    // Exactly the shape ShowcaseEntry.long_content uses: an
    // Option<String> with widget in its own #[umbral(...)] attribute.
    #[umbral(widget = "markdown")]
    pub long_content: Option<String>,
}

#[test]
fn widget_attr_flows_into_field_spec() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> =
        <Doc as Model>::FIELDS.iter().map(|f| (f.name, f)).collect();

    let body = by_name.get("body").expect("body field");
    assert_eq!(body.widget, Some("markdown"));
    assert_eq!(body.help, "Markdown supported — headings, lists, code.");
    // The widget hint does not change the column type — still plain TEXT.
    assert_eq!(body.ty, SqlType::Text);

    let plain = by_name.get("plain").expect("plain field");
    assert_eq!(plain.widget, None, "no attr means no widget");

    // The Option<String> + standalone-#[umbral(widget)] case (the exact
    // ShowcaseEntry.long_content shape) must carry widget too.
    let lc = by_name.get("long_content").expect("long_content field");
    assert_eq!(lc.widget, Some("markdown"), "widget lost on Option<String>");
    assert!(lc.nullable, "Option<String> is nullable");

    // And it must survive the FieldSpec -> migrate::Column conversion
    // that ModelMeta::for_ uses (what the admin actually reads).
    let cols: Vec<umbral_core::migrate::Column> =
        <Doc as Model>::FIELDS.iter().map(Into::into).collect();
    let lc_col = cols.iter().find(|c| c.name == "long_content").unwrap();
    assert_eq!(
        lc_col.widget.as_deref(),
        Some("markdown"),
        "widget dropped in Column::from(FieldSpec)"
    );
}
