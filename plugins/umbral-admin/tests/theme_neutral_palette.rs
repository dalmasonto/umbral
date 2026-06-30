//! Pin the neutral palette so the warm "brownish" tint can't creep back.
//! We assert against the raw wrapper.html source (the token block is static
//! CSS, not template-rendered) for the load-bearing values.

const WRAPPER: &str = include_str!("../templates/wrapper.html");

#[test]
fn light_background_is_pure_white() {
    // :root (light) --background must be pure white, zero chroma.
    assert!(
        WRAPPER.contains("--background:               oklch(1 0 0);"),
        "light --background must be pure white oklch(1 0 0)"
    );
}

#[test]
fn dark_background_is_near_black_neutral() {
    // .dark --background near-black neutral, NOT true black, zero chroma.
    assert!(
        WRAPPER.contains("--background:               oklch(0.16 0 0);"),
        "dark --background must be near-black neutral oklch(0.16 0 0)"
    );
}

#[test]
fn status_tokens_exist() {
    for tok in [
        "--success:",
        "--warning:",
        "--success-container:",
        "--warning-container:",
    ] {
        assert!(
            WRAPPER.matches(tok).count() >= 2,
            "{tok} must be defined in both :root and .dark"
        );
    }
}

#[test]
fn no_warm_hue_left_on_core_surfaces() {
    // The old palette used hue ~75/90/95 with chroma on surfaces. After the
    // refresh, the surface/background/on-surface neutrals are zero-chroma
    // (`0 0`). Guard the specific lines that were brown before.
    assert!(
        !WRAPPER.contains("oklch(0.16 0.009 75)"),
        "old warm dark --surface must be gone"
    );
    assert!(
        !WRAPPER.contains("oklch(0.985 0.005 95)"),
        "old warm light --surface must be gone"
    );
}
