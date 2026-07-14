//! `startpermission` / `startauthentication` / `startpagination` /
//! `startthrottle` — the REST plugin scaffolds its own extension points.
//!
//! REST has four pluggable trait families, and every one of them is a struct
//! the *user* writes: who is the caller ([`Authentication`]), what may they do
//! ([`Permission`]), how are list responses sliced ([`Pagination`]), how often
//! may they ask ([`Throttle`]). Each is a small impl of an obvious trait —
//! which is exactly the kind of code that is annoying to start and easy to get
//! subtly wrong (return `Forbidden` where the contract wants `Unauthenticated`
//! and your 401s become 403s; return an `Err` from `authenticate` and you leak
//! which credential was tried).
//!
//! So the plugin ships generators. They're plugin commands — the same
//! `Plugin::commands()` hook a third-party plugin has — so `umbral-rest` gets
//! no special treatment, which is the contract working as designed.
//!
//! ```bash
//! cargo run -- startpermission IsOwner
//! cargo run -- startauthentication ApiKeyAuth --in blog
//! ```
//!
//! [`Authentication`]: umbral::auth::Authentication
//! [`Permission`]: crate::permission::Permission
//! [`Pagination`]: crate::pagination::Pagination
//! [`Throttle`]: crate::throttle::Throttle

use std::path::{Path, PathBuf};

use umbral::cli::{CliError, PluginCommand, clap};
use umbral::codegen::{
    CodegenError, Scaffolded, Target, declare_module, ensure_dependency, insert_before_marker,
    pascal_case_from_ident, prompt, resolve_target, to_snake_case, validate_ident, write_new_file,
};

/// Marker the generator inserts new module declarations above.
const MODS_MARKER: &str = "// umbral:rest — new modules are declared above this line.";
/// Marker the generator inserts new re-exports above.
const EXPORTS_MARKER: &str = "// umbral:rest — new re-exports go above this line.";

/// The four REST extension points, each with a generator behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Permission,
    Authentication,
    Pagination,
    Throttle,
}

impl Class {
    /// Every generator the plugin contributes.
    pub const ALL: [Class; 4] = [
        Class::Permission,
        Class::Authentication,
        Class::Pagination,
        Class::Throttle,
    ];

    /// The subcommand: `cargo run -- startpermission IsOwner`.
    pub fn command_name(&self) -> &'static str {
        match self {
            Self::Permission => "startpermission",
            Self::Authentication => "startauthentication",
            Self::Pagination => "startpagination",
            Self::Throttle => "startthrottle",
        }
    }

    /// The directory the class lands in, and the module it's declared as.
    pub fn dir(&self) -> &'static str {
        match self {
            Self::Permission => "permissions",
            Self::Authentication => "authentication",
            Self::Pagination => "pagination",
            Self::Throttle => "throttles",
        }
    }

    fn about(&self) -> &'static str {
        match self {
            Self::Permission => "Scaffold a REST permission class (who may do what)",
            Self::Authentication => "Scaffold a REST authentication class (who is the caller)",
            Self::Pagination => "Scaffold a REST pagination class (how list responses are sliced)",
            Self::Throttle => "Scaffold a REST throttle class (how often a caller may ask)",
        }
    }

    /// The name used in `--help` so the example reads like real code.
    fn example(&self) -> &'static str {
        match self {
            Self::Permission => "IsOwner",
            Self::Authentication => "ApiKeyAuth",
            Self::Pagination => "CursorPagination",
            Self::Throttle => "BurstThrottle",
        }
    }

    /// The builder line(s) that put the class to work. The generator can't
    /// write these: only you know which resource a permission guards, and a
    /// `main.rs` builder chain is not a thing to rewrite by regex.
    fn registration(&self, pascal: &str, dir: &str, is_root: bool) -> Vec<String> {
        let import = if is_root {
            format!("use crate::{dir}::{pascal};")
        } else {
            format!("use {dir}::{pascal};   // from this plugin's crate")
        };
        // How the class is CONSTRUCTED, which is not always its bare name: a
        // throttle carries its rate limiters, so it has a `new()`. Printing
        // `.default_throttle(BurstThrottle)` for it would hand the user a line
        // that doesn't compile — and a "next step" that doesn't work is worse
        // than none, because they'll trust it before they read it.
        let pascal = match self {
            Self::Throttle => format!("{pascal}::new()"),
            _ => pascal.to_string(),
        };
        let pascal = pascal.as_str();
        let mut steps = vec![
            "Wire it into the RestPlugin in your App builder:".to_string(),
            format!("    {import}"),
        ];
        match self {
            Self::Permission => {
                steps.push(format!(
                    "    RestPlugin::default().default_permission({pascal})       // every resource"
                ));
                steps.push(format!(
                    "    ResourceConfig::new(\"post\").permission({pascal})        // just this one"
                ));
            }
            Self::Authentication => {
                steps.push(format!(
                    "    RestPlugin::default().authenticate({pascal})             // one backend"
                ));
                steps.push(
                    "    ChainAuthentication::new(vec![...])                    // try several in order"
                        .to_string(),
                );
            }
            Self::Pagination => {
                steps.push(format!(
                    "    RestPlugin::default().paginate({pascal})                 // every list endpoint"
                ));
            }
            Self::Throttle => {
                steps.push(format!(
                    "    RestPlugin::default().default_throttle({pascal})         // every resource"
                ));
                steps.push(format!(
                    "    ResourceConfig::new(\"post\").throttle({pascal})          // just this one"
                ));
            }
        }
        steps
    }
}

