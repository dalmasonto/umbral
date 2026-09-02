//! `umbral doctor` — gaps4 #65(c): a diagnostic for divergent umbral-critical
//! dependency versions.
//!
//! # The problem
//!
//! Nothing stops a plugin or base crate from naming its own version of a
//! crate umbral also depends on directly — most dangerously `sqlx`. When
//! two incompatible `sqlx` majors coexist in one dependency tree (say,
//! umbral resolves 0.8.6 while some other crate pins 0.9), the failure is
//! inscrutable: a `FromRow` impl generated against one version cannot
//! satisfy a `FromRow<PgRow>` bound that names the other, and the compiler
//! error mentions neither "sqlx" nor "two versions" — just a mysterious
//! unsatisfied trait bound. This happened for real (see gaps4 #65) and cost
//! real debugging time.
//!
//! `#[derive(sqlx::FromRow)]` still needs a direct `sqlx` dependency no
//! matter what — sqlx's derive expands to code with hardcoded absolute
//! `::sqlx::...` paths and has no `#[sqlx(crate = "...")]` escape hatch the
//! way serde does, so a re-export can't remove that dependency line (see
//! `umbral::sqlx`'s doc comment in `crates/umbral/src/lib.rs` for the full
//! story). The *prevention* that actually works is narrower: keep that
//! dependency's version matched to the one `umbral-core` itself pins
//! (`umbral startproject` / `umbral startplugin` generate it that way
//! already). This module is the *safety net* for when that match slips
//! anyway — a hand-edit, a third-party base crate with its own opinion — by
//! reading the project's `Cargo.lock` (the ground truth for what actually
//! got resolved) and flagging any umbral-critical crate that shows up at
//! more than one version, with a plain-English explanation instead of
//! leaving the trait-bound error to speak for itself.
//!
//! # Design
//!
//! Detection is a pure function, [`find_duplicate_versions`], over a plain
//! `(name, version)` list — independent of how that list was obtained, so
//! it's unit-testable against fixtures with no filesystem or subprocess
//! involved. [`parse_cargo_lock_packages`] is the one function that touches
//! `Cargo.lock` (parsed as TOML — the lockfile format), and [`run`] wires
//! the two together for the CLI.
//!
//! `Cargo.lock` is read directly rather than shelling out to
//! `cargo metadata`: it is already the exact resolved graph (metadata would
//! just re-derive it), avoids spawning a subprocess and paying its startup
//! cost, and doesn't require `cargo` to be resolvable as an external
//! command from wherever `umbral doctor` runs.

use std::fmt;
use std::path::{Path, PathBuf};

/// umbral-critical crates: ones the framework itself depends on directly,
/// where a second incompatible major version silently coexisting produces a
/// hard-to-diagnose failure (an unsatisfied trait bound, a type mismatch
/// across an FFI-ish boundary) rather than a normal "two copies of a small
/// leaf crate" non-issue. `sqlx` is the motivating case (gaps4 #65); `serde`
/// and `chrono` are named in the same gap entry as crates worth the same
/// scrutiny — a duplicate `serde` means two incompatible
/// `Serialize`/`Deserialize` impls, and a duplicate `chrono` means a
/// `DateTime<Utc>` from one version can't satisfy a bound naming the other.
pub const CRITICAL_CRATES: &[&str] = &["sqlx", "serde", "chrono"];

/// One umbral-critical crate resolved at more than one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupFinding {
    pub crate_name: String,
    /// Distinct versions found, sorted ascending (string order — good
    /// enough to present; these are for reading, not comparing).
    pub versions: Vec<String>,
}

