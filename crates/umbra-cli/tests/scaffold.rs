//! End-to-end scaffolding tests.
//!
//! The unit tests in `scaffold.rs` cover validation helpers. These
//! tests drive the real `scaffold_project` / `scaffold_app` writers
//! against a `tempfile::TempDir`, assert the expected files land,
//! and pin a few key invariants in the generated content.

use std::fs;

use tempfile::TempDir;
use umbra_cli::scaffold::{ScaffoldError, register_dep_in_cargo_toml, scaffold_app, scaffold_project, scaffold_plugin};

#[test]
fn scaffold_project_writes_expected_files() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("myblog", tmp.path(), None).unwrap();

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
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();

    for dep in &[
        "umbra-auth",
        "umbra-sessions",
        "umbra-admin",
        "umbra-rest",
        "umbra-openapi",
        "umbra-security",
        "sqlx",
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
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
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

    // Optional built-ins should appear as commented-out lines (each on
    // its own line starting with `# umbra-…`). Pick three at random as
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
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let main_rs = fs::read_to_string(report.root.join("src/main.rs")).unwrap();

    let markers = &[
        "AuthPlugin",
        "SessionsPlugin",
        "AdminPlugin",
        "RestPlugin",
        "OpenApiPlugin",
        "SecurityPlugin",
        "SecurityConfig",
        "csrf_exempt_paths",
        "login_required_html",
        "LoggedIn",
        "ForeignKey",
        "umbra::transaction",
        "ResourceConfig",
        "ResourceConfig::new(\"post\")",
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
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
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
    let report = scaffold_project("acmecorp", tmp.path(), None).unwrap();

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
    let report = scaffold_project("blog", tmp.path(), None).unwrap();
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
    let report = scaffold_project("acme", tmp.path(), None).unwrap();
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
    let err = scaffold_project("existing", tmp.path(), None).unwrap_err();
    matches!(err, ScaffoldError::AlreadyExists(_));
}

#[test]
fn scaffold_project_rejects_invalid_names() {
    let tmp = TempDir::new().unwrap();
    assert!(matches!(
        scaffold_project("", tmp.path(), None),
        Err(ScaffoldError::InvalidName(_))
    ));
    assert!(matches!(
        scaffold_project("2cool", tmp.path(), None),
        Err(ScaffoldError::InvalidName(_))
    ));
    assert!(matches!(
        scaffold_project("has spaces", tmp.path(), None),
        Err(ScaffoldError::InvalidName(_))
    ));
}

#[test]
fn scaffold_app_writes_plugin_under_plugins_dir() {
    let tmp = TempDir::new().unwrap();
    // First scaffold a project so plugins/ has a sensible parent.
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("posts", &project_root, None).unwrap();
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
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("blog-engine", &project_root, None).unwrap();
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
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    scaffold_app("posts", &project_root, None).unwrap();
    let err = scaffold_app("posts", &project_root, None).unwrap_err();
    matches!(err, ScaffoldError::AlreadyExists(_));
}

// =========================================================================
// BUG-17 + IMP-4 from bugs/tests/testBugs.md.
// =========================================================================

/// BUG-17: passing `--local /path/to/umbra` rewrites every git-dep
/// in the generated Cargo.toml into a `path = "..."` form anchored
/// at the supplied repo. Comments + commented-out optional plugins
/// all flow through; the version pin / trailing comment on a line
/// stays in place.
#[test]
fn scaffold_project_local_flag_emits_path_deps() {
    let tmp = TempDir::new().unwrap();
    let fake_umbra = tmp.path().join("checkout");
    std::fs::create_dir_all(&fake_umbra).unwrap();
    let report = scaffold_project("acme", tmp.path(), Some(&fake_umbra)).unwrap();
    let cargo_toml = std::fs::read_to_string(report.root.join("Cargo.toml")).unwrap();
    assert!(
        !cargo_toml.contains("git = \"https://github.com/dalmasonto/umbra\""),
        "no git deps should remain when --local was set; got:\n{cargo_toml}",
    );
    // Facade crate lands under crates/.
    let expected_umbra = format!("path = \"{}/crates/umbra\"", fake_umbra.display());
    assert!(
        cargo_toml.contains(&expected_umbra),
        "umbra should path-dep against crates/umbra; expected {expected_umbra:?}, got:\n{cargo_toml}",
    );
    // Plugin crate lands under plugins/.
    let expected_auth = format!("path = \"{}/plugins/umbra-auth\"", fake_umbra.display());
    assert!(
        cargo_toml.contains(&expected_auth),
        "umbra-auth should path-dep against plugins/umbra-auth; expected {expected_auth:?}, got:\n{cargo_toml}",
    );
}

/// IMP-4: `startapp` (the minimal scaffold) now writes a
/// `src/models.rs` stub alongside `src/lib.rs` so the user has an
/// obvious place to declare their first model. Previously they had
/// to create it themselves or step up to `startplugin` for the
/// richer layout.
#[test]
fn scaffold_app_writes_models_rs_stub() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("acme", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("acme");
    let report = scaffold_app("posts", &project_root, None).unwrap();
    let models = std::fs::read_to_string(report.root.join("src/models.rs")).unwrap();
    assert!(
        models.contains("#[derive(umbra::orm::Model)]"),
        "models.rs should show the canonical derive line in its example block; got:\n{models}",
    );
    let lib = std::fs::read_to_string(report.root.join("src/lib.rs")).unwrap();
    assert!(
        lib.contains("pub mod models;"),
        "lib.rs should declare `pub mod models;`; got:\n{lib}",
    );
    assert!(
        lib.contains("Plugin::models()") || lib.contains("fn models("),
        "lib.rs should reference Plugin::models() so the user sees where to register; got:\n{lib}",
    );
}

// =========================================================================
// gaps2 #67: startapp auto-registers plugin in project Cargo.toml
// =========================================================================

/// After `startapp`, the project's Cargo.toml must contain a path dep
/// for the new plugin. `cargo_toml_registered` must be `Some(true)`.
#[test]
fn scaffold_app_registers_path_dep_in_project_cargo_toml() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("posts", &project_root, None).unwrap();

    // The function must report that it added the dep.
    assert_eq!(
        report.cargo_toml_registered,
        Some(true),
        "cargo_toml_registered should be Some(true) when the dep was freshly added"
    );

    // The project's Cargo.toml must contain the path dep line.
    let cargo = fs::read_to_string(project_root.join("Cargo.toml")).unwrap();
    assert!(
        cargo.contains("posts = { path = \"plugins/posts\" }"),
        "project Cargo.toml must list the new plugin as a path dep; got:\n{cargo}"
    );
}

