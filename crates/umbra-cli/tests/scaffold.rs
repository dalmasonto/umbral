//! End-to-end scaffolding tests.
//!
//! The unit tests in `scaffold.rs` cover validation helpers. These
//! tests drive the real `scaffold_project` / `scaffold_app` writers
//! against a `tempfile::TempDir`, assert the expected files land,
//! and pin a few key invariants in the generated content.

use std::fs;

use tempfile::TempDir;
use umbra_cli::scaffold::{ScaffoldError, scaffold_app, scaffold_project};

#[test]
fn scaffold_project_writes_expected_files() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("myblog", tmp.path()).unwrap();

    let root = tmp.path().join("myblog");
    assert_eq!(report.root, root);
    assert!(root.is_dir());

    // All files the scaffolder is contracted to produce.
    let expected = [
        "Cargo.toml",
        "src/main.rs",
        "umbra.toml",
        ".env",
        ".env.example",
        ".gitignore",
        "README.md",
        "templates/base.html",
        "templates/home.html",
        "templates/dashboard.html",
        "templates/404.html",
        "templates/500.html",
    ];
    for path in &expected {
        let full = root.join(path);
        assert!(full.is_file(), "expected file at {}", full.display());
    }
}

// gap 20: Comprehensive scaffold — all plugin deps in Cargo.toml.
#[test]
fn scaffold_project_cargo_toml_references_all_plugins() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path()).unwrap();
    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();

    for dep in &[
        "umbra-auth",
        "umbra-sessions",
        "umbra-admin",
        "umbra-rest",
        "umbra-openapi",
    ] {
        assert!(
            cargo.contains(dep),
            "Cargo.toml should list {dep} as a dep; got:\n{cargo}"
        );
    }
}

// features.md #5: every built-in plugin appears in the generated
// Cargo.toml. The non-default ones are commented out (`# umbra-…`)
// but listed so the user can discover them by skimming the manifest.
#[test]
fn scaffold_project_cargo_toml_lists_every_builtin_plugin_at_least_commented() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path()).unwrap();
    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();

    for plugin in &[
        "umbra-playground",
        "umbra-tasks",
        "umbra-permissions",
        "umbra-rls",
        "umbra-cache",
        "umbra-email",
        "umbra-media",
        "umbra-signals",
        "umbra-static",
        "umbra-security",
    ] {
        assert!(
            cargo.contains(plugin),
            "Cargo.toml should list every built-in (active or commented); missing {plugin} in:\n{cargo}",
        );
    }

    // All ten should appear as commented-out lines (each on its own
    // line starting with `# umbra-…`). Pick three at random as
    // sentinels — full coverage is the loop above.
    for plugin in &["umbra-tasks", "umbra-playground", "umbra-cache"] {
        assert!(
            cargo
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("# {plugin}"))),
            "{plugin} should be present as a commented-out line in Cargo.toml",
        );
    }
}

// gap 20: main.rs references all major plugin and auth surfaces.
#[test]
fn scaffold_project_main_rs_references_all_plugins() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path()).unwrap();
    let main_rs = fs::read_to_string(report.root.join("src/main.rs")).unwrap();

    let markers = &[
        "AuthPlugin",
        "SessionsPlugin",
        "AdminPlugin",
        "RestPlugin",
        "OpenApiPlugin",
        "login_required_html",
        "LoggedIn",
        "ForeignKey",
        "umbra::transaction",
        "ResourceConfig",
        "enable_filters",
        "umbra_cli::dispatch(app).await",
        "#[tokio::main]",
        "auto_migrate",
    ];
    for marker in markers {
        assert!(
            main_rs.contains(marker),
            "main.rs should contain `{marker}`;\ngot:\n{main_rs}"
        );
    }
}

// gap 20: base.html contains the Tailwind CDN link.
#[test]
fn scaffold_project_base_html_has_tailwind_cdn() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path()).unwrap();
    let base = fs::read_to_string(report.root.join("templates/base.html")).unwrap();
    assert!(
        base.contains("cdn.tailwindcss.com"),
        "templates/base.html should include the Tailwind CDN link; got:\n{base}"
    );
}

