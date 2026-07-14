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

/// The GENERATORS emit code too, and nobody was compiling it either.
///
/// `umbral startcommand` and umbral-rest's four class generators
/// (`startpermission` / `startauthentication` / `startpagination` /
/// `startthrottle`) are string templates. String templates have no
/// typechecker, and the unit tests around them assert on *substrings* — which
/// is the same trap that let three broken `startproject` templates ship, and it
/// caught the generators too: `PaginationScalar::Integer` does not exist (it is
/// `Number`), and `RateLimiter::new` takes a `Rate`, not a `&str`. Both compiled
/// fine as strings. Both were found only by handing the output to rustc.
///
/// So this does that, in CI, every push: scaffold a project, run every
/// generator, wire the results into the App exactly as the generators' own
/// printed instructions say to, and type-check the lot with warnings denied.
#[test]
#[ignore = "type-checks an entire generated app (minutes, GBs). CI runs it; locally: --ignored"]
fn the_generated_commands_and_rest_classes_compile_with_no_warnings() {
    use umbral::codegen::Target;
    use umbral_cli::scaffold::scaffold_command;

    let repo = repo_root();
    let tmp = tempfile::tempdir().expect("tempdir");
    let report =
        umbral_cli::scaffold::scaffold_project("genme", tmp.path(), Some(&repo)).expect("project");
    let root = &report.root;
    let target_dir = repo.join("target/scaffold-check");

    // A project-owned command, and one inside a plugin — both wiring paths.
    umbral_cli::scaffold::scaffold_app("blog", root, Some(&repo)).expect("startapp");
    scaffold_command("backfill_slugs", &Target::Root, root).expect("startcommand --in root");
    scaffold_command("reindex", &Target::Plugin("blog".into()), root)
        .expect("startcommand --in blog");

    // The REST classes are driven through the PROJECT'S OWN BINARY — `cargo run --
    // startpermission IsOwner` — which is how a user reaches them, and which is
    // also the only honest way to test them: they are plugin commands, so this
    // exercises `Plugin::commands()` dispatch and the generator together. It also
    // keeps `umbral-cli` from dev-depending on `umbral-rest`, which would add an
    // edge to the publish order for a test's sake.
    for (cmd, name) in [
        ("startpermission", "IsOwner"),
        ("startauthentication", "ApiKeyAuth"),
        ("startpagination", "CursorPagination"),
        ("startthrottle", "BurstThrottle"),
    ] {
        let out = Command::new(env!("CARGO"))
            .current_dir(root)
            .args(["run", "--quiet", "--", cmd, name, "--in", "root"])
            .env("CARGO_TARGET_DIR", &target_dir)
            .output()
            .unwrap_or_else(|e| panic!("running `{cmd}`: {e}"));
        assert!(
            out.status.success(),
            "`cargo run -- {cmd} {name}` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Wire the classes into the RestPlugin, which is what the generators tell the
    // user to do. Unwired, each class's re-export is an unused import — a true
    // warning, and the nudge that you have generated something you haven't used
    // yet. Wired, the project must be clean.
    let main_rs = root.join("src/main.rs");
    let src = std::fs::read_to_string(&main_rs).expect("read main.rs");
    let src = src.replace(
        "use umbral_rest::{RestPlugin, ResourceConfig};",
        "use umbral_rest::{RestPlugin, ResourceConfig};\n\
         use crate::authentication::ApiKeyAuth;\n\
         use crate::pagination::CursorPagination;\n\
         use crate::permissions::IsOwner;\n\
         use crate::throttles::BurstThrottle;",
    );
    let src = src.replace(
        "            RestPlugin::default()\n                .resource(ResourceConfig::new(\"post\")),",
        "            RestPlugin::default()\n                .resource(ResourceConfig::new(\"post\"))\n\
         \x20               .default_permission(IsOwner)\n\
         \x20               .authenticate(ApiKeyAuth)\n\
         \x20               .paginate(CursorPagination)\n\
         \x20               .default_throttle(BurstThrottle::new()),",
    );
    assert!(
        src.contains(".default_permission(IsOwner)"),
        "the startproject main.rs no longer has the RestPlugin shape this test wires into — \
         update the fixture, and check the generators' printed instructions still match too"
    );
    std::fs::write(&main_rs, src).expect("write main.rs");

    let out = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["check", "--all-targets", "--message-format=short"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("RUSTFLAGS", "-D warnings")
        .output()
        .expect("cargo check should be runnable");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a generator emitted code that does not compile. Fix the TEMPLATE, not the test.\n\n\
         cargo check said:\n{stderr}"
    );
}