/// Running `startapp` a second time on a *different* name must not
/// duplicate the first dep, and must still register the second.
#[test]
fn scaffold_app_second_plugin_gets_registered_independently() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    scaffold_app("posts", &project_root, None).unwrap();
    let report2 = scaffold_app("comments", &project_root, None).unwrap();

    assert_eq!(report2.cargo_toml_registered, Some(true));

    let cargo = fs::read_to_string(project_root.join("Cargo.toml")).unwrap();
    // Count only non-comment lines (the template has a hint comment that
    // contains "posts = { path = ..." as a substring).
    let non_comment_lines: Vec<&str> = cargo
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let posts_count = non_comment_lines
        .iter()
        .filter(|l| l.contains("posts = { path = \"plugins/posts\" }"))
        .count();
    let comments_count = non_comment_lines
        .iter()
        .filter(|l| l.contains("comments = { path = \"plugins/comments\" }"))
        .count();
    assert_eq!(posts_count, 1, "posts dep should appear exactly once");
    assert_eq!(comments_count, 1, "comments dep should appear exactly once");
}

/// `register_dep_in_cargo_toml` is idempotent: calling it twice with the
/// same name must not write a duplicate line and must return `Ok(false)`
/// on the second call.
#[test]
fn register_dep_idempotent_no_duplicate() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path(), None).unwrap();
    let cargo_path = tmp.path().join("blog").join("Cargo.toml");

    let added_first = register_dep_in_cargo_toml(&cargo_path, "posts").unwrap();
    assert!(added_first, "first call should return true (dep was added)");

    let added_second = register_dep_in_cargo_toml(&cargo_path, "posts").unwrap();
    assert!(!added_second, "second call should return false (already present)");

    let cargo = fs::read_to_string(&cargo_path).unwrap();
    // Count only non-comment lines that declare the dep (the project
    // Cargo.toml template has `# blog-posts = { path = "plugins/posts" }`
    // as a hint comment which also contains the substring — exclude it).
    let count = cargo
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("posts = { path = \"plugins/posts\" }"))
        .count();
    assert_eq!(count, 1, "dep line must appear exactly once; got:\n{cargo}");
}

/// When `scaffold_app` is called without a project `Cargo.toml` present
/// (bare temp dir), `cargo_toml_registered` must be `None` (soft failure)
/// and the scaffold files must still be written.
#[test]
fn scaffold_app_succeeds_without_project_cargo_toml() {
    let tmp = TempDir::new().unwrap();
    // No scaffold_project call — project_root has no Cargo.toml.
    let report = scaffold_app("widgets", tmp.path(), None).unwrap();

    assert_eq!(
        report.cargo_toml_registered,
        None,
        "should be None when no project Cargo.toml is present"
    );

    // Files must have been written regardless.
    assert!(
        report.root.join("Cargo.toml").is_file(),
        "plugin Cargo.toml must be written even when project Cargo.toml is absent"
    );
    assert!(
        report.root.join("src/lib.rs").is_file(),
        "plugin src/lib.rs must be written even when project Cargo.toml is absent"
    );
}

/// `startplugin` (the richer scaffold) must also auto-register the
/// path dep — same contract as startapp.
#[test]
fn scaffold_plugin_registers_path_dep_in_project_cargo_toml() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_plugin("widgets", &project_root, None).unwrap();

    assert_eq!(
        report.cargo_toml_registered,
        Some(true),
        "scaffold_plugin should register the dep in Cargo.toml"
    );

    let cargo = fs::read_to_string(project_root.join("Cargo.toml")).unwrap();
    assert!(
        cargo.contains("widgets = { path = \"plugins/widgets\" }"),
        "project Cargo.toml must list widgets as a path dep; got:\n{cargo}"
    );
}

/// The next_steps for startapp must NOT include the manual
/// "add to Cargo.toml" instruction — we do it automatically now.
#[test]
fn scaffold_app_next_steps_no_longer_mention_manual_cargo_toml_edit() {
    let tmp = TempDir::new().unwrap();
    scaffold_project("blog", tmp.path(), None).unwrap();
    let project_root = tmp.path().join("blog");

    let report = scaffold_app("posts", &project_root, None).unwrap();

    // None of the next_steps lines should tell the user to manually edit
    // Cargo.toml — that step is done automatically.
    for step in &report.next_steps {
        assert!(
            !step.contains("Cargo.toml"),
            "next_steps should not ask user to edit Cargo.toml manually \
             (auto-registered); offending step: {step:?}"
        );
    }
    // But the builder wiring step must still be present.
    let has_builder_step = report.next_steps.iter().any(|s| s.contains("App::builder") || s.contains(".plugin("));
    assert!(
        has_builder_step,
        "next_steps must still tell the user to wire .plugin(...) in main.rs"
    );
}
