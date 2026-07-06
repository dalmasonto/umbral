//! End-to-end scaffolding tests.
//!
//! The unit tests in `scaffold.rs` cover validation helpers. These
//! tests drive the real `scaffold_project` / `scaffold_app` writers
//! against a `tempfile::TempDir`, assert the expected files land,
//! and pin a few key invariants in the generated content.

use std::fs;

use tempfile::TempDir;
use umbral_cli::scaffold::{
    ScaffoldError, register_dep_in_cargo_toml, scaffold_app, scaffold_plugin, scaffold_project,
};

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
        "umbral.toml",
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
        "umbral-auth",
        "umbral-sessions",
        "umbral-admin",
        "umbral-rest",
        "umbral-openapi",
        "umbral-security",
        "sqlx",
    ] {
        assert!(
            cargo.contains(dep),
            "Cargo.toml should list {dep} as a dep; got:\n{cargo}"
        );
    }
}

// features.md #5: every built-in plugin appears in the generated
// Cargo.toml. The non-default ones are commented out (`# umbral-…`)
// but listed so the user can discover them by skimming the manifest.
#[test]
fn scaffold_project_cargo_toml_lists_every_builtin_plugin_at_least_commented() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let cargo = fs::read_to_string(report.root.join("Cargo.toml")).unwrap();

    for plugin in &[
        "umbral-playground",
        "umbral-tasks",
        "umbral-permissions",
        "umbral-rls",
        "umbral-cache",
        "umbral-email",
        "umbral-storage",
        "umbral-signals",
        "umbral-security",
    ] {
        assert!(
            cargo.contains(plugin),
            "Cargo.toml should list every built-in (active or commented); missing {plugin} in:\n{cargo}",
        );
    }

    // Optional built-ins should appear as commented-out lines (each on
    // its own line starting with `# umbral-…`). Pick three at random as
    // sentinels — full coverage is the loop above.
    for plugin in &["umbral-tasks", "umbral-playground", "umbral-cache"] {
        assert!(
            cargo
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("# {plugin}"))),
            "{plugin} should be present as a commented-out line in Cargo.toml",
        );
    }
}

// gap 20: the scaffolded project references all major plugin and auth
// surfaces. With the per-concern layout (gaps2 #8) the App-wiring
// surfaces stay in main.rs while the handler-level surfaces moved to
// views/public.rs; assert each marker in the file that now owns it.
#[test]
fn scaffold_project_main_rs_references_all_plugins() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let main_rs = fs::read_to_string(report.root.join("src/main.rs")).unwrap();
    let public_rs = fs::read_to_string(report.root.join("src/views/public.rs")).unwrap();

    // App-wiring surfaces — these live in main.rs.
    let main_markers = &[
        "AuthPlugin",
        "SessionsPlugin",
        "AdminPlugin",
        "RestPlugin",
        "OpenApiPlugin",
        "SecurityPlugin",
        "SecurityConfig",
        "csrf_exempt_paths",
        "login_required_html",
        "ForeignKey",
        "ResourceConfig",
        "ResourceConfig::new(\"post\")",
        "umbral_cli::dispatch(app).await",
        "#[tokio::main]",
        "auto_migrate",
    ];
    for marker in main_markers {
        assert!(
            main_rs.contains(marker),
            "main.rs should contain `{marker}`;\ngot:\n{main_rs}"
        );
    }

    // Handler-level surfaces — these live in views/public.rs now.
    for marker in &["LoggedIn", "umbral::transaction"] {
        assert!(
            public_rs.contains(marker),
            "views/public.rs should contain `{marker}`;\ngot:\n{public_rs}"
        );
    }
}

// audit core-macros-cli #1: the generated dev-superuser seed must NOT
// plant a fixed-password `admin`/`admin` account. It has to be gated on
// the Dev environment AND opt-in via an env-var password — otherwise a
// bare `./app` launch against an empty prod DB mints a known-credential
// superuser.
#[test]
fn scaffold_project_seed_has_no_hardcoded_admin_password() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let creds = fs::read_to_string(report.root.join("src/seed/credentials.rs")).unwrap();

    // No hardcoded password literal reaches create_superuser.
    assert!(
        !creds.contains(r#"create_superuser("admin", "admin@example.com", "admin")"#),
        "generated credentials.rs must not hardcode an admin/admin superuser;\ngot:\n{creds}"
    );
    // The seed is gated on the Dev environment.
    assert!(
        creds.contains("Environment::Dev"),
        "seed must be gated on the Dev environment;\ngot:\n{creds}"
    );
    // The password is supplied via an env var (opt-in), not a literal.
    assert!(
        creds.contains("UMBRAL_DEV_ADMIN_PASSWORD"),
        "seed must read the password from an env var;\ngot:\n{creds}"
    );
    // The password passed to create_superuser is the env-var value, not a literal.
    assert!(
        creds.contains("create_superuser(\"admin\", \"admin@example.com\", &password)"),
        "create_superuser must receive the env-var password by reference;\ngot:\n{creds}"
    );
}

