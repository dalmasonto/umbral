//! Does `umbral startproject` emit a project that actually **compiles**?
//!
//! # Why a content assertion is not enough
//!
//! The other scaffold tests assert that the generated files contain the right strings. Every
//! one of them passed while `startproject` was emitting a project that did not build:
//!
//! - gaps3 #64 — the generated `main.rs` matched on `Environment`, which did not derive
//!   `PartialEq`. Shipped in 0.0.6.
//! - gaps3 #57 — the error handler used `?` on a `TemplateError`, and `ApiError` had no
//!   `From<TemplateError>`.
//! - an unused import that would have failed a `-D warnings` build.
//!
//! Three separate bugs in the *first thing a new user runs*, and the reason all three
//! survived is simple: **nobody built what `startproject` emits.** A test that greps the
//! output for a string can only tell you the generator wrote what the generator meant to
//! write. Only the compiler can tell you whether that was correct.
//!
//! So this test scaffolds a project and hands it to `cargo check`. It is the one test in this
//! repo whose verdict comes from rustc rather than from an assertion I wrote.
//!
//! # Why it is `#[ignore]`d
//!
//! It type-checks a whole application — every umbral crate plus axum, sqlx and the rest —
//! which is minutes and gigabytes, not milliseconds. Putting that in the default `cargo test`
//! path would make the ordinary loop unusable, so CI runs it instead
//! (`.github/workflows/scaffold.yml`, on every push).
//!
//! Run it locally with:
//!
//! ```bash
//! cargo test -p umbral-cli --test scaffold_compiles -- --ignored --nocapture
//! ```
//!
//! # Warnings are failures here
//!
//! A scaffold that emits a warning is a scaffold that teaches every new user to ignore
//! warnings on day one, in the file they are about to edit. So `RUSTFLAGS=-D warnings`: the
//! bar for generated code is not "it compiles", it is "it is clean".

use std::path::{Path, PathBuf};
use std::process::Command;

/// The repo root — the checkout whose `crates/` and `plugins/` the generated project path-deps.
fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/umbral-cli.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/umbral-cli")
        .to_path_buf()
}

#[test]
#[ignore = "type-checks an entire generated app (minutes, GBs). CI runs it; locally: --ignored"]
fn the_scaffolded_project_compiles_with_no_errors_and_no_warnings() {
    let repo = repo_root();
    assert!(
        repo.join("crates/umbral-core/Cargo.toml").exists(),
        "expected a source checkout at {}",
        repo.display()
    );

    let tmp = tempfile::tempdir().expect("tempdir");

    // `--local <repo>`, because the whole point is to check the code the generator emits
    // against the API it is being generated FROM. Without it the project pins the last
    // PUBLISHED umbral (the CLI's own `CARGO_PKG_VERSION`) and we would be testing 0.0.7's
    // API, not main's — which is gaps3 #65 itself, and would make this test blind to exactly
    // the skew it exists to catch.
    let report = umbral_cli::scaffold::scaffold_project("checkme", tmp.path(), Some(&repo))
        .expect("scaffold_project");

    // A shared, persistent target dir: a distinct path from the outer `target/`, so the
    // nested cargo does not contend for the lock the test runner is holding, and warm on the
    // second run instead of paying full price every time.
    let target_dir = repo.join("target/scaffold-check");

    let out = Command::new(env!("CARGO"))
        .current_dir(&report.root)
        .args(["check", "--all-targets", "--message-format=short"])
        .env("CARGO_TARGET_DIR", &target_dir)
        // The bar for generated code is not "compiles" — it is "clean". A warning in the
        // scaffold teaches every new user to ignore warnings in the first file they open.
        .env("RUSTFLAGS", "-D warnings")
        .output()
        .expect("cargo check should be runnable");

    let stderr = String::from_utf8_lossy(&out.stderr);

    if !out.status.success() {
        panic!(
            "`umbral startproject` emitted a project that does not compile.\n\
             This is the first thing a new user runs. Fix the SCAFFOLD, not the test.\n\n\
             cargo check said:\n{stderr}"
        );
    }

    // `-D warnings` already makes any rustc warning a hard error, so a clean exit implies
    // none. This second pass catches what `-D warnings` cannot: warnings cargo itself emits
    // (a deprecated manifest key, say), which land in the user's face just the same.
    //
    // Build-script notices are excluded — `warning: <pkg>@<version>: ...` is a crate's
    // build.rs talking, and we do not control every dependency's build script. We DO control
    // ours: umbral-admin used to announce a *successful* Tailwind build through that channel,
    // printing a `warning:` line into every build of every umbral app. This test is what
    // found it. A success notice dressed as a warning teaches users to skim past warnings,
    // and then the real one gets skimmed past too.
    let is_build_script_notice = |l: &str| {
        l.starts_with("warning: ")
            && l[9..]
                .split_once(": ")
                .is_some_and(|(pkg, _)| pkg.contains('@'))
    };
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("warning:") && !is_build_script_notice(l))
        .collect();
    assert!(
        warnings.is_empty(),
        "the scaffolded project compiles but is not clean:\n{}",
        warnings.join("\n")
    );
}
