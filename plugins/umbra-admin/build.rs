//! Cargo build script for umbra-admin.
//!
//! Attempts to run the Tailwind CSS build to produce
//! `src/assets/admin.css` from `css/input.css`. If Node or npx is not
//! installed the build is silently skipped and the fallback dev-CDN
//! path is used at runtime.
//!
//! Manual build: `cd plugins/umbra-admin/css && npm install && npm run build`

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Re-run if any template or input CSS changes.
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=css/input.css");
    println!("cargo:rerun-if-changed=css/tailwind.config.js");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let css_dir = PathBuf::from(&manifest_dir).join("css");
    let out_css = PathBuf::from(&manifest_dir).join("src/assets/admin.css");

    // Skip if node_modules doesn't exist (user hasn't run npm install).
    if !css_dir.join("node_modules").exists() {
        println!(
            "cargo:warning=umbra-admin: Tailwind CSS not built (node_modules missing). \
             Run `cd plugins/umbra-admin/css && npm install && npm run build` for production CSS. \
             Development uses the Tailwind CDN."
        );
        return;
    }

    // Try to run the build.
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
            println!("cargo:warning=umbra-admin: Tailwind CSS built successfully.");
        }
        Ok(s) => {
            println!(
                "cargo:warning=umbra-admin: Tailwind CSS build exited with status {s}. \
                 Using CDN fallback in dev."
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=umbra-admin: Tailwind CSS build failed ({e}). \
                 Using CDN fallback in dev."
            );
        }
    }
}