/// Scan `packages` (each a resolved `(crate_name, version)` pair — as many
/// entries as there are resolved package instances, duplicates included)
/// for any name in `critical` that resolved to more than one distinct
/// version. Pure and filesystem-free so it's directly unit-testable against
/// fixture data.
///
/// Findings are returned in `critical`'s order (stable, so `CRITICAL_CRATES`
/// order drives the report) with each finding's `versions` sorted.
pub fn find_duplicate_versions(
    packages: &[(String, String)],
    critical: &[&str],
) -> Vec<DupFinding> {
    critical
        .iter()
        .filter_map(|&name| {
            let mut versions: Vec<String> = packages
                .iter()
                .filter(|(pkg, _)| pkg == name)
                .map(|(_, v)| v.clone())
                .collect();
            versions.sort();
            versions.dedup();
            if versions.len() > 1 {
                Some(DupFinding {
                    crate_name: name.to_string(),
                    versions,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Parse a `Cargo.lock`'s contents into `(name, version)` pairs, one per
/// `[[package]]` entry. `Cargo.lock` is TOML (the lockfile format, not a
/// `Cargo.toml` manifest), so this is a straight `toml::Value` walk — no
/// serde derive, no schema beyond "an array of tables with `name` and
/// `version` string fields."
pub fn parse_cargo_lock_packages(contents: &str) -> Result<Vec<(String, String)>, String> {
    let value: umbral::toml::Value = contents
        .parse()
        .map_err(|e| format!("Cargo.lock is not valid TOML: {e}"))?;
    let packages = value
        .get("package")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Cargo.lock has no [[package]] entries".to_string())?;
    Ok(packages
        .iter()
        .filter_map(|pkg| {
            let name = pkg.get("name")?.as_str()?;
            let version = pkg.get("version")?.as_str()?;
            Some((name.to_string(), version.to_string()))
        })
        .collect())
}

/// Walk `start` and its ancestors for the nearest `Cargo.lock`, the same
/// upward search `cargo` itself does for a manifest (mirrors
/// [`crate::in_cargo_project`]'s `Cargo.toml` walk).
pub fn find_cargo_lock(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join("Cargo.lock"))
        .find(|p| p.is_file())
}

/// The rendered report `run` prints, and what it returns to the caller.
/// `duplicates` empty means clean.
pub struct DoctorReport {
    pub lock_path: PathBuf,
    pub duplicates: Vec<DupFinding>,
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "umbral doctor")?;
        writeln!(f, "=============")?;
        writeln!(f, "Scanned {}", self.lock_path.display())?;
        writeln!(f)?;
        if self.duplicates.is_empty() {
            writeln!(
                f,
                "No duplicate versions found for: {}",
                CRITICAL_CRATES.join(", ")
            )?;
            return Ok(());
        }
        for dup in &self.duplicates {
            let versions = dup.versions.join(" AND ");
            writeln!(
                f,
                "\u{26a0} {}: found {} versions in your dependency tree — {versions}",
                dup.crate_name,
                dup.versions.len()
            )?;
            writeln!(
                f,
                "  Two `{}` crates can coexist in the same binary silently: an impl generated \n  \
                 against one version cannot satisfy a bound that names the other, and the \n  \
                 compiler error will not mention \"{}\" or \"two versions\" — just an \n  \
                 unsatisfied trait bound that looks like a framework bug.",
                dup.crate_name, dup.crate_name
            )?;
            writeln!(
                f,
                "  Fix: grep every `Cargo.toml` under your project for `^{name} =` (root, every \n  \
                 crate under plugins/, every path-dependency) and make them all name the SAME \n  \
                 version — `cargo tree -i {name}` shows who currently wants which. Prefer \n  \
                 whichever version `umbral-core` itself pins (check its `Cargo.toml`, or a \n  \
                 fresh `umbral startproject` scaffold, for the current value). If a plugin \n  \
                 genuinely needs a newer `{name}` than umbral ships, that's a real \n  \
                 incompatibility to raise upstream, not something to route around with two \n  \
                 versions.",
                name = dup.crate_name
            )?;
            if dup.crate_name == "sqlx" {
                writeln!(
                    f,
                    "  Note: `umbral::sqlx` (the facade's public re-export) does not let you \n  \
                     remove this dependency and still `#[derive(FromRow)]` — sqlx's derive \n  \
                     hardcodes absolute `::sqlx::...` paths with no crate-path override, so a \n  \
                     model still needs its own direct `sqlx` dependency. Pinning the version \n  \
                     to match is the actual fix here, not dropping the dependency."
                )?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Run the diagnostic starting from `start_dir`: find the nearest
/// `Cargo.lock`, parse it, and check umbral's [`CRITICAL_CRATES`] for
/// duplicate versions.
pub fn run(start_dir: &Path) -> Result<DoctorReport, String> {
    let lock_path = find_cargo_lock(start_dir).ok_or_else(|| {
        format!(
            "no Cargo.lock found in {} or any parent directory. `umbral doctor` reads the \
             resolved dependency graph, so run it from inside a project that has been built at \
             least once (`cargo build` / `cargo check` generates Cargo.lock).",
            start_dir.display()
        )
    })?;
    let contents = std::fs::read_to_string(&lock_path)
        .map_err(|e| format!("reading {}: {e}", lock_path.display()))?;
    let packages = parse_cargo_lock_packages(&contents)?;
    let duplicates = find_duplicate_versions(&packages, CRITICAL_CRATES);
    Ok(DoctorReport {
        lock_path,
        duplicates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn no_findings_when_every_critical_crate_is_single_version() {
        let packages = pkgs(&[
            ("sqlx", "0.8.6"),
            ("serde", "1.0.210"),
            ("chrono", "0.4.38"),
            ("some-leaf-crate", "2.0.0"),
            ("some-leaf-crate", "3.0.0"), // non-critical dup: ignored
        ]);
        let findings = find_duplicate_versions(&packages, CRITICAL_CRATES);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn flags_a_duplicated_sqlx_version_the_way_the_real_incident_looked() {
        // The exact real-world case gaps4 #65 names: umbral/plugins resolve
        // 0.8.6, a `common` base crate pins 0.9.0.
        let packages = pkgs(&[("sqlx", "0.8.6"), ("sqlx", "0.9.0"), ("serde", "1.0.210")]);
        let findings = find_duplicate_versions(&packages, CRITICAL_CRATES);
        assert_eq!(
            findings,
            vec![DupFinding {
                crate_name: "sqlx".to_string(),
                versions: vec!["0.8.6".to_string(), "0.9.0".to_string()],
            }]
        );
    }

    #[test]
    fn flags_every_duplicated_critical_crate_independently() {
        let packages = pkgs(&[
            ("sqlx", "0.8.6"),
            ("sqlx", "0.9.0"),
            ("chrono", "0.4.38"),
            ("chrono", "0.4.31"),
            ("serde", "1.0.210"),
        ]);
        let findings = find_duplicate_versions(&packages, CRITICAL_CRATES);
        let names: Vec<&str> = findings.iter().map(|f| f.crate_name.as_str()).collect();
        assert_eq!(names, vec!["sqlx", "chrono"]);
    }

    #[test]
    fn versions_are_deduplicated_and_sorted_not_just_counted() {
        // Three resolved instances but only two distinct versions — a
        // duplicate version string (e.g. two dependents both landing on
        // 0.8.6) must not read as three versions.
        let packages = pkgs(&[("sqlx", "0.9.0"), ("sqlx", "0.8.6"), ("sqlx", "0.8.6")]);
        let findings = find_duplicate_versions(&packages, CRITICAL_CRATES);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].versions, vec!["0.8.6", "0.9.0"]);
    }

    #[test]
    fn parses_a_realistic_cargo_lock_fixture() {
        let lock = r#"
# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "sqlx"
version = "0.8.6"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "sqlx"
version = "0.9.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "1.0.210"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
        let packages = parse_cargo_lock_packages(lock).expect("parse");
        assert_eq!(
            packages,
            vec![
                ("sqlx".to_string(), "0.8.6".to_string()),
                ("sqlx".to_string(), "0.9.0".to_string()),
                ("serde".to_string(), "1.0.210".to_string()),
            ]
        );
        let findings = find_duplicate_versions(&packages, CRITICAL_CRATES);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].crate_name, "sqlx");
    }

    #[test]
    fn rejects_malformed_toml_with_a_named_error() {
        let err = parse_cargo_lock_packages("not valid toml {{{").unwrap_err();
        assert!(err.contains("Cargo.lock"), "{err}");
    }

    #[test]
    fn report_renders_a_plain_english_message_naming_both_versions() {
        let report = DoctorReport {
            lock_path: PathBuf::from("/tmp/project/Cargo.lock"),
            duplicates: vec![DupFinding {
                crate_name: "sqlx".to_string(),
                versions: vec!["0.8.6".to_string(), "0.9.0".to_string()],
            }],
        };
        let text = report.to_string();
        assert!(text.contains("sqlx"), "{text}");
        assert!(text.contains("0.8.6"), "{text}");
        assert!(text.contains("0.9.0"), "{text}");
        assert!(text.contains("umbral::sqlx"), "{text}");
    }

    #[test]
    fn report_renders_a_clean_bill_of_health_when_no_duplicates() {
        let report = DoctorReport {
            lock_path: PathBuf::from("/tmp/project/Cargo.lock"),
            duplicates: vec![],
        };
        let text = report.to_string();
        assert!(text.contains("No duplicate versions found"), "{text}");
    }

    #[test]
    fn find_cargo_lock_walks_up_from_a_nested_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("Cargo.lock"), "version = 4\n").expect("write lock");
        let nested = tmp.path().join("src").join("bin");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let found = find_cargo_lock(&nested).expect("found");
        assert_eq!(found, tmp.path().join("Cargo.lock"));
    }

    #[test]
    fn find_cargo_lock_returns_none_with_no_lockfile_anywhere_up_to_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A tempdir has no Cargo.lock among its ancestors up to the
        // filesystem root either — unless the CI checkout root happens to
        // BE an ancestor, which tempdir (under /tmp) never is.
        assert!(find_cargo_lock(tmp.path()).is_none());
    }
}