/// audit_2 macros-cli #7 — the scaffold must NOT write the shared literal
/// `umbral-insecure-dev-key-change-me`; each project gets a random dev key,
/// consistent between umbral.toml and the working .env.
#[test]
fn scaffold_project_generates_a_unique_dev_secret_key() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("keyapp", tmp.path(), None).unwrap();
    let toml = fs::read_to_string(report.root.join("umbral.toml")).unwrap();
    let env = fs::read_to_string(report.root.join(".env")).unwrap();

    assert!(
        !toml.contains("umbral-insecure-dev-key-change-me"),
        "umbral.toml must not ship the shared literal dev key;\ngot:\n{toml}"
    );
    assert!(
        !env.contains("umbral-insecure-dev-key-change-me"),
        ".env must not ship the shared literal dev key;\ngot:\n{env}"
    );

    // Extract the generated key from each file; they must match and be a real
    // 64-hex-char random key.
    let toml_key = toml
        .lines()
        .find_map(|l| l.strip_prefix("secret_key = \"")?.strip_suffix('"'))
        .expect("umbral.toml has a secret_key");
    let env_key = env
        .lines()
        .find_map(|l| l.strip_prefix("UMBRAL_SECRET_KEY="))
        .expect(".env has UMBRAL_SECRET_KEY");
    assert_eq!(toml_key, env_key, "toml + .env keys must match");
    assert_eq!(toml_key.len(), 64, "key is 64 hex chars");
    assert!(
        toml_key.chars().all(|c| c.is_ascii_hexdigit()),
        "key is all hex; got {toml_key}"
    );

    // A second scaffold produces a different key (not a fixed constant).
    let tmp2 = TempDir::new().unwrap();
    let report2 = scaffold_project("keyapp2", tmp2.path(), None).unwrap();
    let toml2 = fs::read_to_string(report2.root.join("umbral.toml")).unwrap();
    let key2 = toml2
        .lines()
        .find_map(|l| l.strip_prefix("secret_key = \"")?.strip_suffix('"'))
        .expect("second umbral.toml has a secret_key");
    assert_ne!(toml_key, key2, "two scaffolds must not share a dev key");
}

// audit core-macros-cli #5: the generated README must not claim that
// `serve` auto-migrates on first run — the boot guard only auto-migrates
// on a bare `cargo run` (no subcommand). `serve` is a subcommand and
// skips it.
#[test]
fn scaffold_project_readme_first_run_is_bare_cargo_run() {
    let tmp = TempDir::new().unwrap();
    let report = scaffold_project("testapp", tmp.path(), None).unwrap();
    let readme = fs::read_to_string(report.root.join("README.md")).unwrap();

    assert!(
        !readme.contains(
            "# First run — applies migrations and starts the server:\ncargo run -- serve"
        ),
        "README must not claim `serve` auto-migrates on first run;\ngot:\n{readme}"
    );
    assert!(
        readme.contains("bare `cargo run`"),
        "README should document that a bare `cargo run` auto-migrates;\ngot:\n{readme}"
    );
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
        main_rs.contains("umbral_cli::dispatch(app).await"),
        "generated main.rs should call umbral_cli::dispatch; got:\n{main_rs}"
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
    let umbral_toml = fs::read_to_string(report.root.join("umbral.toml")).unwrap();
    assert!(
        umbral_toml.contains("sqlite://acme.db"),
        "umbral.toml should default to a per-project DB URL; got:\n{umbral_toml}"
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

/// BUG-17: passing `--local /path/to/umbral` rewrites every git-dep
/// in the generated Cargo.toml into a `path = "..."` form anchored
/// at the supplied repo. Comments + commented-out optional plugins
/// all flow through; the version pin / trailing comment on a line
/// stays in place.
#[test]
fn scaffold_project_local_flag_emits_path_deps() {
    let tmp = TempDir::new().unwrap();
    let fake_umbral = tmp.path().join("checkout");
    std::fs::create_dir_all(&fake_umbral).unwrap();
    let report = scaffold_project("acme", tmp.path(), Some(&fake_umbral)).unwrap();
    let cargo_toml = std::fs::read_to_string(report.root.join("Cargo.toml")).unwrap();
    assert!(
        !cargo_toml.contains("git = \"https://github.com/dalmasonto/umbral\""),
        "no git deps should remain when --local was set; got:\n{cargo_toml}",
    );
    // Facade crate lands under crates/.
    let expected_umbral = format!("path = \"{}/crates/umbral\"", fake_umbral.display());
    assert!(
        cargo_toml.contains(&expected_umbral),
        "umbral should path-dep against crates/umbral; expected {expected_umbral:?}, got:\n{cargo_toml}",
    );
    // Plugin crate lands under plugins/.
    let expected_auth = format!("path = \"{}/plugins/umbral-auth\"", fake_umbral.display());
    assert!(
        cargo_toml.contains(&expected_auth),
        "umbral-auth should path-dep against plugins/umbral-auth; expected {expected_auth:?}, got:\n{cargo_toml}",
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
        models.contains("#[derive(umbral::orm::Model)]"),
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
    assert!(
        !added_second,
        "second call should return false (already present)"
    );

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
        report.cargo_toml_registered, None,
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
    let has_builder_step = report
        .next_steps
        .iter()
        .any(|s| s.contains("App::builder") || s.contains(".plugin("));
    assert!(
        has_builder_step,
        "next_steps must still tell the user to wire .plugin(...) in main.rs"
    );
}