// gap 20: template substitution — project name appears in base.html title
// and in Cargo.toml.
#[test]
fn scaffold_project_substitutes_project_name() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("acmecorp", tmp.path()).unwrap();

    let base = fs::read_to_string(report.root.join("templates/base.html")).unwrap();
    assert!(
        base.contains("acmecorp"),
        "base.html should include the project name 'acmecorp'; got:\n{base}"
    );

    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();
    assert!(
        cargo.contains("name = \"acmecorp\""),
        "Cargo.toml should set name = \"acmecorp\"; got:\n{cargo}"
    );

    let env = fs::read_to_string(report.root.join(".env")).unwrap();
    assert!(
        env.contains("acmecorp.db"),
        ".env should reference acmecorp.db; got:\n{env}"
    );
}

#[test]
fn scaffold_project_main_rs_wires_dispatch() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("blog", tmp.path()).unwrap();
    let main_rs = fs::read_to_string(report.root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("umbra_cli::dispatch(app).await"),
        "generated main.rs should call umbra_cli::dispatch; got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("#[tokio::main]"),
        "generated main.rs should declare a tokio runtime"
    );
}

#[test]
fn scaffold_project_uses_project_name_in_database_url_and_gitignore() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("acme", tmp.path()).unwrap();
    let umbra_toml = fs::read_to_string(report.root.join("umbra.toml")).unwrap();
    assert!(
        umbra_toml.contains("sqlite://acme.db"),
        "umbra.toml should default to a per-project DB URL; got:\n{umbra_toml}"
    );
    let gitignore = fs::read_to_string(report.root.join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("/acme.db*"),
        ".gitignore should ignore the project's DB; got:\n{gitignore}"
    );
}

#[test]
fn scaffold_project_refuses_to_overwrite_existing_directory() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("existing")).unwrap();
    let err = scaffold_project("existing", tmp.path()).unwrap_err();
    matches!(err, ScaffoldError::AlreadyExists(_));
}

#[test]
fn scaffold_project_rejects_invalid_names() {
    let tmp = TempDir::new().unwrap();
    assert!(matches!(
        scaffold_project("", tmp.path()),
        Err(ScaffoldError::InvalidName(_))
    ));
    assert!(matches!(
        scaffold_project("2cool", tmp.path()),
        Err(ScaffoldError::InvalidName(_))
    ));
    assert!(matches!(
        scaffold_project("has spaces", tmp.path()),
        Err(ScaffoldError::InvalidName(_))
    ));
}

#[test]
fn scaffold_app_writes_plugin_under_plugins_dir() {
    let tmp = TempDir::new().unwrap();
    // First scaffold a project so plugins/ has a sensible parent.
    scaffold_project("blog", tmp.path()).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("posts", &project_root).unwrap();
    assert_eq!(report.root, project_root.join("plugins").join("posts"));

    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"posts\""));

    let lib = fs::read_to_string(report.root.join("src/lib.rs")).unwrap();
    assert!(
        lib.contains("pub struct PostsPlugin"),
        "lib.rs should emit a PostsPlugin struct; got:\n{lib}"
    );
    assert!(
        lib.contains("impl Plugin for PostsPlugin"),
        "lib.rs should emit a Plugin impl"
    );
    assert!(
        lib.contains("fn name(&self) -> &'static str {\n        \"posts\""),
        "Plugin::name should return the lowercase name"
    );
}

#[test]
fn scaffold_app_pascal_cases_multi_word_names() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path()).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("blog-engine", &project_root).unwrap();
    let lib = fs::read_to_string(report.root.join("src/lib.rs")).unwrap();
    assert!(
        lib.contains("pub struct BlogEnginePlugin"),
        "kebab-case name should pascal-case to BlogEnginePlugin; got:\n{lib}"
    );
    assert!(
        lib.contains("blog_engine::BlogEnginePlugin"),
        "next-steps should reference the Rust identifier (underscored)"
    );
}

#[test]
fn scaffold_app_refuses_to_overwrite_existing_plugin() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path()).unwrap();
    let project_root = tmp.path().join("blog");

    scaffold_app("posts", &project_root).unwrap();
    let err = scaffold_app("posts", &project_root).unwrap_err();
    matches!(err, ScaffoldError::AlreadyExists(_));
}
