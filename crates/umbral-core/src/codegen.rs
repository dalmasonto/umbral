//! Scaffolding primitives — how a command writes code into the user's project.
//!
//! `umbral startcommand` needs these. So does any *plugin* that wants to
//! generate code: `umbral-rest` ships `startpermission` /
//! `startauthentication` / `startpagination` / `startthrottle`, and a
//! third-party plugin can ship its own generator with nothing more than the
//! facade. That's the whole point of putting this in core rather than in
//! `umbral-cli`: a plugin can't depend on the CLI (dependencies point
//! *inward*), so if the file surgery lived there, every plugin generator
//! would hand-roll its own — and hand-rolled `mod` insertion is how a
//! generator eats somebody's `main.rs`.
//!
//! What's here is deliberately small and boring:
//!
//! - [`Target`] / [`resolve_target`] — "root or which plugin?", the question
//!   every generator asks, answered against what's actually on disk.
//! - [`write_new_file`] — write, never overwrite.
//! - [`declare_module`] / [`insert_before_marker`] — the two edits a
//!   generator makes to a file it doesn't own.
//!
//! Every function that edits an existing file returns `Option`/`Result` and
//! **declines** when the file isn't the shape it expected. A generator that
//! guesses at a file it doesn't recognise is a generator that corrupts it;
//! the caller reports the lines to add by hand instead.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use umbral_casing::{pascal_case_from_ident, to_snake_case};

/// Where generated code lands: the project's own crate, or one of its
/// plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The project itself — `src/`, owned by `main.rs`.
    Root,
    /// A plugin crate — `plugins/<name>/src/`, owned by its `lib.rs`.
    Plugin(String),
}

impl Target {
    /// Parse the `--in` argument. `root` (any case) is the project; anything
    /// else names a plugin.
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("root") {
            Self::Root
        } else {
            Self::Plugin(s.to_string())
        }
    }
}

/// A resolved [`Target`]: the crate to write into, and the file that owns
/// its module tree.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    /// The crate root — the directory holding `src/` and `Cargo.toml`.
    pub crate_root: PathBuf,
    /// The file that declares the crate's modules: `src/main.rs` for the
    /// project, `src/lib.rs` for a plugin. A generator adds its `mod foo;`
    /// here.
    pub owner_file: PathBuf,
    /// True for [`Target::Root`]. Generators use it to pick between `mod x;`
    /// (a binary's private module) and `pub mod x;` (a library's public one).
    pub is_root: bool,
}

impl ResolvedTarget {
    /// `mod x;` in a binary, `pub mod x;` in a plugin library — the module
    /// declaration appropriate to this target.
    pub fn module_decl(&self, module: &str) -> String {
        if self.is_root {
            format!("mod {module};")
        } else {
            format!("pub mod {module};")
        }
    }
}

