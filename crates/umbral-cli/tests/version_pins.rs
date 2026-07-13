//! Version pins in prose must not silently rot.
//!
//! Two rules, enforced here because nothing else can enforce them:
//!
//! 1. **READMEs carry no version at all.** They say `cargo add umbral-rest`, which resolves the
//!    current release at install time. They used to pin `umbral = "0.0.1"` — and because cargo
//!    treats every `0.0.z` as its own incompatible range, that pin did not mean "0.0.1 or newer",
//!    it meant *exactly 0.0.1*. Every reader who copy-pasted an install snippet landed seven
//!    releases in the past and stayed there.
//!
//! 2. **A `.mdx` page that shows a literal manifest must pin the CURRENT version.** A `Cargo.toml`
//!    needs real version strings, so `cargo add` cannot save those pages; the only thing that can
//!    is a test that fails the moment they drift from the crate version.
//!
//! Skips silently when the documentation tree is absent — that is the crate being built from a
//! crates.io tarball rather than a source checkout, where there is nothing to check.

use std::fs;
use std::path::{Path, PathBuf};

/// Walk up to the repo root, identified the same way `scaffold.rs` identifies it.
fn repo_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|d| d.join("crates/umbral-core/Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

fn files_with_extension(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            files_with_extension(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// `umbral-rest = "0.0.8"` -> `Some(("umbral-rest", "0.0.8"))`. Ignores `umbral = "..."`
/// (a deliberate placeholder) and inline tables.
fn version_pin(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let crate_name = line.strip_prefix("umbral")?;
    let (name, rest) = line.split_once(" = ")?;
    if !crate_name
        .split(" = ")
        .next()?
        .chars()
        .all(|c| c.is_ascii_lowercase() || c == '-')
    {
        return None;
    }
    let pin = rest.trim().trim_matches('"');
    let is_semver = !pin.is_empty()
        && pin
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic());
    (is_semver && pin.chars().next()?.is_ascii_digit()).then_some((name, pin))
}

#[test]
fn readmes_use_cargo_add_and_never_pin_a_version() {
    let Some(root) = repo_root() else { return };

    let mut readmes = vec![root.join("README.md")];
    for dir in ["crates", "plugins"] {
        files_with_extension(&root.join(dir), "md", &mut readmes);
    }

    let mut offenders = Vec::new();
    for path in readmes.iter().filter(|p| p.ends_with("README.md")) {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if let Some((name, pin)) = version_pin(line) {
                offenders.push(format!(
                    "{}:{}: `{name} = \"{pin}\"` — use `cargo add {name}` instead",
                    path.strip_prefix(&root).unwrap_or(path).display(),
                    i + 1,
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "READMEs must not pin an umbral version — a 0.0.z pin resolves to that exact \
         release forever, so the snippet rots the moment we ship again:\n  {}",
        offenders.join("\n  "),
    );
}

#[test]
fn docs_manifests_pin_the_current_version() {
    let Some(root) = repo_root() else { return };
    let docs = root.join("documentation/docs");
    if !docs.is_dir() {
        return;
    }

    let current = env!("CARGO_PKG_VERSION");
    let mut pages = Vec::new();
    files_with_extension(&docs, "mdx", &mut pages);

    let mut stale = Vec::new();
    for path in &pages {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if let Some((name, pin)) = version_pin(line)
                && pin != current
            {
                stale.push(format!(
                    "{}:{}: `{name} = \"{pin}\"` but the crates are at {current}",
                    path.strip_prefix(&root).unwrap_or(path).display(),
                    i + 1,
                ));
            }
        }
    }

    assert!(
        stale.is_empty(),
        "documentation shows a Cargo.toml pinned to a version we no longer ship. A manifest \
         needs a literal version, so these have to be bumped by hand on release:\n  {}",
        stale.join("\n  "),
    );
}
