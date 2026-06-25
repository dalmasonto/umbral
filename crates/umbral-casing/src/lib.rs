//! Shared case-conversion helpers for the umbral workspace.
//!
//! This crate is intentionally dependency-free so it can be used from
//! `umbral-macros` (a proc-macro crate), `umbral-core`, `umbral-cli`, and
//! plugin crates without introducing a dependency cycle.
//!
//! # Functions
//!
//! | Function | Canonical call site | Notes |
//! |---|---|---|
//! | [`to_snake_case`] | `umbral-macros` (table-name derivation) | Full acronym-aware algorithm |
//! | [`pascal_case_from_table`] | `umbral-core/inspect.rs` | SQL table → struct name; lowercases segments |
//! | [`pascal_case_from_ident`] | `umbral-cli/scaffold.rs`, `umbral-openapi` | Ident / name → PascalCase; no lowercasing |

/// Convert a `PascalCase` or `camelCase` Rust identifier into `snake_case`.
///
/// # Algorithm
///
/// Inserts `_` before an uppercase letter when:
/// - the previous character was a lowercase letter or digit
///   (`blogPost` → `blog_post`, `post2Tag` → `post2_tag`), OR
/// - we are at the end of an uppercase run that is followed by a lowercase
///   letter (`HTTPRequest` → `http_request`, not `h_t_t_p_request`).
///
/// All non-uppercase characters (including digits and existing underscores)
/// pass through unchanged.  Non-ASCII characters pass through unchanged.
///
/// This is the **canonical** implementation — byte-identical to what
/// `#[derive(Model)]` uses to compute a model's `TABLE` constant, so the
/// output must never change without a coordinated migration.
///
/// # Examples
///
/// ```
/// use umbral_casing::to_snake_case;
///
/// assert_eq!(to_snake_case("BlogPost"),    "blog_post");
/// assert_eq!(to_snake_case("HTTPRequest"), "http_request");
/// assert_eq!(to_snake_case("Post2"),       "post2");
/// assert_eq!(to_snake_case("post_tag"),    "post_tag");
/// assert_eq!(to_snake_case("S3Upload"),    "s3_upload");
/// assert_eq!(to_snake_case("_Hidden"),     "_hidden");
/// ```
pub fn to_snake_case(camel: &str) -> String {
    let chars: Vec<char> = camel.chars().collect();
    let mut out = String::with_capacity(camel.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let next = chars.get(i + 1).copied();
            let prev_lower_or_digit =
                matches!(prev, Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit());
            let run_break = prev.map(|p| p.is_ascii_uppercase()).unwrap_or(false)
                && matches!(next, Some(n) if n.is_ascii_lowercase());
            if i != 0 && (prev_lower_or_digit || run_break) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Convert a SQL table name (typically `snake_case`) into `PascalCase` for
/// use as a Rust struct name.
///
/// Splits on `_`, ` `, and `-`.  Non-alphanumeric characters that are not
/// separators are skipped.  The **first** character of each segment is
/// uppercased; the rest are **lowercased** — this normalises table names
/// that may have been created in ALLCAPS or mixed case.
///
/// Used by `umbral-core/inspect.rs` (the `inspectdb` command) to generate
/// struct names from introspected table names.
///
/// # Examples
///
/// ```
/// use umbral_casing::pascal_case_from_table;
///
/// assert_eq!(pascal_case_from_table("blog_post"),        "BlogPost");
/// assert_eq!(pascal_case_from_table("auth_user_groups"), "AuthUserGroups");
/// assert_eq!(pascal_case_from_table("post"),             "Post");
/// assert_eq!(pascal_case_from_table("tag"),              "Tag");
/// assert_eq!(pascal_case_from_table("BLOG_POST"),        "BlogPost");
/// assert_eq!(pascal_case_from_table("blog-post"),        "BlogPost");
/// ```
pub fn pascal_case_from_table(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut upper_next = true;
    for ch in input.chars() {
        if ch == '_' || ch == ' ' || ch == '-' {
            upper_next = true;
            continue;
        }
        if !ch.is_alphanumeric() {
            continue;
        }
        if upper_next {
            for u in ch.to_uppercase() {
                out.push(u);
            }
            upper_next = false;
        } else {
            for l in ch.to_lowercase() {
                out.push(l);
            }
        }
    }
    out
}

/// Convert a kebab/snake-case identifier or user-provided name into
/// `PascalCase` for use as a Rust type name or plugin struct name.
///
/// Splits on `-` and `_`.  The **first** character of each segment is
/// uppercased; the remaining characters are passed through **unchanged**.
/// This preserves digits and already-correct casing in subsequent positions
/// (`api2` → `Api2`, not `Api2` which lowercasing would give).
///
/// Used by `umbral-cli/scaffold.rs` (to produce `{Name}Plugin`) and
/// `umbral-openapi` (to produce OpenAPI schema names from model names that
/// are already PascalCase).
///
/// # Examples
///
/// ```
/// use umbral_casing::pascal_case_from_ident;
///
/// assert_eq!(pascal_case_from_ident("posts"),       "Posts");
/// assert_eq!(pascal_case_from_ident("blog_engine"), "BlogEngine");
/// assert_eq!(pascal_case_from_ident("blog-engine"), "BlogEngine");
/// assert_eq!(pascal_case_from_ident("api2"),        "Api2");
/// assert_eq!(pascal_case_from_ident("BlogPost"),    "BlogPost");
/// assert_eq!(pascal_case_from_ident("task_queue"),  "TaskQueue");
/// ```
pub fn pascal_case_from_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut next_upper = true;
    for c in name.chars() {
        if c == '-' || c == '_' {
            next_upper = true;
        } else if next_upper {
            out.push(c.to_ascii_uppercase());
            next_upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── to_snake_case ──────────────────────────────────────────────────────

    #[test]
    fn snake_simple_pascal() {
        assert_eq!(to_snake_case("BlogPost"), "blog_post");
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("Comment"), "comment");
    }

    #[test]
    fn snake_acronym_handling() {
        // The run_break rule collapses consecutive caps before a lowercase.
        assert_eq!(to_snake_case("HTTPRequest"), "http_request");
        assert_eq!(to_snake_case("URLParser"), "url_parser");
        assert_eq!(to_snake_case("JSONBody"), "json_body");
        assert_eq!(to_snake_case("APIKey"), "api_key");
    }

    #[test]
    fn snake_trailing_acronym() {
        // Trailing all-caps run has no following lowercase — no split inside.
        assert_eq!(to_snake_case("ParseHTTP"), "parse_http");
        assert_eq!(to_snake_case("RequestURL"), "request_url");
    }

    #[test]
    fn snake_digits() {
        assert_eq!(to_snake_case("Post2"), "post2");
        assert_eq!(to_snake_case("S3Upload"), "s3_upload");
        assert_eq!(to_snake_case("post2Tag"), "post2_tag");
    }

    #[test]
    fn snake_already_snake() {
        // Existing underscores and lowercase pass through unchanged.
        assert_eq!(to_snake_case("post_tag"), "post_tag");
        assert_eq!(to_snake_case("blog_post"), "blog_post");
    }

    #[test]
    fn snake_leading_underscore() {
        assert_eq!(to_snake_case("_Hidden"), "_hidden");
    }

    #[test]
    fn snake_single_char() {
        assert_eq!(to_snake_case("A"), "a");
        assert_eq!(to_snake_case("a"), "a");
    }

    #[test]
    fn snake_empty() {
        assert_eq!(to_snake_case(""), "");
    }

    // ── pascal_case_from_table ─────────────────────────────────────────────

    #[test]
    fn table_simple() {
        assert_eq!(pascal_case_from_table("post"), "Post");
        assert_eq!(pascal_case_from_table("tag"), "Tag");
        assert_eq!(pascal_case_from_table("blog_post"), "BlogPost");
        assert_eq!(pascal_case_from_table("auth_user_groups"), "AuthUserGroups");
    }

    #[test]
    fn table_normalises_allcaps() {
        assert_eq!(pascal_case_from_table("BLOG_POST"), "BlogPost");
        assert_eq!(pascal_case_from_table("USER"), "User");
    }

    #[test]
    fn table_handles_hyphen_and_space() {
        assert_eq!(pascal_case_from_table("blog-post"), "BlogPost");
        assert_eq!(pascal_case_from_table("blog post"), "BlogPost");
    }

    #[test]
    fn table_skips_non_alphanumeric() {
        // A table name with a non-separator, non-alphanumeric char.
        assert_eq!(pascal_case_from_table("foo!bar"), "Foobar");
    }

    #[test]
    fn table_empty() {
        assert_eq!(pascal_case_from_table(""), "");
    }

    // ── pascal_case_from_ident ─────────────────────────────────────────────

    #[test]
    fn ident_simple() {
        assert_eq!(pascal_case_from_ident("posts"), "Posts");
        assert_eq!(pascal_case_from_ident("blog_engine"), "BlogEngine");
        assert_eq!(pascal_case_from_ident("blog-engine"), "BlogEngine");
        assert_eq!(pascal_case_from_ident("task_queue"), "TaskQueue");
    }

    #[test]
    fn ident_digits_preserved() {
        assert_eq!(pascal_case_from_ident("api2"), "Api2");
    }

    #[test]
    fn ident_already_pascal() {
        // Model names are already PascalCase — must round-trip unchanged.
        assert_eq!(pascal_case_from_ident("BlogPost"), "BlogPost");
        assert_eq!(pascal_case_from_ident("UserProfile"), "UserProfile");
    }

    #[test]
    fn ident_empty() {
        assert_eq!(pascal_case_from_ident(""), "");
    }

    // ── cross-impl consistency check ───────────────────────────────────────

    #[test]
    fn snake_then_table_roundtrip() {
        // A PascalCase name → snake → table should recover the original.
        let cases = ["BlogPost", "UserProfile", "AuthGroup", "Comment"];
        for &name in &cases {
            let snake = to_snake_case(name);
            let back = pascal_case_from_table(&snake);
            assert_eq!(back, name, "roundtrip failed for {name}");
        }
    }
}
