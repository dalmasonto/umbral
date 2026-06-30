//! The Tailwind theme is defined once in css/theme.json and consumed by
//! both the compiled build (tailwind.config.js) and the dev CDN config
//! (wrapper.html via the `admin_theme_json` global). These tests pin that
//! single-source contract so the dev/prod drift that made sidebar labels
//! and table headers render at the default 16px can't silently return.

#[test]
fn theme_json_is_valid_and_defines_label_sm_fontsize() {
    let raw = include_str!("../css/theme.json");
    let v: serde_json::Value = serde_json::from_str(raw).expect("theme.json is valid JSON");
    // The class that broke in prod: text-label-sm must resolve to 11px.
    let label_sm = &v["fontSize"]["label-sm"];
    assert!(
        label_sm.is_array(),
        "fontSize.label-sm must be defined (array form): {label_sm}"
    );
    assert_eq!(
        label_sm[0].as_str(),
        Some("11px"),
        "label-sm must be 11px so sidebar labels + table headers stay slender"
    );
    // fontFamily and borderRadius must also be present (prod was missing them).
    assert!(
        v["fontFamily"]["label-sm"].is_array(),
        "fontFamily.label-sm present"
    );
    assert!(
        v["borderRadius"]["xl"].is_string(),
        "borderRadius.xl present"
    );
    assert!(v["colors"]["primary"].is_string(), "colors.primary present");
}

#[test]
fn tailwind_config_requires_theme_json() {
    let cfg = include_str!("../css/tailwind.config.js");
    assert!(
        cfg.contains("require('./theme.json')") || cfg.contains("require(\"./theme.json\")"),
        "tailwind.config.js must read the shared theme.json, not redefine the theme inline"
    );
}