/// Errors a generator can hit before it writes anything.
#[derive(Debug)]
pub enum CodegenError {
    /// Not usable as a Rust identifier.
    InvalidName(String),
    /// The file is already there. Generators never overwrite: the file may be
    /// hours of someone's work with the same name.
    AlreadyExists(PathBuf),
    /// `--in <plugin>` named something that isn't under `plugins/`. Carries
    /// the names that ARE, so the message can list the real choices.
    NoSuchPlugin {
        asked: String,
        available: Vec<String>,
    },
    /// No `src/main.rs` here — this isn't a project root.
    NotAProject(PathBuf),
    /// I/O failure.
    Io(io::Error),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(s) => write!(
                f,
                "invalid name `{s}`: must be ASCII alphanumeric, underscore or hyphen, \
                 and must not start with a digit"
            ),
            Self::AlreadyExists(p) => write!(
                f,
                "`{}` already exists — pick another name, or delete it first. \
                 Nothing was written.",
                p.display()
            ),
            Self::NoSuchPlugin { asked, available } => {
                if available.is_empty() {
                    write!(
                        f,
                        "no plugin named `{asked}` — this project has no plugins yet. \
                         Create one with `umbral startapp <name>`, or use `--in root`."
                    )
                } else {
                    write!(
                        f,
                        "no plugin named `{asked}`. Available: root, {}.",
                        available.join(", ")
                    )
                }
            }
            Self::NotAProject(p) => write!(
                f,
                "`{}` doesn't look like an umbral project — no `src/main.rs`.",
                p.display()
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CodegenError {}

impl From<io::Error> for CodegenError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What a generator wrote, for the report it prints.
#[derive(Debug, Clone, Default)]
pub struct Scaffolded {
    /// The crate the files landed in.
    pub root: PathBuf,
    /// Files written, relative to `root`.
    pub files: Vec<PathBuf>,
    /// What the user still has to do — the registration line a generator
    /// can't place for them, or a file it declined to edit.
    pub next_steps: Vec<String>,
}

/// Validate a name as a Rust identifier stem: ASCII alphanumeric, `_`, `-`,
/// not starting with a digit, not empty.
pub fn validate_ident(name: &str) -> Result<(), CodegenError> {
    if name.is_empty() {
        return Err(CodegenError::InvalidName(String::new()));
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(CodegenError::InvalidName(name.to_string()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CodegenError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// The plugins available in this project: every `plugins/<name>/` that holds
/// a `Cargo.toml`.
///
/// Reads the disk, not `main.rs`. A plugin you scaffolded but haven't
/// registered yet is still a legitimate place to put code.
pub fn discover_plugins(project_root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(project_root.join("plugins")) else {
        return names;
    };
    for entry in entries.flatten() {
        if !entry.path().join("Cargo.toml").is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// Resolve a [`Target`] against the project on disk.
pub fn resolve_target(
    project_root: &Path,
    target: &Target,
) -> Result<ResolvedTarget, CodegenError> {
    match target {
        Target::Root => {
            let owner_file = project_root.join("src/main.rs");
            if !owner_file.is_file() {
                return Err(CodegenError::NotAProject(project_root.to_path_buf()));
            }
            Ok(ResolvedTarget {
                crate_root: project_root.to_path_buf(),
                owner_file,
                is_root: true,
            })
        }
        Target::Plugin(name) => {
            let crate_root = project_root.join("plugins").join(name);
            let owner_file = crate_root.join("src/lib.rs");
            if !owner_file.is_file() {
                return Err(CodegenError::NoSuchPlugin {
                    asked: name.clone(),
                    available: discover_plugins(project_root),
                });
            }
            Ok(ResolvedTarget {
                crate_root,
                owner_file,
                is_root: false,
            })
        }
    }
}

/// Write a file that must not already exist, recording it in `files`
/// (relative to `crate_root`) for the report.
pub fn write_new_file(
    crate_root: &Path,
    rel_path: &str,
    contents: &str,
    files: &mut Vec<PathBuf>,
) -> Result<(), CodegenError> {
    let full = crate_root.join(rel_path);
    if full.exists() {
        return Err(CodegenError::AlreadyExists(full));
    }
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&full, contents)?;
    files.push(PathBuf::from(rel_path));
    Ok(())
}

/// Add a module declaration (`mod foo;` / `pub mod foo;`) to a file's module
/// list, placed before the first existing declaration so the list stays
/// readable at the top of the file.
///
/// Returns `None` when the declaration is already there (idempotent — the
/// second generator run is a no-op) or when the file declares no modules at
/// all and there's no obvious place to put one. The caller reports it as a
/// manual step rather than guessing.
pub fn declare_module(text: &str, decl: &str) -> Option<String> {
    if text.lines().any(|l| l.trim() == decl) {
        return None;
    }
    let idx = text
        .lines()
        .position(|l| (l.starts_with("mod ") || l.starts_with("pub mod ")) && l.ends_with(';'))?;
    Some(insert_line_before(text, idx, decl))
}

/// Insert `line` immediately before the line that equals `marker` (trimmed).
///
/// Marker comments are how a generator finds its insertion point without
/// parsing Rust. Returns `None` when the marker is gone — the user
/// restructured the file, and a generator that "helpfully" rewrites a file it
/// no longer recognises is worse than one that asks.
pub fn insert_before_marker(text: &str, marker: &str, line: &str) -> Option<String> {
    let idx = text.lines().position(|l| l.trim() == marker)?;
    Some(insert_line_before(text, idx, line))
}

/// Ensure `<name> = <spec>` is listed under `[dependencies]` in a
/// `Cargo.toml`.
///
/// `Ok(true)` = added, `Ok(false)` = already there (idempotent),
/// `Err(_)` = the file has no `[dependencies]` section to add it to, and the
/// caller should report it rather than invent one.
///
/// The generator needs this because code scaffolded into a *plugin* crate
/// usually needs a dependency the plugin doesn't have yet — a REST permission
/// class in `plugins/blog/` doesn't compile until `plugins/blog/Cargo.toml`
/// depends on `umbral-rest`. Writing the file and leaving the crate unable to
/// build it is a generator that produced a broken project.
///
/// String surgery, not a TOML rewrite: comments, ordering and formatting of
/// the existing manifest all survive.
pub fn ensure_dependency(cargo_toml: &Path, name: &str, spec: &str) -> Result<bool, CodegenError> {
    let text = fs::read_to_string(cargo_toml)?;
    let key = format!("{name} =");
    if text.lines().any(|l| l.trim_start().starts_with(&key)) {
        return Ok(false);
    }
    let Some(idx) = text.lines().position(|l| l.trim() == "[dependencies]") else {
        return Err(CodegenError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{}` has no [dependencies] section", cargo_toml.display()),
        )));
    };
    let out = insert_line_after(&text, idx, &format!("{name} = {spec}"));
    fs::write(cargo_toml, out)?;
    Ok(true)
}

/// Insert `line` before line index `idx`.
fn insert_line_before(text: &str, idx: usize, line: &str) -> String {
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    for (i, l) in text.lines().enumerate() {
        if i == idx {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Insert `line` after line index `idx`.
fn insert_line_after(text: &str, idx: usize, line: &str) -> String {
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    for (i, l) in text.lines().enumerate() {
        out.push_str(l);
        out.push('\n');
        if i == idx {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Terminal prompts for a generator that asks before it writes.
///
/// Shared so `umbral startcommand` and a plugin's generator ask the same
/// questions the same way — and so a plugin author gets the non-obvious part
/// for free: **a prompt is only legal when stdin is a terminal.** Prompting a
/// pipe blocks a CI job forever on a question nothing will ever answer.
pub mod prompt {
    use std::io::{self, BufRead, IsTerminal, Write};
    use std::path::Path;

    use super::{Target, discover_plugins};

    /// Whether we may prompt at all. `false` in CI, a pipe, a cron job —
    /// where the caller must fall back to flags or fail with a clear message.
    pub fn is_interactive() -> bool {
        io::stdin().is_terminal()
    }

    /// Ask, and take a blank answer as an answer (the caller has a default).
    /// EOF (Ctrl-D) is a cancellation, not a blank.
    pub fn ask(question: &str) -> io::Result<String> {
        print!("{question}");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "cancelled"));
        }
        Ok(line.trim().to_string())
    }

    /// Ask until the answer isn't blank.
    pub fn ask_required(question: &str) -> io::Result<String> {
        loop {
            let answer = ask(question)?;
            if !answer.is_empty() {
                return Ok(answer);
            }
        }
    }

    /// Ask where generated code should live: the project root, or one of the
    /// plugins found on disk. Accepts the menu number or the name; blank takes
    /// the default (root).
    pub fn ask_target(project_root: &Path) -> io::Result<Target> {
        let plugins = discover_plugins(project_root);

        println!();
        println!("Where should it live?");
        println!("  1. root  — this project's own crate");
        for (i, p) in plugins.iter().enumerate() {
            println!("  {}. {p}  — the `{p}` plugin (travels with it)", i + 2);
        }
        if plugins.is_empty() {
            println!("  (no plugins yet — `umbral startapp <name>` creates one)");
        }
        println!();

        loop {
            let answer = ask("Choose [1]: ")?;
            if answer.is_empty() {
                return Ok(Target::Root);
            }
            if let Ok(n) = answer.parse::<usize>() {
                if n == 1 {
                    return Ok(Target::Root);
                }
                if let Some(p) = plugins.get(n - 2) {
                    return Ok(Target::Plugin(p.clone()));
                }
                println!("  no such choice: {n}");
                continue;
            }
            if answer.eq_ignore_ascii_case("root") || plugins.iter().any(|p| p == &answer) {
                return Ok(Target::parse(&answer));
            }
            println!("  no such target: `{answer}`");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_dependency_adds_once_and_never_twice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest = tmp.path().join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"blog\"\n\n[dependencies]\numbral = \"1\"\n",
        )
        .unwrap();

        assert!(ensure_dependency(&manifest, "umbral-rest", "\"0.0.9\"").unwrap());
        let text = fs::read_to_string(&manifest).unwrap();
        assert!(text.contains("umbral-rest = \"0.0.9\""), "{text}");
        // The existing manifest is untouched apart from the new line.
        assert!(text.contains("umbral = \"1\""), "{text}");

        // Idempotent: a second call adds nothing.
        assert!(!ensure_dependency(&manifest, "umbral-rest", "\"0.0.9\"").unwrap());
        assert_eq!(
            fs::read_to_string(&manifest)
                .unwrap()
                .matches("umbral-rest =")
                .count(),
            1
        );
    }

    #[test]
    fn ensure_dependency_refuses_a_manifest_with_no_dependencies_section() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest = tmp.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"blog\"\n").unwrap();
        assert!(ensure_dependency(&manifest, "umbral-rest", "\"0.0.9\"").is_err());
    }

    #[test]
    fn validate_ident_matches_rust_identifier_rules() {
        assert!(validate_ident("is_owner").is_ok());
        assert!(validate_ident("IsOwner").is_ok());
        assert!(validate_ident("cursor-pagination").is_ok());
        assert!(validate_ident("").is_err());
        assert!(validate_ident("2fast").is_err());
        assert!(validate_ident("is owner").is_err());
    }

    /// Either input form lands on the same pair: the struct is PascalCase,
    /// the module snake_case. A user typing `IsOwner` and a user typing
    /// `is_owner` must get the same file.
    #[test]
    fn casing_round_trips_from_either_input_form() {
        for input in ["IsOwner", "is_owner", "is-owner"] {
            let pascal = pascal_case_from_ident(input);
            assert_eq!(pascal, "IsOwner", "from {input}");
            assert_eq!(to_snake_case(&pascal), "is_owner", "from {input}");
        }
    }

    #[test]
    fn declare_module_inserts_before_the_first_existing_decl() {
        let text = "//! doc\n\nmod seed;\nmod views;\n\nuse foo;\n";
        let out = declare_module(text, "mod commands;").expect("should insert");
        assert!(
            out.contains("mod commands;\nmod seed;\nmod views;"),
            "{out}"
        );
    }

    #[test]
    fn declare_module_is_idempotent() {
        let text = "mod commands;\nmod seed;\n";
        assert!(
            declare_module(text, "mod commands;").is_none(),
            "a declaration already present must not be added twice"
        );
    }

    #[test]
    fn declare_module_declines_when_there_is_no_module_list() {
        let text = "//! just docs\n\nuse foo;\n";
        assert!(declare_module(text, "mod commands;").is_none());
    }

    #[test]
    fn insert_before_marker_places_the_line_above_the_marker() {
        let text = "pub mod a;\n// MARK\n";
        let out = insert_before_marker(text, "// MARK", "pub mod b;").expect("marker present");
        assert_eq!(out, "pub mod a;\npub mod b;\n// MARK\n");
    }

    #[test]
    fn insert_before_marker_declines_when_the_marker_is_gone() {
        let text = "pub mod a;\n";
        assert!(
            insert_before_marker(text, "// MARK", "pub mod b;").is_none(),
            "without its marker a generator must decline, not guess"
        );
    }

    #[test]
    fn write_new_file_never_overwrites() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut files = Vec::new();
        write_new_file(tmp.path(), "src/x.rs", "one", &mut files).expect("first write");
        let err = write_new_file(tmp.path(), "src/x.rs", "two", &mut files)
            .expect_err("second write must refuse");
        assert!(matches!(err, CodegenError::AlreadyExists(_)));
        assert_eq!(
            fs::read_to_string(tmp.path().join("src/x.rs")).unwrap(),
            "one",
            "the existing file was clobbered"
        );
    }

    #[test]
    fn resolve_target_names_the_owner_file_per_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("plugins/blog/src")).unwrap();
        fs::write(root.join("plugins/blog/Cargo.toml"), "").unwrap();
        fs::write(root.join("plugins/blog/src/lib.rs"), "").unwrap();

        let r = resolve_target(root, &Target::Root).expect("root");
        assert!(r.is_root);
        assert_eq!(r.owner_file, root.join("src/main.rs"));
        assert_eq!(r.module_decl("commands"), "mod commands;");

        let p = resolve_target(root, &Target::Plugin("blog".into())).expect("plugin");
        assert!(!p.is_root);
        assert_eq!(p.owner_file, root.join("plugins/blog/src/lib.rs"));
        assert_eq!(p.module_decl("commands"), "pub mod commands;");

        match resolve_target(root, &Target::Plugin("blgo".into())) {
            Err(CodegenError::NoSuchPlugin { asked, available }) => {
                assert_eq!(asked, "blgo");
                assert_eq!(available, vec!["blog".to_string()]);
            }
            other => panic!("expected NoSuchPlugin, got {other:?}"),
        }
    }
}