/// Write a REST class and declare it, returning what was written.
///
/// Pure apart from the filesystem: give it a project root and it does the same
/// thing whether a human or a test called it.
pub fn scaffold_class(
    class: Class,
    name: &str,
    target: &Target,
    project_root: &Path,
) -> Result<Scaffolded, CodegenError> {
    validate_ident(name)?;
    // Either input form lands in the same place: `IsOwner` and `is_owner` both
    // give the struct `IsOwner` in the module `is_owner`.
    let pascal = pascal_case_from_ident(name);
    let module = to_snake_case(&pascal);
    let dir = class.dir();

    let resolved = resolve_target(project_root, target)?;
    let mut files = Vec::new();
    let mut next_steps = Vec::new();

    // 1. The class itself. Refuses to overwrite (write_new_file), so a typo'd
    //    re-run can't eat the impl you spent an hour on.
    write_new_file(
        &resolved.crate_root,
        &format!("src/{dir}/{module}.rs"),
        &render_class(class, &pascal, resolved.is_root),
        &mut files,
    )?;

    // 2. The `mod.rs`: created on the first class of this kind, appended to on
    //    every one after.
    let mod_rel = format!("src/{dir}/mod.rs");
    let mod_path = resolved.crate_root.join(&mod_rel);
    if mod_path.is_file() {
        let text = std::fs::read_to_string(&mod_path)?;
        let updated = insert_before_marker(&text, MODS_MARKER, &format!("pub mod {module};"))
            .and_then(|t| {
                insert_before_marker(&t, EXPORTS_MARKER, &format!("pub use {module}::{pascal};"))
            });
        match updated {
            Some(text) => {
                std::fs::write(&mod_path, text)?;
                files.push(PathBuf::from(&mod_rel));
            }
            None => {
                // Markers gone — the file has been restructured and we don't
                // recognise it. Say what to add; don't rewrite what we can't read.
                next_steps.push(format!(
                    "`{mod_rel}` has no `umbral:rest` markers — add by hand:"
                ));
                next_steps.push(format!("    pub mod {module};"));
                next_steps.push(format!("    pub use {module}::{pascal};"));
            }
        }
    } else {
        write_new_file(
            &resolved.crate_root,
            &mod_rel,
            &render_mod(class, &module, &pascal),
            &mut files,
        )?;
    }

    // 3. Declare the directory as a module of the crate that now owns it.
    let owner_text = std::fs::read_to_string(&resolved.owner_file)?;
    let decl = resolved.module_decl(dir);
    match declare_module(&owner_text, &decl) {
        Some(text) => std::fs::write(&resolved.owner_file, text)?,
        None => {
            // Already declared (the second class of this kind) — or the file
            // declares no modules at all, in which case we say so.
            if !owner_text.lines().any(|l| l.trim() == decl) {
                next_steps.push(format!(
                    "Add to {}:  {decl}",
                    resolved
                        .owner_file
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
            }
        }
    }

    // 4. A class scaffolded into a plugin needs that plugin to depend on
    //    umbral-rest, or it doesn't compile. The project root already does
    //    (startproject wires it), so only a plugin target needs the check.
    if !resolved.is_root {
        let manifest = resolved.crate_root.join("Cargo.toml");
        let version = env!("CARGO_PKG_VERSION");
        match ensure_dependency(&manifest, "umbral-rest", &format!("\"{version}\"")) {
            Ok(true) => next_steps.push(format!(
                "Added `umbral-rest = \"{version}\"` to {}. If your project path-deps \
                 umbral, point it at your checkout instead.",
                manifest.display()
            )),
            Ok(false) => {}
            Err(e) => next_steps.push(format!(
                "Could not add umbral-rest to {}: {e}. Add it by hand or the new file \
                 won't compile.",
                manifest.display()
            )),
        }
    }

    next_steps.extend(class.registration(&pascal, dir, resolved.is_root));

    Ok(Scaffolded {
        root: resolved.crate_root,
        files,
        next_steps,
    })
}

/// The generated `<dir>/mod.rs` — module list + re-exports, both marker-driven
/// so the next class of the same kind appends cleanly.
fn render_mod(class: Class, module: &str, pascal: &str) -> String {
    let (what, dir) = (class.what(), class.dir());
    format!(
        r#"//! Custom REST {what} classes.
//!
//! One file per class; re-exported here so the App builder can reach them as
//! `{dir}::{pascal}`. `umbral {cmd} <Name>` appends to both lists below —
//! that's what the marker comments are for. Editing them by hand is fine too.

pub mod {module};
{MODS_MARKER}

pub use {module}::{pascal};
{EXPORTS_MARKER}
"#,
        cmd = class.command_name(),
    )
}

impl Class {
    /// The word that reads naturally in "custom REST ___ classes".
    fn what(&self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Authentication => "authentication",
            Self::Pagination => "pagination",
            Self::Throttle => "throttle",
        }
    }
}

/// The generated class file. Every one is a *working* implementation with a
/// sane default behaviour — not a `todo!()` — because a stub that compiles and
/// denies everything teaches you nothing about the contract you're
/// implementing.
fn render_class(class: Class, pascal: &str, is_root: bool) -> String {
    // A class in a plugin reaches its own models via `crate::models`; one at the
    // project root reaches them via `crate`.
    let models = if is_root {
        "crate::{Post, post}"
    } else {
        "crate::models::{Post, post}"
    };
    match class {
        Class::Permission => render_permission(pascal, models),
        Class::Authentication => render_authentication(pascal, models),
        Class::Pagination => render_pagination(pascal),
        Class::Throttle => render_throttle(pascal),
    }
}

fn render_permission(pascal: &str, models: &str) -> String {
    format!(
        r#"//! `{pascal}` — a REST permission class: who may do what.
//!
//! A permission answers a question about the ACTION, not about a row: "may
//! this caller create a post?", not "may they edit *post 7*?". It runs before
//! the row is fetched, so there is no row to consult.
//!
//! **Row-level ownership is a different tool.** `ResourceConfig::owned_by("author")`
//! scopes every query to the caller's own rows — a filter on the SQL, not a
//! check after the fact, so somebody else's row is not merely forbidden, it is
//! invisible. Reach for that when you mean "only your own"; reach for a
//! permission class when you mean "only staff", "only during business hours",
//! "only if the caller's plan allows it".

use umbral::auth::Identity;
use umbral_rest::permission::{{Action, Permission, PermissionError}};

/// Read for anyone signed in; write for staff. Rewrite `check` to taste.
pub struct {pascal};

impl Permission for {pascal} {{
    fn check(&self, action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {{
        // Anonymous → 401, NOT 403. The distinction is the contract:
        // `Unauthenticated` tells the client "log in and try again";
        // `Forbidden` tells it "you are logged in and the answer is still no".
        // Collapsing the two is how a client ends up with no way to recover.
        let Some(identity) = identity else {{
            return Err(PermissionError::Unauthenticated);
        }};

        // A superuser bypasses the rest. The built-in classes do the same;
        // drop this line if you want a permission nobody can escape.
        if identity.is_superuser {{
            return Ok(());
        }}

        match action {{
            // Reads: any authenticated caller.
            Action::List | Action::Retrieve => Ok(()),

            // Writes: staff only.
            Action::Create | Action::Update | Action::Delete => {{
                if identity.is_staff {{
                    Ok(())
                }} else {{
                    Err(PermissionError::Forbidden)
                }}
            }}

            // `@action` endpoints (`ResourceConfig::action("publish", ...)`).
            // Match the ones you know; deny what you don't, so a new action
            // added next year is closed until you decide otherwise.
            Action::Custom(name) => match name.as_str() {{
                "publish" if identity.is_staff => Ok(()),
                _ => Err(PermissionError::Forbidden),
            }},
        }}
    }}
}}

// The check is SYNC on purpose: it walks an in-memory identity, it doesn't hit
// the database. If you find yourself wanting a query here, you probably want
// `owned_by` (scoping) or an extra field on `Identity::extras` (populated once,
// at authentication time) rather than a query per request.
//
// Models, if you need them for a compile-time reference:
//   use {models};
"#
    )
}

fn render_authentication(pascal: &str, models: &str) -> String {
    format!(
        r#"//! `{pascal}` — a REST authentication class: who is the caller?
//!
//! Look at the request headers and return `Some(Identity)` if you recognise
//! the caller, `None` if you don't. That's the whole contract. Permissions
//! ([`umbral_rest::permission`]) then decide what that identity may do.

use umbral::auth::{{Authentication, Identity}};
use umbral::web::HeaderMap;

/// Bearer-token authentication. Swap the lookup for whatever you actually
/// issue — an API key table, a JWT, an HMAC signature.
pub struct {pascal};

#[umbral::async_trait]
impl Authentication for {pascal} {{
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {{
        // Note what this does NOT do: return an error. An invalid token, a
        // missing header and a malformed one all mean the same thing here —
        // "I don't know who this is" — and answering anything more specific
        // tells an attacker which of their guesses was closer. The permission
        // check turns the resulting anonymity into a 401.
        let raw = headers.get("authorization")?.to_str().ok()?;
        let token = raw.strip_prefix("Bearer ")?.trim();
        if token.is_empty() {{
            return None;
        }}

        // Look the token up. Through the ORM — the pool is ambient here, and a
        // hand-written `sqlx::query` would work on SQLite and break on Postgres:
        //
        //     use {models};
        //
        //     let user = ApiKey::objects()
        //         .filter(api_key::TOKEN.eq(token))
        //         .select_related(api_key::USER)
        //         .first()
        //         .await
        //         .ok()??;
        //
        //     return Some(Identity {{
        //         user_id: user.id.to_string(),
        //         is_staff: user.is_staff,
        //         is_superuser: user.is_superuser,
        //         extras: Default::default(),
        //     }});
        //
        // Compare secrets in constant time (`subtle`/`ring`) if you're matching
        // a token by value — a plain `==` on a secret leaks its prefix through
        // timing.
        None
    }}

    /// What `umbral-openapi` publishes under `securitySchemes`, so the Swagger
    /// UI shows an Authorize button that actually works. Delete if you don't
    /// serve an OpenAPI spec.
    fn security_scheme(&self) -> Option<(String, umbral_rest::serde_json::Value)> {{
        Some((
            "{pascal}".to_string(),
            umbral_rest::serde_json::json!({{
                "type": "http",
                "scheme": "bearer",
            }}),
        ))
    }}
}}
"#
    )
}

fn render_pagination(pascal: &str) -> String {
    format!(
        r#"//! `{pascal}` — a REST pagination class: how a list response is sliced.
//!
//! Two halves. `extract_request` reads the query params and says which slice
//! of rows to fetch; `paginate` wraps the fetched rows in the envelope the
//! client sees. The framework does the fetching in between.

use std::collections::HashMap;

use umbral_rest::pagination::{{
    PageRequest, Pagination, PaginationField, PaginationScalar, PaginationSchema, PaginationStyle,
}};
use umbral_rest::serde_json::{{Map, Value, json}};

/// Offset pagination with a hard page-size cap.
///
/// `?limit=50&offset=100` → `{{ "results": [...], "count": 1234, "next_offset": 150 }}`
pub struct {pascal};

/// Rows per page when the caller doesn't say.
const DEFAULT_LIMIT: u64 = 25;
/// The ceiling. Without one, `?limit=100000000` is a denial-of-service request
/// that your own API cheerfully serves.
const MAX_LIMIT: u64 = 100;

impl Pagination for {pascal} {{
    fn extract_request(&self, params: &HashMap<String, String>) -> PageRequest {{
        // Never error out of here. A typo'd `?limt=10` is not worth a 400 —
        // fall back to the default and serve the request. (The contract says so
        // explicitly, and the built-ins all behave this way.)
        let limit = params
            .get("limit")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let offset = params
            .get("offset")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        PageRequest {{
            limit,
            offset,
            page: None,
        }}
    }}

    fn paginate(&self, rows: Vec<Map<String, Value>>, total_rows: i64, req: &PageRequest) -> Value {{
        let next_offset = req.offset + req.limit;
        let has_next = (next_offset as i64) < total_rows;

        json!({{
            "results": rows,
            "count": total_rows,
            "next_offset": if has_next {{ Some(next_offset) }} else {{ None }},
        }})
    }}

    /// `true` (the default) costs one extra `SELECT COUNT(*)` per list request,
    /// which is what `count` above is made of. Return `false` if your envelope
    /// doesn't need a total — cursor pagination usually doesn't — and the
    /// framework skips the round-trip.
    fn needs_total(&self) -> bool {{
        true
    }}

    fn style(&self) -> PaginationStyle {{
        // `Custom` tells codegen "don't assume you know my query params".
        // The schema below is how it learns them anyway.
        PaginationStyle::Custom
    }}

    /// Declare the wire shape so the OpenAPI spec and the generated TypeScript
    /// client come out TYPED rather than as an opaque escape hatch. Skip this
    /// and the client still works — it just hands your callers `unknown` and a
    /// generic `.param(...)`, which is how a typed client stops being one.
    fn schema(&self) -> Option<PaginationSchema> {{
        Some(PaginationSchema {{
            // `results: T[]` is implicit — list the keys BESIDE it.
            envelope: vec![
                PaginationField::new("count", PaginationScalar::Number),
                PaginationField::nullable("next_offset", PaginationScalar::Number),
            ],
            params: vec![
                PaginationField::new("limit", PaginationScalar::Number),
                PaginationField::new("offset", PaginationScalar::Number),
            ],
        }})
    }}
}}
"#
    )
}

fn render_throttle(pascal: &str) -> String {
    format!(
        r#"//! `{pascal}` — a REST throttle class: how often a caller may ask.
//!
//! `Ok(())` allows the request; `Err(ThrottleDenied)` turns into a 429, with
//! the `retry_after` you supply rendered as the `Retry-After` header. Give the
//! client a number and it can back off politely; give it nothing and it will
//! hammer you.

use std::time::Duration;

use umbral::ratelimit::{{Rate, RateLimiter}};
use umbral_rest::throttle::{{Throttle, ThrottleContext, ThrottleDenied}};

/// A stricter limit for anonymous callers than for signed-in ones — the shape
/// most APIs actually want, and the one the built-ins can't express as a pair.
pub struct {pascal} {{
    anon: RateLimiter,
    user: RateLimiter,
}}

impl Default for {pascal} {{
    fn default() -> Self {{
        Self::new()
    }}
}}

impl {pascal} {{
    pub fn new() -> Self {{
        // Rate strings: "<count>/<second|minute|hour|day>". `expect` is right
        // here and only here — the literal is yours, so a bad one is a bug you
        // want to hear about at boot, not a 500 at 3am. A rate read from
        // config gets a `?`, not an `expect`.
        Self {{
            anon: RateLimiter::new(Rate::parse("30/minute").expect("valid rate literal")),
            user: RateLimiter::new(Rate::parse("300/minute").expect("valid rate literal")),
        }}
    }}
}}

impl Throttle for {pascal} {{
    fn check(&self, ctx: &ThrottleContext) -> Result<(), ThrottleDenied> {{
        // Key the bucket by WHO, not by what. An authenticated caller is keyed
        // by user id so they carry their limit across IPs; an anonymous one is
        // keyed by IP because that's all we have. Keying everyone by IP means
        // an office behind one NAT shares a single bucket.
        let (limiter, key) = match ctx.identity {{
            Some(identity) => (&self.user, format!("user:{{}}", identity.user_id)),
            None => (
                &self.anon,
                format!("anon:{{}}", ctx.client_ip.unwrap_or("unknown")),
            ),
        }};

        let decision = limiter.check(&key);
        if decision.allowed {{
            Ok(())
        }} else {{
            Err(ThrottleDenied {{
                // Hand back the limiter's own hint. `None` here would drop the
                // Retry-After header and leave the client guessing.
                retry_after: decision.retry_after.or(Some(Duration::from_secs(60))),
            }})
        }}
    }}
}}

// `ctx.scope` carries a "<resource>:<action>" label (e.g. "post:create") if you
// want different limits per endpoint — that's what the built-in
// `ScopedRateThrottle` keys on.
"#
    )
}

// =========================================================================
// The commands
// =========================================================================

/// One generator, bound to the class it writes.
struct ClassCommand(Class);

#[umbral::async_trait]
impl PluginCommand for ClassCommand {
    fn command(&self) -> clap::Command {
        let class = self.0;
        clap::Command::new(class.command_name())
            .about(class.about())
            .long_about(format!(
                "{about}\n\nWrites src/{dir}/<name>.rs, re-exports it from src/{dir}/mod.rs, \
                 and declares the module. Prints the builder line that puts it to work — \
                 which it can't write for you, since only you know which resource it guards.\n\n\
                 Example:\n    cargo run -- {cmd} {example}",
                about = class.about(),
                dir = class.dir(),
                cmd = class.command_name(),
                example = class.example(),
            ))
            .arg(
                clap::Arg::new("name")
                    .required(false)
                    .help("Struct name, PascalCase or snake_case — `IsOwner` and `is_owner` land in the same place. Prompted for if omitted."),
            )
            .arg(
                clap::Arg::new("in")
                    .long("in")
                    .value_name("root|PLUGIN")
                    .help("Where it lives: `root` (this project) or a plugin under plugins/. Prompted for if omitted."),
            )
            .arg(
                clap::Arg::new("path")
                    .long("path")
                    .value_name("DIR")
                    .default_value(".")
                    .help("Project root. Defaults to the working directory."),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), CliError> {
        let class = self.0;
        let project_root = PathBuf::from(
            matches
                .get_one::<String>("path")
                .map(String::as_str)
                .unwrap_or("."),
        );

        // Ask for anything not passed — but only when there's a human to ask.
        // Prompting a pipe hangs a CI job on a question nothing will answer.
        let interactive = prompt::is_interactive();

        let name = match matches.get_one::<String>("name") {
            Some(n) => n.clone(),
            None if interactive => prompt::ask_required(&format!(
                "{} name (e.g. {}): ",
                class.what(),
                class.example()
            ))?,
            None => {
                return Err(format!(
                    "a name is required when stdin isn't a terminal: \
                     `{} <NAME> --in root`",
                    class.command_name()
                )
                .into());
            }
        };

        let target = match matches.get_one::<String>("in") {
            Some(t) => Target::parse(t),
            None if interactive => prompt::ask_target(&project_root)?,
            None => Target::Root,
        };

        let report = scaffold_class(class, &name, &target, &project_root)?;

        println!("Created in `{}`:", report.root.display());
        for f in &report.files {
            println!("  {}", f.display());
        }
        println!();
        println!("Next steps:");
        for step in &report.next_steps {
            println!("  {step}");
        }
        Ok(())
    }
}

/// Every generator the REST plugin contributes — returned from
/// `Plugin::commands()`.
pub(crate) fn all() -> Vec<Box<dyn PluginCommand>> {
    Class::ALL
        .iter()
        .map(|c| Box::new(ClassCommand(*c)) as Box<dyn PluginCommand>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project shaped like `umbral startproject` leaves it — enough of one
    /// for the generator's file surgery to be exercised against the real thing.
    fn project(tmp: &tempfile::TempDir) -> PathBuf {
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "//! demo\n\nmod seed;\nmod views;\n\nuse umbral::prelude::*;\n\nfn main() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\numbral = \"0.0.9\"\numbral-rest = \"0.0.9\"\n",
        )
        .unwrap();
        root
    }

    fn plugin(root: &Path, name: &str) {
        let p = root.join("plugins").join(name);
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname = \"blog\"\n\n[dependencies]\numbral = \"0.0.9\"\n",
        )
        .unwrap();
        std::fs::write(
            p.join("src/lib.rs"),
            "//! blog\n\npub mod models;\npub mod views;\n\npub struct BlogPlugin;\n",
        )
        .unwrap();
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    #[test]
    fn every_class_writes_its_file_and_declares_its_module() {
        for class in Class::ALL {
            let tmp = tempfile::tempdir().expect("tempdir");
            let root = project(&tmp);

            let report = scaffold_class(class, "MyClass", &Target::Root, &root)
                .unwrap_or_else(|e| panic!("{}: {e}", class.command_name()));

            let dir = class.dir();
            assert!(
                report
                    .files
                    .contains(&PathBuf::from(format!("src/{dir}/my_class.rs"))),
                "{}: class file missing from the report",
                class.command_name()
            );

            let file = read(&root, &format!("src/{dir}/my_class.rs"));
            assert!(
                file.contains("pub struct MyClass"),
                "{}: no struct in {file}",
                class.command_name()
            );

            let mod_rs = read(&root, &format!("src/{dir}/mod.rs"));
            assert!(mod_rs.contains("pub mod my_class;"), "{mod_rs}");
            assert!(mod_rs.contains("pub use my_class::MyClass;"), "{mod_rs}");

            let main_rs = read(&root, "src/main.rs");
            assert!(
                main_rs.contains(&format!("mod {dir};")),
                "{}: main.rs never declared the module: {main_rs}",
                class.command_name()
            );

            // The registration line the user has to place is spelled out.
            let steps = report.next_steps.join("\n");
            assert!(
                steps.contains("MyClass"),
                "{}: no registration hint: {steps}",
                class.command_name()
            );
        }
    }

    /// `IsOwner` and `is_owner` must land on the same file and the same struct.
    /// Somebody will type each.
    #[test]
    fn either_casing_lands_in_the_same_place() {
        for input in ["IsOwner", "is_owner"] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let root = project(&tmp);
            scaffold_class(Class::Permission, input, &Target::Root, &root).expect("scaffold");
            let file = read(&root, "src/permissions/is_owner.rs");
            assert!(
                file.contains("pub struct IsOwner;"),
                "from `{input}`: {file}"
            );
        }
    }

    #[test]
    fn a_second_class_appends_to_the_same_mod_rs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);

        scaffold_class(Class::Permission, "IsOwner", &Target::Root, &root).expect("first");
        let main_after_first = read(&root, "src/main.rs");
        scaffold_class(Class::Permission, "IsStaffOrReadOnly", &Target::Root, &root)
            .expect("second");

        let mod_rs = read(&root, "src/permissions/mod.rs");
        assert!(mod_rs.contains("pub mod is_owner;"), "{mod_rs}");
        assert!(
            mod_rs.contains("pub mod is_staff_or_read_only;"),
            "{mod_rs}"
        );
        assert!(mod_rs.contains("pub use is_owner::IsOwner;"), "{mod_rs}");
        assert!(
            mod_rs.contains("pub use is_staff_or_read_only::IsStaffOrReadOnly;"),
            "{mod_rs}"
        );

        // main.rs was declared once and never touched again.
        assert_eq!(
            read(&root, "src/main.rs"),
            main_after_first,
            "the second class edited main.rs again"
        );
        assert_eq!(
            read(&root, "src/main.rs")
                .matches("mod permissions;")
                .count(),
            1
        );
    }

    /// A class in a plugin needs that plugin to depend on umbral-rest, or the
    /// generator has handed the user a crate that doesn't compile.
    #[test]
    fn a_class_in_a_plugin_adds_the_rest_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        plugin(&root, "blog");

        scaffold_class(
            Class::Permission,
            "IsOwner",
            &Target::Plugin("blog".into()),
            &root,
        )
        .expect("scaffold");

        let manifest = read(&root, "plugins/blog/Cargo.toml");
        assert!(
            manifest.contains("umbral-rest ="),
            "the plugin never got the umbral-rest dep: {manifest}"
        );
        let lib_rs = read(&root, "plugins/blog/src/lib.rs");
        assert!(
            lib_rs.contains("pub mod permissions;"),
            "a plugin's module must be `pub mod`, not `mod`: {lib_rs}"
        );
        assert!(
            root.join("plugins/blog/src/permissions/is_owner.rs")
                .is_file()
        );
    }

    #[test]
    fn an_existing_class_is_never_overwritten() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        scaffold_class(Class::Permission, "IsOwner", &Target::Root, &root).expect("first");
        std::fs::write(root.join("src/permissions/is_owner.rs"), "// my work\n").unwrap();

        let err = scaffold_class(Class::Permission, "IsOwner", &Target::Root, &root)
            .expect_err("must refuse");
        assert!(matches!(err, CodegenError::AlreadyExists(_)));
        assert_eq!(
            read(&root, "src/permissions/is_owner.rs"),
            "// my work\n",
            "the generator clobbered an existing file"
        );
    }

    #[test]
    fn an_unknown_plugin_is_rejected_with_the_real_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        plugin(&root, "blog");

        match scaffold_class(
            Class::Throttle,
            "BurstThrottle",
            &Target::Plugin("blgo".into()),
            &root,
        ) {
            Err(CodegenError::NoSuchPlugin { asked, available }) => {
                assert_eq!(asked, "blgo");
                assert_eq!(available, vec!["blog".to_string()]);
            }
            other => panic!("expected NoSuchPlugin, got {other:?}"),
        }
    }

    #[test]
    fn the_four_commands_are_named_and_described() {
        let cmds = all();
        assert_eq!(cmds.len(), 4);
        let names: Vec<String> = cmds
            .iter()
            .map(|c| c.command().get_name().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "startpermission",
                "startauthentication",
                "startpagination",
                "startthrottle"
            ]
        );
        for cmd in &cmds {
            assert!(
                cmd.command().get_about().is_some(),
                "a command with no `about` lists as a dash and nobody finds it"
            );
        }
    }
}
