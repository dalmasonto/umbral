//! Pin the neutral palette so the old WARM ("brownish") tint can't creep back.
//! The intentional tint since the "umbra dusk" refresh (gaps4 #61) is a FAINT
//! COOL slate — hue 264, chroma 0.008 — not zero-chroma and definitely not the
//! old warm hue ~75/90. We assert against the raw wrapper.html source (the token
//! block is static CSS, not template-rendered) for the load-bearing values.

const WRAPPER: &str = include_str!("../templates/wrapper.html");

#[test]
fn light_background_is_near_white_cool_slate() {
    // :root (light) --background: near-white with a faint COOL slate tint
    // (hue 264), never warm.
    assert!(
        WRAPPER.contains("--background:               oklch(1 0.008 264);"),
        "light --background must be near-white cool slate oklch(1 0.008 264)"
    );
}

#[test]
fn dark_background_is_near_black_cool_slate() {
    // .dark --background: near-black (NOT true black) with the same faint cool
    // slate tint.
    assert!(
        WRAPPER.contains("--background:               oklch(0.16 0.008 264);"),
        "dark --background must be near-black cool slate oklch(0.16 0.008 264)"
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
