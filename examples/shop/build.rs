//! Cargo build script for the shop example.
//!
//! Compiles `styles/input.css` → `static/css/shop.css` via Tailwind so
//! the runtime doesn't ship the CDN bundle (Lighthouse-flagged
//! render-blocking ~310ms + ~430ms for Google Fonts; see bugs/gaps2.md
//! #20). If node_modules isn't installed the build silently skips and
//! the served page falls back to plain text + the system stylesheet —
//! the `wrapper.html` `<link>` still loads, just resolves to the empty
//! committed CSS until the build runs. This matches umbral-admin's
//! build.rs posture: prod CSS is opt-in via `npm install`, dev is
//! lenient.
//!
//! Manual build (one-time):
//!   cd examples/shop/styles && npm install && npm run build

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=styles/input.css");
    println!("cargo:rerun-if-changed=styles/tailwind.config.js");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let css_dir = PathBuf::from(&manifest_dir).join("styles");
    let out_css = PathBuf::from(&manifest_dir).join("static/css/shop.css");

    if !css_dir.join("node_modules").exists() {
        println!(
            "cargo:warning=shop: Tailwind CSS not built (styles/node_modules missing). \
             Run `cd examples/shop/styles && npm install && npm run build`. \
             Until then the served page will reference an empty shop.css."
        );
        return;
    }

    let status = Command::new("npx")
        .arg("tailwindcss")
        .arg("-c")
        .arg("tailwind.config.js")
        .arg("-i")
        .arg("input.css")
        .arg("-o")
        .arg(&out_css)
        .arg("--minify")
        .current_dir(&css_dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=shop: Tailwind CSS built successfully.");
        }
        Ok(s) => {
            println!(
                "cargo:warning=shop: Tailwind CSS build exited with status {s}. \
                 Using whatever's already in static/css/shop.css."
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=shop: Tailwind CSS build failed ({e}). \
                 Using whatever's already in static/css/shop.css."
            );
        }
    }
}
