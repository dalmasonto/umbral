//! Compile the Umbral website Tailwind bundle when local node tooling exists.
//!
//! The explicit path remains:
//!   cd umbral_website/styles && npm install && npm run build
//!
//! Cargo builds stay lenient so Rust checks do not fail on machines that
//! have not installed the frontend toolchain yet.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=templates");
    // Watch ALL plugin sources, not just public's templates: the Tailwind
    // config scans `plugins/**/templates` and `plugins/**/src/**/*.rs`, so
    // a new class in any plugin template (e.g. the community page) must
    // re-trigger the CSS build — otherwise its utilities never generate.
    println!("cargo:rerun-if-changed=plugins");
    println!("cargo:rerun-if-changed=styles/input.css");
    println!("cargo:rerun-if-changed=styles/tailwind.config.js");
    println!("cargo:rerun-if-changed=styles/package-lock.json");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let styles_dir = PathBuf::from(&manifest_dir).join("styles");
    let out_css = PathBuf::from(&manifest_dir).join("static/css/umbral.css");

    if !styles_dir.join("node_modules").exists() {
        println!(
            "cargo:warning=umbral_website: Tailwind CSS not built \
             (styles/node_modules missing). Run `cd umbral_website/styles && npm install && npm run build`."
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
        .current_dir(&styles_dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=umbral_website: Tailwind CSS built successfully.");
        }
        Ok(s) => {
            println!(
                "cargo:warning=umbral_website: Tailwind CSS exited with status {s}. \
                 Using the existing static/css/umbral.css."
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=umbral_website: Tailwind CSS failed ({e}). \
                 Using the existing static/css/umbral.css."
            );
        }
    }
}
