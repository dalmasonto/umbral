//! Derive and attribute macros for umbra: `#[derive(Model)]`, `#[task]`, etc.
//!
//! Do not depend on this crate directly. Use the `umbra` facade, which
//! re-exports the derives so user code only ever imports `umbra`.
//!
//! Status: M3 ships `#[derive(Model)]` this milestone; more derives land
//! as their milestones do. See `docs/specs/04-orm-model-and-fields.md`
//! for the target shape. What M2's hand-written `impl Model for Post`
//! looks like is exactly what the derive emits.
//!
//! # Examples
//!
//! The minimum-viable derive. A struct of plain fields turns into a real
//! `Model` impl: table name from the snake_case of the struct, one
//! `FieldSpec` per field.
//!
//! ```
//! use umbra::orm::Model;
//!
//! #[derive(Debug, sqlx::FromRow, umbra::orm::Model)]
//! struct Comment {
//!     id: i64,
//!     body: String,
//! }
//!
//! fn main() {
//!     // The derive emits the TABLE constant from the snake_case struct name.
//!     assert_eq!(<Comment as Model>::TABLE, "comment");
//!     assert_eq!(Comment::FIELDS.len(), 2);
//! }
//! ```
//!
//! The sibling column module and the `objects()` entry point. The derive
//! emits a module named after the table, with one typed column constant
//! per field, and an inherent `objects()` returning a `Manager<Self>`.
//!
//! ```
//! use umbra::orm::Model;
//!
//! #[derive(Debug, sqlx::FromRow, umbra::orm::Model)]
//! struct Comment {
//!     id: i64,
//!     body: String,
//! }
//!
//! fn main() {
//!     // The derive emits a sibling `comment` module with typed column
//!     // constants used in predicates.
//!     let _predicate = comment::BODY.eq("hello");
//!
//!     // It also emits `Comment::objects()` returning a `Manager<Comment>`.
//!     // Constructing a Manager doesn't touch the database.
//!     let _manager = Comment::objects();
//! }
//! ```
//!
//! Nullable handling. `Option<chrono::DateTime<chrono::Utc>>` becomes a
//! `NullableDateTimeCol`, which is the only column type exposing
//! `is_null` / `is_not_null`. The mapping is one-to-one with the field
//! type catalogue in spec 04.
//!
//! ```
//! use umbra::orm::Model;
//!
//! #[derive(Debug, sqlx::FromRow, umbra::orm::Model)]
//! struct Article {
//!     id: i64,
//!     title: String,
//!     body: String,
//!     published_at: Option<chrono::DateTime<chrono::Utc>>,
//! }
//!
//! fn main() {
//!     // The Option<DateTime<Utc>> field becomes a NullableDateTimeCol so
//!     // is_null / is_not_null are available in predicates.
//!     let _draft_predicate = article::PUBLISHED_AT.is_null();
//!     assert_eq!(<Article as Model>::TABLE, "article");
//! }
//! ```

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Field, Fields, GenericArgument, ItemFn, PathArguments, ReturnType, Type,
    TypePath, parse_macro_input,
};

/// Generate `impl Model` for a struct.
///
/// Emits the trait impl, the sibling column module, and an inherent
/// `objects()` entry point. The emitted shape matches M2's hand-written
/// `impl Model for Post` in `umbra-core::orm::post`, but uses `::umbra`
/// facade paths so the derive works from any user crate.
///
/// M3 constraints (relaxed at later milestones):
///
/// - The struct must have a field named `id`. The primary key type may
///   be `i32`, `i64`, or `uuid::Uuid`; spec 04 §4.2.
/// - The supported field types follow the M3 catalogue: signed and small
///   unsigned ints, `f32` / `f64`, `bool`, `String`, `chrono::NaiveDate`,
///   `chrono::NaiveTime`, `chrono::DateTime<chrono::Utc>`, `uuid::Uuid`,
///   plus the `Option<T>` of each.
/// - One struct-level `#[umbra(table = "...")]` attribute is
///   accepted at M3.1, used to override the default snake_case-of-
///   struct-name table name. Other attributes (per-field
///   `max_length`, `db_index`, `default`, `choices`, `on_delete`)
///   land as plugin authors need each. Foreign derive attributes
///   (`#[serde(...)]`, `#[sqlx(...)]`, …) are ignored.
#[proc_macro_derive(Model, attributes(umbra))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_model(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Parse the struct-level `#[umbra(...)]` attribute. M3.1 ships
/// `table = "..."` to override the default snake_case-of-struct-name
/// table name. Gap 30 adds `plugin = "..."` so a plugin-owned model
/// opts into a `<plugin>_<table>` namespaced table name, preventing
/// collisions when two plugins each declare a model with the same
/// struct name (e.g. two `Post` models from different plugins).
/// Gap 44 adds `display = "..."` (human-readable label for the admin
/// sidebar) and `icon = "..."` (Lucide icon slug).
///
/// Precedence: `table` > `plugin` > bare snake_case. An explicit
/// `table = "..."` always wins regardless of whether `plugin = "..."`
/// is also present.
struct UmbraStructAttr {
    table: Option<String>,
    plugin: Option<String>,
    display: Option<String>,
    icon: Option<String>,
    database: Option<String>,
    /// `#[umbra(singleton)]` — single-row model marker.
    /// Closes BUG-9 from bugs/tests/testBugs.md.
    singleton: bool,
    /// Feature #72 — `#[umbra(soft_delete)]`. Set when the model
    /// opts into soft-delete semantics. Emitted as
    /// `Model::SOFT_DELETE = true`; the framework's QuerySet /
    /// Manager paths read this const to inject `WHERE deleted_at
    /// IS NULL` and rewrite delete operations as updates. The
    /// user must declare `pub deleted_at: Option<DateTime<Utc>>`
    /// on the struct themselves (derive macros can't add fields).
    soft_delete: bool,
    /// `#[umbra(unique_together = [["a", "b"], ["c"]])]` — composite
    /// UNIQUE constraints. Each inner array names a constraint over the
    /// listed column names. Closes BUG-6.
    unique_together: Vec<Vec<String>>,
    /// `#[umbra(indexes = [["a", "b"], ["status"]])]` — multi-column
    /// indexes (single-column already covered by field-level
    /// `#[umbra(index)]`). Closes BUG-7.
    indexes: Vec<Vec<String>>,
    /// `#[umbra(ordering = ["-published_at", "id"])]` — default
    /// `ORDER BY`. A leading `-` flips the direction to DESC.
    /// Closes BUG-8. Stored as `(column, direction)` pairs.
    ordering: Vec<(String, bool)>,
}

/// Field-level `#[umbra(...)]` attribute parsed from a struct field.
#[derive(Default)]
struct UmbraFieldAttr {
    /// `#[umbra(noform)]` — never show on any form.
    noform: bool,
    /// `#[umbra(noedit)]` — show on edit form read-only, never on create.
    noedit: bool,
    /// `#[umbra(primary_key)]` — explicitly nominate this field as the
    /// model's primary key. Used when the PK field isn't named `id`
    /// (the macro's historical convention). Example: a `Permission`
    /// model keyed by a string codename uses
    /// `#[umbra(primary_key)] pub codename: String` instead of
    /// `pub id: String`.
    primary_key: bool,
    /// `#[umbra(no_reverse)]` — suppress the Gap-30 reverse-FK
    /// accessor (`<child>_set` on the parent type) for this FK.
    /// Required when the FK target is defined in another crate
    /// (e.g. `ForeignKey<AuthUser>` from `umbra-auth`), because
    /// Rust's orphan rules forbid emitting an inherent `impl
    /// AuthUser { ... }` from the child's crate. Without this
    /// opt-out, the Model derive produces an E0116. No-ops on
    /// non-FK fields and on `Option<ForeignKey<_>>` (nullable FKs
    /// already skip reverse-set generation).
    no_reverse: bool,
    /// `#[umbra(string)]` — Django-style "__str__" marker. The admin's
    /// default `list_display` falls back to this field when the model
    /// has no explicit `list_display` config, so the table shows a
    /// human label instead of every column. Only meaningful on
    /// `String`-typed fields.
    is_string_repr: bool,
    /// `#[umbra(max_length = N)]` — soft limit. The admin truncates
    /// the value at this many characters in `list_display` so a long
    /// body doesn't blow out a column. `0` means no truncation.
    max_length: u32,
    /// `#[umbra(choices)]` — the field's type implements
    /// [`umbra::orm::ChoiceField`]. The Model derive emits the field
    /// as `SqlType::Text` and pulls the variant list from the trait
    /// at derive time. Stored as `Some(TypeTokens)` for the choices
    /// type when set, so the emitted FieldSpec can reference
    /// `<T as ChoiceField>::VALUES`.
    choices_ty: Option<proc_macro2::TokenStream>,
    /// `#[umbra(default = "...")]` — SQL `DEFAULT` clause for this
    /// column. Accepts a string literal; the migration engine emits
    /// it verbatim on `CREATE TABLE` and `ALTER TABLE ADD COLUMN`.
    /// `None` means no default.
    default: Option<String>,
    /// `#[umbra(unique)]` — emit a column-level `UNIQUE` constraint
    /// at `CREATE TABLE` time. Closes gap #65. v1 scope: new
    /// tables only; toggling on an existing column doesn't auto-
    /// migrate (the diff engine watches type and nullable, not
    /// constraint flags).
    unique: bool,
    /// `#[umbra(on_delete = "cascade" | "restrict" | "set_null" |
    /// "no_action")]` — emit `ON DELETE <action>` in the
    /// `REFERENCES ...` tail. FK columns only. Closes gap #68.
    /// Stored as the lowercase string supplied; the FieldSpec
    /// emitter parses it into `umbra::orm::FkAction` at codegen
    /// time and rejects unknown values with a compile error.
    on_delete: Option<String>,
    /// `#[umbra(on_update = "...")]` — emit `ON UPDATE <action>`.
    /// Same vocabulary as `on_delete`.
    on_update: Option<String>,
    /// `#[umbra(index)]` — single-column index. Closes BUG-4 from
    /// bugs/tests/testBugs.md.
    index: bool,
    /// `#[umbra(auto_now_add)]` — populate with `Utc::now()` on
    /// create. Closes BUG-5 from bugs/tests/testBugs.md.
    auto_now_add: bool,
    /// `#[umbra(auto_now)]` — populate with `Utc::now()` on every
    /// write. Closes BUG-5.
    auto_now: bool,
    /// `#[umbra(help = "...")]` — column help text. Flows to
    /// OpenAPI `description` and admin form hints. Closes
    /// playground-openapi-gaps item 5.
    help: Option<String>,
    /// `#[umbra(example = "...")]` — sample value rendered as
    /// OpenAPI `example`. Closes playground-openapi-gaps item 6.
    example: Option<String>,
    /// `#[umbra(backend = "postgres")]` — restrict this field to a
    /// specific backend (or several). The boot system check fails
    /// when the active backend isn't in the list. Closes IMP-5
    /// from `bugs/tests/testBugs.md`. Accept multiple via repeat:
    /// `#[umbra(backend = "postgres"), umbra(backend = "mysql")]`.
    backends: Vec<String>,
    /// `#[umbra(min = N)]` — numeric lower bound. Closes IMP-3.
    min: Option<i64>,
    /// `#[umbra(max = N)]` — numeric upper bound. Closes IMP-3.
    max: Option<i64>,
    /// `#[umbra(slug_from = "title")]` — auto-derive this column from
    /// a sibling column at write time. Gap 109. Only meaningful on
    /// `Slug` / `String` columns; the runtime in
    /// `umbra-core::orm::write` computes the slug when the column
    /// itself isn't supplied on the body, leaving explicit
    /// user-supplied slugs alone.
    slug_from: Option<String>,
    /// Gap #44 — `#[umbra(reverse_fk = "post")]` on a
    /// `ReverseSet<Comment>` field names the FK column on `Comment`
    /// that points back at this parent. The macro emits one
    /// `ReverseFkRelationSpec` entry per such field and wires
    /// `set_parent_id` / `set_fk_column` / `set_reverse_fk_resolved_json`
    /// arms so `prefetch_related("comment_set")` lights up.
    /// Required when the field type is `ReverseSet<C>`; ignored on
    /// any other field type.
    reverse_fk: Option<String>,
}

fn parse_umbra_field_attr(attrs: &[syn::Attribute]) -> syn::Result<UmbraFieldAttr> {
    let mut parsed = UmbraFieldAttr {
        noform: false,
        noedit: false,
        primary_key: false,
        no_reverse: false,
        is_string_repr: false,
        max_length: 0,
        choices_ty: None,
        default: None,
        unique: false,
        on_delete: None,
        on_update: None,
        index: false,
        auto_now_add: false,
        auto_now: false,
        help: None,
        example: None,
        backends: Vec::new(),
        min: None,
        max: None,
        slug_from: None,
        reverse_fk: None,
    };
    for attr in attrs {
        if !attr.path().is_ident("umbra") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("noform") {
                parsed.noform = true;
                Ok(())
            } else if meta.path.is_ident("noedit") {
                parsed.noedit = true;
                Ok(())
            } else if meta.path.is_ident("primary_key") {
                parsed.primary_key = true;
                Ok(())
            } else if meta.path.is_ident("no_reverse") {
                parsed.no_reverse = true;
                Ok(())
            } else if meta.path.is_ident("string") {
                // Both `#[umbra(string)]` and `#[umbra(string = true)]` work.
                if let Ok(value) = meta.value() {
                    let lit: syn::LitBool = value.parse()?;
                    parsed.is_string_repr = lit.value;
                } else {
                    parsed.is_string_repr = true;
                }
                Ok(())
            } else if meta.path.is_ident("max_length") {
                let value = meta.value()?;
                let lit: syn::LitInt = value.parse()?;
                parsed.max_length = lit.base10_parse()?;
                Ok(())
            } else if meta.path.is_ident("choices") {
                // `#[umbra(choices)]` — marker. The Rust field type
                // is the choices enum; no explicit type token needed.
                // We stamp Some(()) here and read the *field's* Rust
                // type at the FieldSpec emission site to fill in the
                // `<T as ChoiceField>::VALUES` tokens.
                parsed.choices_ty = Some(quote!(()));
                Ok(())
            } else if meta.path.is_ident("default") {
                // `#[umbra(default = "...")]` — SQL DEFAULT clause.
                // String literal only at v1; the migration engine
                // emits the value verbatim as a quoted SQL string on
                // both CREATE TABLE and ADD COLUMN.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.default = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("unique") {
                // `#[umbra(unique)]` — marker. Emits a column-level
                // UNIQUE constraint at CREATE TABLE time. Closes
                // gap #65.
                parsed.unique = true;
                Ok(())
            } else if meta.path.is_ident("on_delete") {
                // `#[umbra(on_delete = "cascade" | "restrict" |
                // "set_null" | "no_action")]` — emit
                // `ON DELETE <action>` in the REFERENCES tail.
                // Closes gap #68.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.on_delete = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("on_update") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.on_update = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("index") {
                // `#[umbra(index)]` — marker. Emits a single-column
                // `CREATE INDEX idx_<table>_<column>` alongside the
                // CREATE TABLE. Skipped for PK / UNIQUE columns
                // (already indexed by the constraint).
                parsed.index = true;
                Ok(())
            } else if meta.path.is_ident("auto_now_add") {
                parsed.auto_now_add = true;
                Ok(())
            } else if meta.path.is_ident("auto_now") {
                parsed.auto_now = true;
                Ok(())
            } else if meta.path.is_ident("help") {
                // `#[umbra(help = "human text")]` — column
                // description string. Flows to OpenAPI
                // `description` and admin form hints.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.help = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("example") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.example = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("backend") {
                // `#[umbra(backend = "postgres")]` — restrict to
                // one backend. Repeat the attribute to add more.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.backends.push(lit.value());
                Ok(())
            } else if meta.path.is_ident("min") {
                let value = meta.value()?;
                let lit: syn::LitInt = value.parse()?;
                parsed.min = Some(lit.base10_parse()?);
                Ok(())
            } else if meta.path.is_ident("max") {
                let value = meta.value()?;
                let lit: syn::LitInt = value.parse()?;
                parsed.max = Some(lit.base10_parse()?);
                Ok(())
            } else if meta.path.is_ident("slug_from") {
                // `#[umbra(slug_from = "title")]` — auto-derive at
                // write time. Gap 109. The string is a sibling column
                // name (snake_case form, matching FieldSpec::name).
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.slug_from = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("reverse_fk") {
                // Gap #44 — `#[umbra(reverse_fk = "post")]` on a
                // `ReverseSet<Comment>` field names the FK column on
                // `Comment` that points back at this parent. Required
                // for ReverseSet fields; ignored on others.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.reverse_fk = Some(lit.value());
                Ok(())
            } else {
                // Unknown key. Report it with the known set so the
                // common typo case (`is_string_repr` instead of
                // `string`, an old name from an earlier doc draft, a
                // struct-level key on a field) doesn't manifest as the
                // opaque "expected `,`" parser error.
                //
                // Consume any `= value` part first so the outer parser
                // doesn't trip on `=` after we hand back control —
                // otherwise the user sees the wrong span and the wrong
                // error message.
                if let Ok(value) = meta.value() {
                    // Best-effort: parse + discard whatever comes next.
                    let _: syn::Expr = value.parse()?;
                }
                let path = meta
                    .path
                    .get_ident()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "<unknown>".to_string());
                Err(meta.error(format!(
                    "unknown field-level umbra attribute `{path}` — known keys are \
                     `noform`, `noedit`, `primary_key`, `no_reverse`, \
                     `string` (or `string = true`), \
                     `max_length = N`, `choices`, `default = \"...\"`, \
                     `unique`, `on_delete = \"...\"`, \
                     `on_update = \"...\"`, `index`, `auto_now`, \
                     `auto_now_add`, `help = \"...\"`, \
                     `example = \"...\"`, `backend = \"...\"`, \
                     `min = N`, `max = N`, `slug_from = \"...\"`, \
                     and `reverse_fk = \"...\"`"
                )))
            }
        })?;
    }
    Ok(parsed)
}

/// Translate the user-supplied `on_delete` / `on_update` string into
/// a path token referring to `umbra::orm::FkAction::<Variant>`.
/// `None` resolves to `NoAction` so a missing attribute is the
/// default. An unknown value or a non-FK field with the attribute
/// set produces a typed compile error at the field's span.
fn fk_action_tokens(
    value: &Option<String>,
    field_ty: &syn::Type,
    is_fk_field: bool,
    attr_name: &str,
) -> syn::Result<proc_macro2::TokenStream> {
    let Some(raw) = value else {
        return Ok(quote!(::umbra::orm::FkAction::NoAction));
    };
    if !is_fk_field {
        return Err(syn::Error::new_spanned(
            field_ty,
            format!(
                "umbra: `{attr_name}` is only meaningful on `ForeignKey<T>` / \
                 `Option<ForeignKey<T>>` fields"
            ),
        ));
    }
    let normalised = raw.to_lowercase();
    let variant_ident = match normalised.as_str() {
        "no_action" | "no action" => "NoAction",
        "cascade" => "Cascade",
        "restrict" => "Restrict",
        "set_null" | "set null" => "SetNull",
        other => {
            return Err(syn::Error::new_spanned(
                field_ty,
                format!(
                    "umbra: unknown `{attr_name}` value `{other}` — accepted: \
                     `cascade`, `restrict`, `set_null`, `no_action`"
                ),
            ));
        }
    };
    let variant = syn::Ident::new(variant_ident, proc_macro2::Span::call_site());
    Ok(quote!(::umbra::orm::FkAction::#variant))
}

/// Lower a `Vec<Vec<String>>` (e.g. `unique_together` / `indexes`)
/// into a `&'static [&'static [&'static str]]` token stream the
/// `Model` trait consts can hold without allocation. Closes BUG-6/7.
fn render_str_groups(groups: &[Vec<String>]) -> TokenStream2 {
    if groups.is_empty() {
        return quote!(&[]);
    }
    let groups_tokens = groups.iter().map(|group| {
        let lits = group.iter().map(|s| quote!(#s));
        quote! { &[#(#lits),*] as &'static [&'static str] }
    });
    quote!(&[#(#groups_tokens),*])
}

/// Read a `[ "a", "b", "c" ]` literal expression into a `Vec<String>`,
/// or accept a single bare string literal `"a"` as a one-element list.
/// `context` names the attribute for the error message so the user sees
/// e.g. "umbra: `unique_together` entries must be string literals."
/// Closes BUG-6/7/8 parser helpers.
fn parse_str_array(expr: &syn::Expr, context: &str) -> syn::Result<Vec<String>> {
    match expr {
        syn::Expr::Array(arr) => {
            let mut out = Vec::with_capacity(arr.elems.len());
            for elem in &arr.elems {
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(lit),
                    ..
                }) = elem
                else {
                    return Err(syn::Error::new_spanned(
                        elem,
                        format!("umbra: `{context}` entries must be string literals"),
                    ));
                };
                out.push(lit.value());
            }
            Ok(out)
        }
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit),
            ..
        }) => Ok(vec![lit.value()]),
        _ => Err(syn::Error::new_spanned(
            expr,
            format!("umbra: `{context}` must be a string-literal array"),
        )),
    }
}

fn parse_umbra_struct_attr(attrs: &[syn::Attribute]) -> syn::Result<UmbraStructAttr> {
    let mut parsed = UmbraStructAttr {
        table: None,
        plugin: None,
        display: None,
        icon: None,
        database: None,
        singleton: false,
        soft_delete: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
    };
    for attr in attrs {
        if !attr.path().is_ident("umbra") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.table = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("plugin") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.plugin = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("display") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.display = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("icon") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.icon = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("database") {
                // `#[umbra(database = "analytics")]` — pin this model
                // to a specific pool alias when the app registers
                // more than one via `AppBuilder::database(...)`. Wins
                // over the owning plugin's `Plugin::database()`.
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                parsed.database = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("singleton") {
                // `#[umbra(singleton)]` — single-row marker. Closes
                // BUG-9. The admin reads `Model::SINGLETON` to
                // redirect list view to the row's edit page and to
                // hide the "+ New" button.
                parsed.singleton = true;
                Ok(())
            } else if meta.path.is_ident("soft_delete") {
                // Feature #72 — soft-delete marker. The user MUST
                // also declare `pub deleted_at:
                // Option<DateTime<Utc>>` on the struct (derive
                // macros can't add fields). The framework reads
                // `Model::SOFT_DELETE` to inject
                // `WHERE deleted_at IS NULL` on QuerySet terminals
                // and to rewrite delete() as an UPDATE.
                parsed.soft_delete = true;
                Ok(())
            } else if meta.path.is_ident("unique_together") {
                // `#[umbra(unique_together = [["a","b"], ["c"]])]`
                // — composite UNIQUE constraints. Closes BUG-6.
                let value = meta.value()?;
                let outer: syn::ExprArray = value.parse()?;
                for inner in outer.elems {
                    parsed
                        .unique_together
                        .push(parse_str_array(&inner, "unique_together")?);
                }
                Ok(())
            } else if meta.path.is_ident("indexes") {
                // `#[umbra(indexes = [["a","b"], ["c"]])]` —
                // multi-column indexes. Closes BUG-7.
                let value = meta.value()?;
                let outer: syn::ExprArray = value.parse()?;
                for inner in outer.elems {
                    parsed.indexes.push(parse_str_array(&inner, "indexes")?);
                }
                Ok(())
            } else if meta.path.is_ident("ordering") {
                // `#[umbra(ordering = ["-published_at", "id"])]` —
                // default ORDER BY. Closes BUG-8. A leading `-` on a
                // column name flips the direction to DESC.
                let value = meta.value()?;
                let outer: syn::ExprArray = value.parse()?;
                let names = parse_str_array(&syn::Expr::Array(outer), "ordering")?;
                for raw in names {
                    let (name, desc) = if let Some(stripped) = raw.strip_prefix('-') {
                        (stripped.to_string(), true)
                    } else {
                        (raw, false)
                    };
                    parsed.ordering.push((name, desc));
                }
                Ok(())
            } else {
                Err(meta.error(
                    "umbra::Model derive accepts struct-level `table = \"...\"`, `plugin = \"...\"`, \
                     `display = \"...\"`, `icon = \"...\"`, `database = \"...\"`, `singleton`, `soft_delete`, \
                     `unique_together = [[...]]`, `indexes = [[...]]`, `ordering = [\"-col\", \"col\"]`; \
                     and field-level `noform` and `noedit`. \
                     Other attributes (max_length, db_index, default, choices, on_delete) land as \
                     plugin authors need them",
                ))
            }
        })?;
    }
    Ok(parsed)
}

/// Top-level expansion: parse the input, validate the shape, emit the
/// three pieces (trait impl, inherent `objects`, sibling column module).
///
/// Errors here are user-facing (wrong struct shape, missing `id`). Per-
/// field errors are produced inside the field loop and woven into the
/// output so the user sees every problem at once rather than fixing one,
/// recompiling, and discovering the next.
fn expand_model(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;

    // Only named-field structs are valid models. Enums, unions, tuple
    // structs, and unit structs all fail with the same message so the
    // user knows the shape M3 expects.
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    struct_name,
                    "umbra::Model can only be derived on structs with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "umbra::Model can only be derived on structs with named fields",
            ));
        }
    };

    // Primary-key field detection:
    //   1. First-match: any field carrying `#[umbra(primary_key)]`.
    //      The explicit marker lets a model name its PK something
    //      domain-specific — e.g. `Permission` with
    //      `#[umbra(primary_key)] pub codename: String`.
    //   2. Fallback: a field literally named `id`. The historical
    //      default, kept so existing models compile unchanged.
    //
    // Either way the PK type (the field's Rust type) isn't validated
    // here: any type implementing `umbra::orm::PrimaryKey` works. The
    // trait ships impls for every Rust integer width, `uuid::Uuid`,
    // and `String`; user crates can add their own with
    // `impl PrimaryKey for MyId {}`.
    //
    // The `PrimaryKey` associated type echoes the user-written field
    // type verbatim so user crate paths (`uuid::Uuid`, `::uuid::Uuid`,
    // bare `Uuid`) round-trip unchanged through the emitted tokens.
    let explicit_pk = fields
        .iter()
        .find(|f| match parse_umbra_field_attr(&f.attrs) {
            Ok(attrs) => attrs.primary_key,
            Err(_) => false,
        });
    let id_field = explicit_pk.or_else(|| {
        fields
            .iter()
            .find(|f| f.ident.as_ref().is_some_and(|i| i == "id"))
    });
    let id_field = match id_field {
        Some(f) => f,
        None => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "umbra::Model requires a primary-key field — either name a field \
                 `id` (historical default) or mark one with `#[umbra(primary_key)]`",
            ));
        }
    };
    // Name of the PK field — needed by the `primary_key()` impl
    // below so it picks `self.codename` instead of `self.id` when the
    // model nominated a non-standard PK column.
    let pk_field_name = id_field.ident.as_ref().expect("PK field must have a name");
    // The id field's type isn't validated here: any type implementing
    // `umbra::orm::PrimaryKey` works. The trait ships impls for every
    // Rust integer width, `uuid::Uuid`, and `String`; user crates can
    // add their own with `impl PrimaryKey for MyId {}`. The trait
    // bound on `Model::PrimaryKey` makes the compiler reject types
    // that don't implement it, with a real Rust diagnostic (which is
    // more useful than the previous hard-coded "i32/i64/Uuid" message
    // when the user's intent is genuinely a custom newtype).
    //
    // The `PrimaryKey` associated type echoes the user-written field
    // type verbatim so user crate paths (`uuid::Uuid`, `::uuid::Uuid`,
    // bare `Uuid`) round-trip unchanged through the emitted tokens.
    let pk_ty_tokens = &id_field.ty;
    // Gap 19: detect whether the PK type is `i64` so we can emit the
    // `HydrateRelated::pk_i64` override that the prefetch_related
    // hydration uses to collect parent ids. Match on the type's
    // token-string; covers the bare `i64` form. Non-i64 PK models
    // inherit the default (`None`), silently disabling prefetch on
    // them — same v1 constraint as `set_m2m_parent_ids`.
    let pk_is_i64 = quote!(#pk_ty_tokens).to_string().trim() == "i64";

    // The default table name is snake_case of the struct name. Two opt-in
    // attribute keys change this, with explicit-table winning over plugin
    // prefix:
    //
    //   1. `#[umbra(plugin = "blog")]` — prefixes the snake_case struct
    //      name: `Post` → `"blog_post"`. Prevents table collisions when
    //      multiple plugins each ship a model with the same struct name.
    //   2. `#[umbra(table = "...")]` — explicit override, always wins.
    //      Even if `plugin` is also set, the explicit table is used.
    //
    // Built-in plugins (auth, sessions, admin, tasks) keep their existing
    // bare names and do NOT use `plugin = "..."` — their table names are
    // stable DB identifiers that existing users must not have renamed.
    let struct_attr = parse_umbra_struct_attr(&input.attrs)?;
    let bare_name = to_snake_case(&struct_name.to_string());
    let table_name = if let Some(explicit) = struct_attr.table {
        // Explicit table always wins over the plugin prefix.
        explicit
    } else if let Some(plugin_prefix) = struct_attr.plugin {
        // `"app"` is the implicit namespace for user-binary models
        // (everything registered via `App::builder().model::<T>()`).
        // Tagging a model `#[umbra(plugin = "app")]` is a no-op for
        // the prefix — the bare snake_case table name stays, so the
        // model continues to land in the "app" admin sidebar bucket
        // without forcing a destructive rename migration on existing
        // databases. Any other prefix means "this model belongs to
        // plugin <X>; prefix the table for collision-free coexistence
        // with other plugins' same-named models."
        if plugin_prefix == "app" {
            bare_name
        } else {
            format!("{}_{}", plugin_prefix, bare_name)
        }
    } else {
        bare_name
    };
    // Gap 44: DISPLAY defaults to the struct name; overridden by
    // `#[umbra(display = "...")]`. ICON defaults to `"database"`;
    // overridden by `#[umbra(icon = "...")]`.
    let display_lit = struct_attr
        .display
        .unwrap_or_else(|| struct_name.to_string());
    let icon_lit = struct_attr.icon.unwrap_or_else(|| "database".to_string());
    let database_tokens = match struct_attr.database {
        Some(alias) => quote! { ::core::option::Option::Some(#alias) },
        None => quote! { ::core::option::Option::None },
    };
    let soft_delete_lit = if struct_attr.soft_delete {
        quote!(true)
    } else {
        quote!(false)
    };
    let singleton_lit = if struct_attr.singleton {
        quote!(true)
    } else {
        quote!(false)
    };
    // BUG-6/7/8: render the struct-level attributes as `&'static [...]`
    // slices so they can sit as `Model::UNIQUE_TOGETHER` / `INDEXES` /
    // `ORDERING` associated consts. Slice-of-slices keeps `Model` Copy.
    let unique_together_tokens = render_str_groups(&struct_attr.unique_together);
    let indexes_tokens = render_str_groups(&struct_attr.indexes);
    let ordering_pairs = struct_attr
        .ordering
        .iter()
        .map(|(name, desc)| {
            let desc_lit = if *desc { quote!(true) } else { quote!(false) };
            quote! { (#name, #desc_lit) }
        })
        .collect::<Vec<_>>();
    let ordering_tokens = if ordering_pairs.is_empty() {
        quote!(&[])
    } else {
        quote!(&[#(#ordering_pairs),*])
    };
    // The sibling column module's identifier is always snake_case of
    // the struct name (the user-facing path is `<snake_struct>::FIELD`).
    // Leaving it untouched keeps existing user code working when a
    // table-name override lands.
    let module_name = format_ident!("{}", to_snake_case(&struct_name.to_string()));

    // Field-spec entries for the trait's FIELDS const, and column-const
    // declarations for the sibling module. Built side by side so the
    // declaration order matches between the two.
    let mut field_specs: Vec<TokenStream2> = Vec::new();
    let mut column_consts: Vec<TokenStream2> = Vec::new();
    let mut m2m_specs: Vec<TokenStream2> = Vec::new();
    // Gap #44 — one entry per `ReverseSet<C>` field. Emitted as
    // `Model::REVERSE_FK_RELATIONS` so the prefetch_related dispatch
    // can look them up at terminal time.
    let mut reverse_fk_specs: Vec<TokenStream2> = Vec::new();
    // OneToOne — same collector trio as ReverseSet but emits to
    // ONE_TO_ONE_RELATIONS, set_m2m_parent_ids (parent_id only —
    // no fk_column, that's resolved at runtime), and
    // set_one_to_one_resolved_json.
    let mut one_to_one_specs: Vec<TokenStream2> = Vec::new();
    let mut one_to_one_parent_arms: Vec<TokenStream2> = Vec::new();
    let mut one_to_one_resolved_arms: Vec<TokenStream2> = Vec::new();
    // Arms for the macro-emitted `set_m2m_parent_ids` body: each
    // calls `set_parent_id(__pk)` + `set_fk_column("<fk_col>")` on
    // one ReverseSet field, so the slot knows which children to
    // associate with itself.
    let mut reverse_fk_parent_arms: Vec<TokenStream2> = Vec::new();
    // Match arms for the per-field
    // `HydrateRelated::set_reverse_fk_resolved_json` body.
    let mut reverse_fk_resolved_arms: Vec<TokenStream2> = Vec::new();

    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        // PK detection: the field this iteration is on is the PK iff it
        // matches the one `id_field` resolved above — either explicitly
        // tagged `#[umbra(primary_key)]` or named `id` as the default.
        let is_primary_key = field_name == pk_field_name;

        let kind = classify_field_type(&field.ty);

        // BUG-16: M2M<T> fields have no column on the parent table.
        // Skip them for FIELDS/column_consts and collect them into
        // M2M_RELATIONS instead.
        if let FieldKind::Many2Many(ref inner_ty) = kind {
            let inner = inner_ty.as_ref();
            m2m_specs.push(quote! {
                ::umbra::orm::M2MRelationSpec {
                    field_name: #field_name_str,
                    target_table: <#inner as ::umbra::orm::Model>::TABLE,
                    target_name: <#inner as ::umbra::orm::Model>::NAME,
                }
            });
            continue;
        }

        // Gap #44: ReverseSet<C> fields also have no column on the
        // parent table. Skip from FIELDS/column_consts; collect into
        // reverse_fk_specs (plus the field ident so the hydrate /
        // set_parent_id arms can reference it later). The
        // `#[umbra(reverse_fk = "...")]` attribute is REQUIRED on
        // ReverseSet fields — without it we'd have no FK column
        // name to filter children on. Emit a compile-error if
        // missing so the failure surfaces at the right span.
        if let FieldKind::ReverseSet(ref inner_ty) = kind {
            let inner = inner_ty.as_ref();
            let field_attr = match parse_umbra_field_attr(&field.attrs) {
                Ok(a) => a,
                Err(e) => {
                    field_specs.push(e.to_compile_error());
                    continue;
                }
            };
            let Some(fk_col) = field_attr.reverse_fk.as_ref() else {
                let err = syn::Error::new_spanned(
                    field,
                    "ReverseSet<C> fields require `#[umbra(reverse_fk = \"<fk_column>\")]` \
                     naming the FK column on the child model that points back at this parent",
                );
                field_specs.push(err.to_compile_error());
                continue;
            };
            reverse_fk_specs.push(quote! {
                ::umbra::orm::ReverseFkRelationSpec {
                    field_name: #field_name_str,
                    target_table: <#inner as ::umbra::orm::Model>::TABLE,
                    target_name: <#inner as ::umbra::orm::Model>::NAME,
                    fk_column: #fk_col,
                }
            });
            let field_ident = field.ident.as_ref().expect("named field has ident").clone();
            let fk_col_lit = fk_col.clone();
            reverse_fk_parent_arms.push(quote! {
                self.#field_ident.set_parent_id(__pk);
                self.#field_ident.set_fk_column(#fk_col_lit);
            });
            reverse_fk_resolved_arms.push(quote! {
                #field_name_str => {
                    let mut decoded: ::std::vec::Vec<#inner> = ::std::vec::Vec::with_capacity(rows.len());
                    for row in rows {
                        if let ::core::result::Result::Ok(c) =
                            ::umbra::_serde_json::from_value::<#inner>(row)
                        {
                            decoded.push(c);
                        }
                    }
                    self.#field_ident.set_resolved(decoded);
                }
            });
            continue;
        }

        // OneToOne<C> — no column on the parent. Unlike ReverseSet
        // this requires NO `#[umbra(...)]` attribute: the back-
        // pointing FK column is resolved at runtime by scanning
        // the child's FIELDS for the UNIQUE FK whose `fk_target`
        // is this parent's table. The macro just emits the spec
        // + the hydration arms.
        if let FieldKind::OneToOne(ref inner_ty) = kind {
            let inner = inner_ty.as_ref();
            one_to_one_specs.push(quote! {
                ::umbra::orm::OneToOneRelationSpec {
                    field_name: #field_name_str,
                    target_table: <#inner as ::umbra::orm::Model>::TABLE,
                    target_name: <#inner as ::umbra::orm::Model>::NAME,
                }
            });
            let field_ident = field.ident.as_ref().expect("named field has ident").clone();
            one_to_one_parent_arms.push(quote! {
                self.#field_ident.set_parent_id(__pk);
            });
            one_to_one_resolved_arms.push(quote! {
                #field_name_str => {
                    let decoded: ::core::option::Option<#inner> = match row {
                        ::core::option::Option::Some(v) =>
                            ::umbra::_serde_json::from_value::<#inner>(v).ok(),
                        ::core::option::Option::None => ::core::option::Option::None,
                    };
                    self.#field_ident.set_resolved(decoded);
                }
            });
            continue;
        }

        // Parse field-level `#[umbra(noform)]` / `#[umbra(noedit)]`.
        let field_attr = match parse_umbra_field_attr(&field.attrs) {
            Ok(a) => a,
            Err(e) => {
                field_specs.push(e.to_compile_error());
                column_consts.push(e.to_compile_error());
                continue;
            }
        };
        let noform_lit = if field_attr.noform {
            quote!(true)
        } else {
            quote!(false)
        };
        let noedit_lit = if field_attr.noedit {
            quote!(true)
        } else {
            quote!(false)
        };
        let is_string_repr_lit = if field_attr.is_string_repr {
            quote!(true)
        } else {
            quote!(false)
        };
        let max_length_lit = field_attr.max_length;
        let is_choices_field = field_attr.choices_ty.is_some();
        // The other path into "choices-shaped metadata": a field of
        // type `MultiChoice<E>` carries the same closed-set values, but
        // stores them as a CSV. Detected purely from the Rust type — no
        // attribute marker — since `MultiChoice<E>` is already
        // unambiguous.
        let is_multichoice_field = matches!(kind, FieldKind::MultiChoice(_));

        // When `#[umbra(choices)]` is set, the field's Rust type is
        // a `ChoiceField`-implementing enum. Bypass the catalogue and
        // emit `SqlType::Text`. The bind/decode round-trip is handled
        // by the user's `#[derive(Choices)]` (which emits sqlx::Type +
        // Encode + Decode treating the enum as a TEXT value).
        let (sql_ty_tokens, nullable_lit) = if is_choices_field {
            // A choices field of type `T` or `Option<T>`. We don't
            // unwrap the Option here — the existing classifier already
            // walks `Option<T>` for primitive types; for choices we
            // tell the user "non-nullable only at v1" via a hard
            // error if they wrap in `Option`. Detecting that means
            // peeking at the Rust type.
            if is_option_type(&field.ty) {
                let err = syn::Error::new_spanned(
                    &field.ty,
                    "umbra: `#[umbra(choices)]` on `Option<T>` is deferred. \
                     For a nullable choices column, declare a `None` variant on the enum \
                     and use a non-Option field.",
                )
                .to_compile_error();
                field_specs.push(err.clone());
                column_consts.push(err);
                continue;
            }
            (quote!(::umbra::orm::SqlType::Text), quote!(false))
        } else {
            match kind.sql_type_tokens() {
                Some((ty, nullable)) => (ty, nullable),
                None => {
                    // `Unsupported` lands here. Emit a typed error at
                    // the field's span and keep going so the user sees
                    // every problematic field at once.
                    let err =
                        syn::Error::new_spanned(&field.ty, kind.error_message()).to_compile_error();
                    field_specs.push(err.clone());
                    column_consts.push(err);
                    continue;
                }
            }
        };

        let pk_lit = if is_primary_key {
            quote!(true)
        } else {
            quote!(false)
        };

        // For ForeignKey<T> fields, emit `fk_target: Some(<T as Model>::TABLE)`.
        // For all other fields, emit `fk_target: None`.
        let fk_target_tokens = match &kind {
            FieldKind::ForeignKey(inner_ty) => {
                quote! { Some(<#inner_ty as ::umbra::orm::Model>::TABLE) }
            }
            FieldKind::NullableForeignKey(inner_ty) => {
                quote! { Some(<#inner_ty as ::umbra::orm::Model>::TABLE) }
            }
            _ => quote! { None },
        };

        // For choices fields, emit `choices: <T as ChoiceField>::VALUES`
        // (and the matching label slice). For MultiChoice<E> fields,
        // the inner enum `E` is what implements `ChoiceField`. The
        // user's enum type is passed verbatim into the trait
        // disambiguation so user crate paths round-trip.
        let (choices_tokens, choice_labels_tokens) = if is_choices_field {
            let ty = &field.ty;
            (
                quote! { <#ty as ::umbra::orm::ChoiceField>::VALUES },
                quote! { <#ty as ::umbra::orm::ChoiceField>::LABELS },
            )
        } else if let FieldKind::MultiChoice(ref inner) = kind {
            let inner_ty = inner.as_ref();
            (
                quote! { <#inner_ty as ::umbra::orm::ChoiceField>::VALUES },
                quote! { <#inner_ty as ::umbra::orm::ChoiceField>::LABELS },
            )
        } else {
            (quote! { &[] }, quote! { &[] })
        };

        // `#[umbra(default = "...")]` lifts to a static-str default.
        // Empty string means none.
        let default_tokens = match &field_attr.default {
            Some(s) => quote! { #s },
            None => quote! { "" },
        };
        let is_multichoice_lit = if is_multichoice_field {
            quote!(true)
        } else {
            quote!(false)
        };
        let unique_lit = if field_attr.unique {
            quote!(true)
        } else {
            quote!(false)
        };
        let index_lit = if field_attr.index {
            quote!(true)
        } else {
            quote!(false)
        };
        let auto_now_add_lit = if field_attr.auto_now_add {
            quote!(true)
        } else {
            quote!(false)
        };
        let auto_now_lit = if field_attr.auto_now {
            quote!(true)
        } else {
            quote!(false)
        };
        let help_tokens = match &field_attr.help {
            Some(s) => quote! { #s },
            None => quote! { "" },
        };
        let example_tokens = match &field_attr.example {
            Some(s) => quote! { #s },
            None => quote! { "" },
        };
        let backends_tokens = if field_attr.backends.is_empty() {
            quote! { &[] }
        } else {
            let lits = field_attr.backends.iter().map(|s| quote!(#s));
            quote! { &[#(#lits),*] }
        };
        let min_tokens = match field_attr.min {
            Some(n) => {
                let lit = syn::LitInt::new(&format!("{n}_i64"), proc_macro2::Span::call_site());
                quote! { ::core::option::Option::Some(#lit) }
            }
            None => quote! { ::core::option::Option::None },
        };
        let max_tokens = match field_attr.max {
            Some(n) => {
                let lit = syn::LitInt::new(&format!("{n}_i64"), proc_macro2::Span::call_site());
                quote! { ::core::option::Option::Some(#lit) }
            }
            None => quote! { ::core::option::Option::None },
        };

        // `on_delete` / `on_update` → token paths into FkAction. An
        // unknown value (typo, unsupported variant) becomes a
        // compile error pointing at the field so the user sees the
        // wrong attribute in IDE squiggle, not a downstream
        // runtime panic.
        let is_fk_field = matches!(
            kind,
            FieldKind::ForeignKey(_) | FieldKind::NullableForeignKey(_)
        );
        let on_delete_tokens =
            match fk_action_tokens(&field_attr.on_delete, &field.ty, is_fk_field, "on_delete") {
                Ok(t) => t,
                Err(e) => {
                    field_specs.push(e.to_compile_error());
                    column_consts.push(e.to_compile_error());
                    continue;
                }
            };
        let on_update_tokens =
            match fk_action_tokens(&field_attr.on_update, &field.ty, is_fk_field, "on_update") {
                Ok(t) => t,
                Err(e) => {
                    field_specs.push(e.to_compile_error());
                    column_consts.push(e.to_compile_error());
                    continue;
                }
            };

        // BUG-11/12/13: lower validator wrapper types to a
        // `text_format` marker so downstream consumers know which
        // validator to run / which OpenAPI format key to emit.
        let text_format_tokens = match kind {
            FieldKind::Slug => {
                quote!(::core::option::Option::Some("slug"))
            }
            FieldKind::Email => {
                quote!(::core::option::Option::Some("email"))
            }
            FieldKind::Url => {
                quote!(::core::option::Option::Some("url"))
            }
            _ => quote!(::core::option::Option::None),
        };

        // Gap 109: `#[umbra(slug_from = "title")]` carries through to
        // FieldSpec so the dynamic write path can auto-derive the
        // slug when the body omits the column.
        let slug_from_tokens = match &field_attr.slug_from {
            Some(s) => quote!(::core::option::Option::Some(#s)),
            None => quote!(::core::option::Option::None),
        };

        field_specs.push(quote! {
            ::umbra::orm::FieldSpec {
                name: #field_name_str,
                ty: #sql_ty_tokens,
                primary_key: #pk_lit,
                nullable: #nullable_lit,
                supported_backends: #backends_tokens,
                fk_target: #fk_target_tokens,
                noform: #noform_lit,
                noedit: #noedit_lit,
                is_string_repr: #is_string_repr_lit,
                max_length: #max_length_lit,
                choices: #choices_tokens,
                choice_labels: #choice_labels_tokens,
                default: #default_tokens,
                is_multichoice: #is_multichoice_lit,
                unique: #unique_lit,
                on_delete: #on_delete_tokens,
                on_update: #on_update_tokens,
                index: #index_lit,
                auto_now_add: #auto_now_add_lit,
                auto_now: #auto_now_lit,
                help: #help_tokens,
                example: #example_tokens,
                min: #min_tokens,
                max: #max_tokens,
                text_format: #text_format_tokens,
                slug_from: #slug_from_tokens,
            }
        });

        if is_choices_field || is_multichoice_field {
            // Closed-set TEXT fields (single- or multi-valued) get a
            // `StrCol` predicate constant so filter chains stay
            // ergonomic: `article::STATUS.eq("draft")` or
            // `article::TAGS.contains("design")`. The exact set of
            // operations expressible on multichoice is narrower than
            // on a true relational M2M, but the predicate constant
            // form is the same.
            let const_ident = format_ident!("{}", to_screaming_snake_case(&field_name_str));
            let span = field.ty.span();
            column_consts.push(quote_spanned! { span =>
                pub const #const_ident: ::umbra::orm::column::StrCol<super::#struct_name> =
                    ::umbra::orm::column::StrCol::new(#field_name_str);
            });
        } else {
            column_consts.push(column_const_for(struct_name, &field_name_str, field, &kind));
        }
    }

    // Collect the FK field names and their target types for the
    // HydrateRelated impl. We need (field_name_str, inner_ty) pairs for
    // each ForeignKey<U> or Option<ForeignKey<U>> field.
    let mut hydrate_arms: Vec<TokenStream2> = Vec::new();
    let mut fk_id_arms: Vec<TokenStream2> = Vec::new();
    // BUG-16 step 2: collect M2M field idents so the macro can emit a
    // `set_m2m_parent_ids` body that wires the parent's PK into each
    // junction-table accessor at row-decode time. BUG-16 phase 3
    // follow-up: also carry the inner child type so we can emit
    // typed `<field>_contains_any` / `<field>_union_for` helpers on
    // the parent — keeps developers from ever having to spell the
    // auto-generated junction-table name themselves.
    let mut m2m_field_idents: Vec<syn::Ident> = Vec::new();
    let mut m2m_field_children: Vec<syn::Type> = Vec::new();
    // Gap 30: collect (field_ident, parent_type) pairs from every
    // non-nullable ForeignKey field so we can emit reverse-set
    // accessors on the parent type. The token-string of the parent
    // type doubles as the disambiguation key: two FKs to the same
    // type from this Child get `<child_snake>_via_<field>_set`
    // names instead of a colliding `<child_snake>_set`.
    let mut reverse_fk_entries: Vec<(syn::Ident, syn::Type)> = Vec::new();
    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        let kind = classify_field_type(&field.ty);
        // Re-parse the field attr here so we can honour `no_reverse`
        // when deciding whether to emit a Gap-30 accessor. The first
        // pass at line ~797 already validated the attrs, so a parse
        // error is impossible here — fall back to defaults to keep
        // the call infallible.
        let field_attr = parse_umbra_field_attr(&field.attrs).unwrap_or_default();
        match &kind {
            FieldKind::Many2Many(inner_ty) => {
                m2m_field_idents.push(field_name.clone());
                m2m_field_children.push((**inner_ty).clone());
            }
            FieldKind::ForeignKey(inner_ty) => {
                if !field_attr.no_reverse {
                    reverse_fk_entries.push((field_name.clone(), (**inner_ty).clone()));
                }
                hydrate_arms.push(quote! {
                    #field_name_str => {
                        if let Ok(resolved) = ::umbra::_serde_json::from_value::<#inner_ty>(row.clone()) {
                            self.#field_name.set_resolved(resolved);
                        }
                    }
                });
                // PK serialised through serde_json then coerced back
                // to i64. i64-keyed FK targets round-trip cleanly;
                // String/UUID-keyed targets serialise to a JSON
                // string whose `as_i64()` returns None, silently
                // disabling `select_related` on this FK field — a
                // v1 limitation while the related-loading machinery
                // is still i64-shaped.
                fk_id_arms.push(quote! {
                    #field_name_str => ::umbra::_serde_json::to_value(&self.#field_name.id())
                        .ok()
                        .and_then(|v| v.as_i64()),
                });
            }
            FieldKind::NullableForeignKey(inner_ty) => {
                hydrate_arms.push(quote! {
                    #field_name_str => {
                        if let ::core::option::Option::Some(ref mut fk_mut) = self.#field_name {
                            if let Ok(resolved) = ::umbra::_serde_json::from_value::<#inner_ty>(row.clone()) {
                                fk_mut.set_resolved(resolved);
                            }
                        }
                    }
                });
                fk_id_arms.push(quote! {
                    #field_name_str => self.#field_name
                        .as_ref()
                        .and_then(|fk| ::umbra::_serde_json::to_value(&fk.id()).ok())
                        .and_then(|v| v.as_i64()),
                });
            }
            _ => {}
        }
    }

    // Sibling module name collision with the struct ident is harmless
    // because Rust's type and value namespaces are separate, but the
    // module-inception clippy lint trips when the snake_case happens to
    // equal the struct ident (e.g. a struct already named `comment`).
    // Silence it the same way `post.rs` does for parity with the M2
    // hand-written shape.
    let struct_name_str = struct_name.to_string();
    // BUG-16 phase 2: emit one `set_parent_id` + `set_junction_table`
    // pair per M2M field. The junction name follows the deterministic
    // `<parent_table>_<field_name>` convention; the migration engine
    // emits CREATE TABLE under the same name, so the two sides agree.
    // `.clone()` on __pk handles models with multiple M2M fields
    // (set_parent_id takes the PK by value).
    // Gap #44: extend the per-field block to cover ReverseSet<C>
    // slots too. The body runs once after each parent is decoded,
    // seeding `parent_id` + (for M2M) `junction_table` / (for
    // reverse-FK) `fk_column` on every relation slot.
    let set_m2m_body = if m2m_field_idents.is_empty()
        && reverse_fk_parent_arms.is_empty()
        && one_to_one_parent_arms.is_empty()
    {
        quote!({})
    } else {
        let m2m_arms = m2m_field_idents.iter().map(|ident| {
            let junction_name = format!("{}_{}", table_name, ident);
            quote! {
                self.#ident.set_parent_id(__pk.clone());
                self.#ident.set_junction_table(#junction_name);
            }
        });
        // ReverseSet arms expect a bare `__pk: i64` (the slot stores
        // Option<i64>). Models with non-i64 PKs that happen to have
        // a ReverseSet field will fail to compile here — matches the
        // same v1 i64-PK constraint as the M2M plumbing.
        let rfk_arms = reverse_fk_parent_arms.iter();
        let o2o_arms = one_to_one_parent_arms.iter();
        quote! {{
            let __pk = <Self as ::umbra::orm::Model>::primary_key(self);
            #(#m2m_arms)*
            #(#rfk_arms)*
            #(#o2o_arms)*
        }}
    };

    // BUG-16 phase 3 follow-up: typed bulk-across-parents helpers
    // emitted on the parent's inherent impl. Closes the developer
    // ergonomics gap: the auto-generated junction-table name never
    // appears in user code — these macro-emitted methods derive it
    // from `<parent_table>_<field_name>` internally and pass it to
    // `M2M::<Child>::any_holds` / `holders_of_any`.
    //
    // For a field `pub permissions: M2M<Permission>` on `Group`, the
    // macro emits:
    //
    //   impl Group {
    //       pub async fn permissions_contains_any(
    //           parent_ids: &[<Self as Model>::PrimaryKey],
    //           child_pk: <Permission as Model>::PrimaryKey,
    //       ) -> Result<bool, sqlx::Error>;
    //
    //       pub async fn permissions_union_for(
    //           parent_ids: &[<Self as Model>::PrimaryKey],
    //       ) -> Result<Vec<<Permission as Model>::PrimaryKey>, sqlx::Error>;
    //
    //       pub const fn permissions_junction_table() -> &'static str;
    //   }
    //
    // The `_junction_table()` method is the escape hatch for raw
    // queries against the junction — admin chip-picker HTMX backends
    // and similar low-level integrations. Application code should
    // prefer the typed helpers and never touch the string.
    // Gap 19: per-M2M-field arm for `set_m2m_resolved_json`. For
    // `pub tags: M2M<Tag>` on this model, emit
    // `"tags" => { ... self.tags.set_resolved(...) }`.
    let m2m_resolved_arms: Vec<TokenStream2> = m2m_field_idents
        .iter()
        .zip(m2m_field_children.iter())
        .map(|(ident, child_ty)| {
            let field_name_str = ident.to_string();
            quote! {
                #field_name_str => {
                    let parsed: ::std::vec::Vec<#child_ty> = rows
                        .into_iter()
                        .filter_map(|r| ::umbra::_serde_json::from_value::<#child_ty>(r).ok())
                        .collect();
                    self.#ident.set_resolved(parsed);
                }
            }
        })
        .collect();

    let m2m_helper_methods: Vec<TokenStream2> = m2m_field_idents
        .iter()
        .zip(m2m_field_children.iter())
        .map(|(ident, child_ty)| {
            let junction_name = format!("{}_{}", table_name, ident);
            let junction_fn = format_ident!("{}_junction_table", ident);
            let contains_any_fn = format_ident!("{}_contains_any", ident);
            let union_for_fn = format_ident!("{}_union_for", ident);
            quote! {
                /// The auto-generated M2M junction table name. Exposed
                /// for low-level integrations (raw queries, custom
                /// admin pickers). Most application code should prefer
                /// the typed helpers below.
                pub const fn #junction_fn() -> &'static str {
                    #junction_name
                }

                /// "Do any of `parent_ids` hold the M2M relation to
                /// `child_pk`?" One round-trip via
                /// `SELECT 1 FROM <junction> WHERE parent_id IN (?)
                /// AND child_id = ? LIMIT 1`.
                pub async fn #contains_any_fn(
                    parent_ids: &[<Self as ::umbra::orm::Model>::PrimaryKey],
                    child_pk: <#child_ty as ::umbra::orm::Model>::PrimaryKey,
                ) -> ::core::result::Result<bool, ::umbra::_sqlx::Error> {
                    ::umbra::orm::M2M::<#child_ty, <Self as ::umbra::orm::Model>::PrimaryKey>::any_holds(
                        Self::#junction_fn(),
                        parent_ids,
                        child_pk,
                    ).await
                }

                /// "Distinct union of every child PK any of
                /// `parent_ids` holds." One round-trip via
                /// `SELECT DISTINCT child_id FROM <junction>
                /// WHERE parent_id IN (?)`.
                pub async fn #union_for_fn(
                    parent_ids: &[<Self as ::umbra::orm::Model>::PrimaryKey],
                ) -> ::core::result::Result<
                    ::std::vec::Vec<<#child_ty as ::umbra::orm::Model>::PrimaryKey>,
                    ::umbra::_sqlx::Error,
                >
                where
                    <#child_ty as ::umbra::orm::Model>::PrimaryKey:
                        for<'r> ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Sqlite>
                        + for<'r> ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Postgres>
                        + ::umbra::_sqlx::Type<::umbra::_sqlx::Sqlite>
                        + ::umbra::_sqlx::Type<::umbra::_sqlx::Postgres>
                        + ::core::marker::Send
                        + ::core::marker::Unpin,
                {
                    ::umbra::orm::M2M::<#child_ty, <Self as ::umbra::orm::Model>::PrimaryKey>::holders_of_any(
                        Self::#junction_fn(),
                        parent_ids,
                    ).await
                }
            }
        })
        .collect();

    // Gap 30 + Gap 105: emit reverse-FK accessors on each FK target
    // type. For every `pub author: ForeignKey<User>` on this Child,
    // emit a trait `<Child><Field>Reverse` plus an
    // `impl ... for User`. Trait-based emission sidesteps Rust's
    // orphan rule, so the accessor works even when the parent type
    // lives in another crate — the canonical case is
    // `ForeignKey<AuthUser>` from `umbra-auth` consumed by a model in
    // an app crate. The user imports the trait
    // (`use blog::PostAuthorReverse;` or `use blog::*;`) and writes
    // `user.post_set()` exactly like Django.
    //
    // Trait naming: `<Child><FieldPascal>Reverse`. One trait per
    // `(Child, field)` pair, scoped to this crate, so unique within
    // the consumer's namespace and never collides across plugins.
    //
    // Method naming: `<child_snake>_set` when the child has exactly
    // one FK to this parent type; `<child_snake>_via_<field>_set`
    // when the child has multiple FKs to the same parent (so two
    // `ForeignKey<User>` fields on Post emit `post_via_author_set`
    // and `post_via_reviewer_set`, both impl'd on User — no
    // method-name collision).
    //
    // Limitations:
    //  - parent's PrimaryKey type must satisfy the column-const's
    //    `.eq` bound. For i64-keyed parents (the default) this is
    //    trivially true; non-i64 PKs work too as long as the FK
    //    column's predicate constant accepts the PK type.
    //  - The opt-out `#[umbra(no_reverse)]` still works: it skips
    //    both the trait and the impl, no E0116 because we no longer
    //    emit an inherent impl on the parent.
    let child_snake = to_snake_case(&struct_name.to_string());
    // Group occurrences by the parent type's token-string so the
    // disambiguation decision is local to a single FK target.
    let mut parent_type_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (_, parent_ty) in &reverse_fk_entries {
        let key = quote!(#parent_ty).to_string();
        *parent_type_counts.entry(key).or_insert(0) += 1;
    }
    let reverse_fk_impls: Vec<TokenStream2> = reverse_fk_entries
        .iter()
        .map(|(field_ident, parent_ty)| {
            let key = quote!(#parent_ty).to_string();
            let count = parent_type_counts.get(&key).copied().unwrap_or(1);
            let accessor_name = if count > 1 {
                format_ident!("{}_via_{}_set", child_snake, field_ident)
            } else {
                format_ident!("{}_set", child_snake)
            };
            let fk_const = format_ident!("{}", to_screaming_snake_case(&field_ident.to_string()));
            let field_pascal = to_pascal_case(&field_ident.to_string());
            let trait_name = format_ident!("{}{}Reverse", struct_name, field_pascal);
            let trait_doc = format!(
                "Reverse-FK trait emitted by `#[derive(Model)]` for `{}::{}`. \
                 Importing this trait lets callers spell `parent.{}()` to get \
                 a `QuerySet<{}>` filtered to children whose `{}` FK points at \
                 the parent. Trait-based emission (gap 105) sidesteps the \
                 orphan rule, so the accessor works even when the parent \
                 type is defined in another crate.",
                struct_name, field_ident, accessor_name, struct_name, field_ident,
            );
            quote! {
                #[doc = #trait_doc]
                pub trait #trait_name {
                    fn #accessor_name(&self) -> ::umbra::orm::QuerySet<#struct_name>;
                }
                impl #trait_name for #parent_ty {
                    fn #accessor_name(&self) -> ::umbra::orm::QuerySet<#struct_name> {
                        let __pk = <Self as ::umbra::orm::Model>::primary_key(self);
                        #struct_name::objects()
                            .filter(#module_name::#fk_const.eq(__pk))
                    }
                }
            }
        })
        .collect();

    // Gap 19: emit the `pk_i64` override only when the model's PK is
    // `i64`. Non-i64 PK models inherit the default (returns None) and
    // become a silent no-op for prefetch_related, matching the rest
    // of the v1 M2M-i64 constraint.
    let pk_i64_override: TokenStream2 = if pk_is_i64 {
        quote! {
            fn pk_i64(&self) -> ::core::option::Option<i64> {
                ::core::option::Option::Some(self.#pk_field_name)
            }
        }
    } else {
        quote! {}
    };

    let output = quote! {
        impl ::umbra::orm::Model for #struct_name {
            type PrimaryKey = #pk_ty_tokens;
            const NAME: &'static str = #struct_name_str;
            const TABLE: &'static str = #table_name;
            const FIELDS: &'static [::umbra::orm::FieldSpec] = &[
                #(#field_specs),*
            ];
            const DISPLAY: &'static str = #display_lit;
            const ICON: &'static str = #icon_lit;
            const DATABASE: ::core::option::Option<&'static str> = #database_tokens;
            const SINGLETON: bool = #singleton_lit;
            const SOFT_DELETE: bool = #soft_delete_lit;
            const UNIQUE_TOGETHER: &'static [&'static [&'static str]] = #unique_together_tokens;
            const INDEXES: &'static [&'static [&'static str]] = #indexes_tokens;
            const ORDERING: &'static [(&'static str, bool)] = #ordering_tokens;
            const M2M_RELATIONS: &'static [::umbra::orm::M2MRelationSpec] = &[
                #(#m2m_specs),*
            ];
            const REVERSE_FK_RELATIONS: &'static [::umbra::orm::ReverseFkRelationSpec] = &[
                #(#reverse_fk_specs),*
            ];
            const ONE_TO_ONE_RELATIONS: &'static [::umbra::orm::OneToOneRelationSpec] = &[
                #(#one_to_one_specs),*
            ];
            fn primary_key(&self) -> #pk_ty_tokens {
                // `.clone()` works for every PK type the trait accepts
                // (the bound is `Clone`, not `Copy`). For `i32`, `i64`,
                // `Uuid`, etc. the optimiser folds the clone back into
                // a copy; for `String` the clone is the work the call
                // site would have done anyway.
                self.#pk_field_name.clone()
            }
        }

        impl ::umbra::orm::HydrateRelated for #struct_name {
            fn fk_id_for(&self, field_name: &str) -> ::core::option::Option<i64> {
                match field_name {
                    #(#fk_id_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn hydrate_fk(&mut self, field_name: &str, row: &::umbra::_serde_json::Value) {
                match field_name {
                    #(#hydrate_arms)*
                    _ => {}
                }
            }
            fn set_m2m_parent_ids(&mut self) {
                // BUG-16: every M2M<U> field carries an Option<i64>
                // parent_id slot. Setting it from the parent's PK is
                // what lets `add`/`remove`/`clear` write the right
                // junction-table rows. The block is empty when the
                // model has no M2M fields — the trait's default
                // would do the same, but emitting the override
                // explicitly keeps the macro output uniform.
                #set_m2m_body
            }
            #pk_i64_override
            fn set_m2m_resolved_json(
                &mut self,
                field_name: &str,
                rows: ::std::vec::Vec<::umbra::_serde_json::Value>,
            ) {
                // Gap 19: prefetch_related populates each parent
                // row's M2M slot via this hook. The macro emits one
                // arm per M2M field on this model; per-row
                // deserialisation errors silently drop a row rather
                // than fail the prefetch (same forgive-and-continue
                // posture as hydrate_fk).
                match field_name {
                    #(#m2m_resolved_arms)*
                    _ => {}
                }
            }
            fn set_reverse_fk_resolved_json(
                &mut self,
                field_name: &str,
                rows: ::std::vec::Vec<::umbra::_serde_json::Value>,
            ) {
                // Gap #44: prefetch_related's reverse-FK path calls
                // this hook with each parent's bucket of children.
                // The macro emits one arm per `ReverseSet<C>` field.
                match field_name {
                    #(#reverse_fk_resolved_arms)*
                    _ => {}
                }
            }
            fn set_one_to_one_resolved_json(
                &mut self,
                field_name: &str,
                row: ::core::option::Option<::umbra::_serde_json::Value>,
            ) {
                // OneToOne reverse path: prefetch_related feeds at
                // most one child JSON object (or None for "loaded
                // but no row"). The macro emits one arm per
                // `OneToOne<C>` field.
                match field_name {
                    #(#one_to_one_resolved_arms)*
                    _ => {}
                }
            }
        }

        impl #struct_name {
            pub fn objects() -> ::umbra::orm::Manager<Self> {
                ::umbra::orm::Manager::default()
            }

            #(#m2m_helper_methods)*
        }

        // Reverse-FK accessors emitted on each FK target (gap #30).
        // One inherent-impl block per FK on this Child; multiple FKs
        // to the same parent are disambiguated with the field name.
        #(#reverse_fk_impls)*

        #[allow(clippy::module_inception)]
        pub mod #module_name {
            use super::#struct_name;

            #(#column_consts)*
        }
    };

    Ok(output)
}

/// The classification a field's Rust type lands in for M3 (extended at gap 14
/// with `ForeignKey`).
///
/// This is the single switchboard for the type → column-type mapping.
/// The new column type lands here, its SqlType picks up an arm in
/// `sql_type_tokens`, and the column const expansion picks up an arm in
/// `column_const_for`. The full M3 catalogue:
///
/// | Rust field type                          | FieldKind             | SqlType       | Column type                  |
/// |------------------------------------------|-----------------------|---------------|------------------------------|
/// | `i8` / `i16` / `u8`                      | `SmallInt`            | `SmallInt`    | `IntCol<Self>`               |
/// | `i32` / `u16`                            | `Integer`             | `Integer`     | `IntCol<Self>`               |
/// | `i64` / `u32`                            | `BigInt`              | `BigInt`      | `IntCol<Self>`               |
/// | `f32`                                    | `Real`                | `Real`        | `F64Col<Self>`               |
/// | `f64`                                    | `Double`              | `Double`      | `F64Col<Self>`               |
/// | `bool`                                   | `Bool`                | `Boolean`     | `BoolCol<Self>`              |
/// | `String`                                 | `Str`                 | `Text`        | `StrCol<Self>`               |
/// | `chrono::NaiveDate`                      | `Date`                | `Date`        | `DateCol<Self>`              |
/// | `chrono::NaiveTime`                      | `Time`                | `Time`        | `TimeCol<Self>`              |
/// | `chrono::DateTime<chrono::Utc>`          | `DateTime`            | `Timestamptz` | `DateTimeCol<Self>`          |
/// | `uuid::Uuid`                             | `Uuid`                | `Uuid`        | `UuidCol<Self>`              |
/// | `Option<i8>` / `i16` / `u8`              | `NullableSmallInt`    | `SmallInt`    | `NullableIntCol<Self>`       |
/// | `Option<i32>` / `u16`                    | `NullableInteger`     | `Integer`     | `NullableIntCol<Self>`       |
/// | `Option<i64>` / `u32`                    | `NullableBigInt`      | `BigInt`      | `NullableIntCol<Self>`       |
/// | `Option<f32>`                            | `NullableReal`        | `Real`        | `NullableF64Col<Self>`       |
/// | `Option<f64>`                            | `NullableDouble`      | `Double`      | `NullableF64Col<Self>`       |
/// | `Option<bool>`                           | `NullableBool`        | `Boolean`     | `NullableBoolCol<Self>`      |
/// | `Option<String>`                         | `NullableStr`         | `Text`        | `NullableStrCol<Self>`       |
/// | `Option<chrono::NaiveDate>`              | `NullableDate`        | `Date`        | `NullableDateCol<Self>`      |
/// | `Option<chrono::NaiveTime>`              | `NullableTime`        | `Time`        | `NullableTimeCol<Self>`      |
/// | `Option<chrono::DateTime<chrono::Utc>>`  | `NullableDateTime`    | `Timestamptz` | `NullableDateTimeCol<Self>`  |
/// | `Option<uuid::Uuid>`                     | `NullableUuid`        | `Uuid`        | `NullableUuidCol<Self>`      |
/// | `i128` / `u64` / `u128` / anything else  | `Unsupported(...)`    | (error)       | (error)                      |
#[allow(dead_code)] // Cidr / NullableCidr are matched but the derive
// doesn't yet emit them (Inet is the default for
// `ipnetwork::IpNetwork`; Cidr opt-in via
// `#[umbra(cidr)]` attribute is a follow-on).
enum FieldKind {
    SmallInt,
    Integer,
    BigInt,
    Real,
    Double,
    Bool,
    Str,
    Date,
    Time,
    DateTime,
    Uuid,
    NullableSmallInt,
    NullableInteger,
    NullableBigInt,
    NullableReal,
    NullableDouble,
    NullableBool,
    NullableStr,
    NullableDate,
    NullableTime,
    NullableDateTime,
    NullableUuid,
    /// `serde_json::Value` — a JSON document. Backed by Postgres
    /// JSONB or SQLite TEXT depending on the active backend; the
    /// derive doesn't care which.
    Json,
    NullableJson,
    /// `Vec<T>` where `T` is one of the [`ArrayElementKind`] variants —
    /// a Postgres array column. The field.backend system check fires at
    /// boot when this lands on SQLite.
    Array(ArrayElementKind),
    NullableArray(ArrayElementKind),
    /// `ForeignKey<T>` — an i64 FK reference to model `T`'s primary key.
    /// The inner `Type` is the generic argument `T`, used to derive
    /// `T::TABLE` for the `FieldSpec.fk_target` slot.
    ForeignKey(Box<Type>),
    /// `Option<ForeignKey<T>>` — a nullable FK column.
    NullableForeignKey(Box<Type>),
    /// `MultiChoice<E>` — a CSV-encoded list of `ChoiceField` variants.
    /// Stored as TEXT; the inner `Type` is `E`, used to pull the
    /// `VALUES` / `LABELS` slices off the trait at derive time.
    MultiChoice(Box<Type>),
    /// `M2M<T>` — many-to-many relation. No column on the parent table;
    /// the migration engine auto-generates a junction table. The inner
    /// `Type` is the target model `T`. Closes BUG-16.
    Many2Many(Box<Type>),
    /// `ReverseSet<C>` — reverse-FK collection. No column on the
    /// parent table; the child has a FK column pointing back. The
    /// `#[umbra(reverse_fk = "<fk_col>")]` attribute names that
    /// column. The macro emits one `ReverseFkRelationSpec` entry +
    /// the hydration arms. Closes gap #44.
    ReverseSet(Box<Type>),
    /// `OneToOne<C>` — reverse OneToOne accessor. No column on the
    /// parent table; the child has a UNIQUE FK column pointing
    /// back. Unlike ReverseSet, no `#[umbra(...)]` attribute is
    /// needed — the macro resolves the back-pointing column at
    /// runtime by scanning the child's FIELDS for the UNIQUE FK.
    OneToOne(Box<Type>),
    /// `ipnetwork::IpNetwork` — Postgres INET column (Phase 4.4).
    Inet,
    NullableInet,
    /// `ipnetwork::IpNetwork` declared as a CIDR — same Rust type as
    /// Inet but with the constraint that host bits are zero. The
    /// derive picks Inet by default; the `#[umbra(cidr)]` field-level
    /// attribute switches to Cidr (deferred — for now users emit Cidr
    /// fields by writing the FieldSpec by hand or via an
    /// inspectdb-generated `models.rs`).
    Cidr,
    NullableCidr,
    /// `mac_address::MacAddress` — Postgres MACADDR column.
    MacAddr,
    NullableMacAddr,
    /// `umbra::orm::TsVector` — Postgres full-text-search tsvector
    /// column (Phase 4.3).
    FullText,
    NullableFullText,
    /// `Vec<u8>` — BLOB on SQLite, BYTEA on Postgres. Detected before
    /// the array-element catalogue so `Vec<u8>` doesn't fall through
    /// into `Array(SmallInt)`.
    Bytes,
    NullableBytes,
    /// `rust_decimal::Decimal` — NUMERIC(19, 4) fixed-point. Closes
    /// BUG-10.
    Decimal,
    /// `umbra::orm::Slug` — TEXT with `[A-Za-z0-9_-]+` validation.
    /// Closes BUG-11. Storage is the inner String; the `text_format`
    /// marker on FieldSpec carries the validator selector.
    Slug,
    /// `umbra::orm::Email` — TEXT with structural email validation.
    /// Closes BUG-12.
    Email,
    /// `umbra::orm::Url` — TEXT with `http(s)://...` validation.
    /// Closes BUG-13.
    Url,
    /// Catch-all: not a recognised M3 catalogue type, or one of the
    /// explicitly-rejected wide / unsigned ints. Carries the exact
    /// diagnostic to emit at the field's span.
    Unsupported(UnsupportedReason),
}

/// Element kinds the derive recognises inside `Vec<T>`. Mirrors the
/// `umbra::orm::ArrayElement` enum the framework re-exports — the
/// macro can't reach into `umbra-core` at expand time so the catalogue
/// is duplicated, with the `sql_type_tokens` body emitting the right
/// `ArrayElement::Foo` for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayElementKind {
    SmallInt,
    Integer,
    BigInt,
    Real,
    Double,
    Boolean,
    Text,
    Uuid,
}

/// Why a field was rejected. Splits the catch-all so the diagnostic
/// can point the user at the right answer (use `i64` instead of `u64`,
/// or look at the catalogue table for an exotic type).
enum UnsupportedReason {
    /// `i128`, `u64`, `u128`, including their `Option<...>` wrappers.
    /// Worth a specific message because the issue is "no SQL backend
    /// handles this natively," not "type unrecognised."
    WideOrUnsignedInt,
    /// Everything else off the catalogue. Generic message pointing at
    /// spec 04.
    NotInCatalogue,
    /// Same as `NotInCatalogue` but the user wrote `Option<T>` of a
    /// recognised base type, so we can be slightly more specific. Kept
    /// as a separate variant so the error wording can be tuned without
    /// reshuffling the main switch.
    NullableOfWide,
}

impl FieldKind {
    /// The `(SqlType expression, nullable bool literal)` to splice into
    /// a `FieldSpec` for this kind, or `None` for `Unsupported`.
    fn sql_type_tokens(&self) -> Option<(TokenStream2, TokenStream2)> {
        let sql = match self {
            FieldKind::SmallInt | FieldKind::NullableSmallInt => {
                quote!(::umbra::orm::SqlType::SmallInt)
            }
            FieldKind::Integer | FieldKind::NullableInteger => {
                quote!(::umbra::orm::SqlType::Integer)
            }
            FieldKind::BigInt | FieldKind::NullableBigInt => quote!(::umbra::orm::SqlType::BigInt),
            FieldKind::Real | FieldKind::NullableReal => quote!(::umbra::orm::SqlType::Real),
            FieldKind::Double | FieldKind::NullableDouble => quote!(::umbra::orm::SqlType::Double),
            FieldKind::Bool | FieldKind::NullableBool => quote!(::umbra::orm::SqlType::Boolean),
            FieldKind::Str | FieldKind::NullableStr => quote!(::umbra::orm::SqlType::Text),
            // BUG-11/12/13: validator wrappers store as TEXT. The
            // FieldSpec.text_format marker (emitted in the field
            // loop below) tells downstream consumers which
            // validator runs / which OpenAPI format to emit.
            FieldKind::Slug | FieldKind::Email | FieldKind::Url => {
                quote!(::umbra::orm::SqlType::Text)
            }
            FieldKind::Date | FieldKind::NullableDate => quote!(::umbra::orm::SqlType::Date),
            FieldKind::Time | FieldKind::NullableTime => quote!(::umbra::orm::SqlType::Time),
            FieldKind::DateTime | FieldKind::NullableDateTime => {
                quote!(::umbra::orm::SqlType::Timestamptz)
            }
            FieldKind::Uuid | FieldKind::NullableUuid => quote!(::umbra::orm::SqlType::Uuid),
            FieldKind::Json | FieldKind::NullableJson => quote!(::umbra::orm::SqlType::Json),
            FieldKind::Array(elem) | FieldKind::NullableArray(elem) => {
                let elem_tokens = array_element_tokens(*elem);
                quote!(::umbra::orm::SqlType::Array(#elem_tokens))
            }
            FieldKind::Inet | FieldKind::NullableInet => quote!(::umbra::orm::SqlType::Inet),
            FieldKind::Cidr | FieldKind::NullableCidr => quote!(::umbra::orm::SqlType::Cidr),
            FieldKind::MacAddr | FieldKind::NullableMacAddr => {
                quote!(::umbra::orm::SqlType::MacAddr)
            }
            FieldKind::FullText | FieldKind::NullableFullText => {
                quote!(::umbra::orm::SqlType::FullText)
            }
            FieldKind::ForeignKey(_) | FieldKind::NullableForeignKey(_) => {
                quote!(::umbra::orm::SqlType::ForeignKey)
            }
            FieldKind::MultiChoice(_) => quote!(::umbra::orm::SqlType::Text),
            FieldKind::Bytes | FieldKind::NullableBytes => quote!(::umbra::orm::SqlType::Bytes),
            FieldKind::Decimal => quote!(::umbra::orm::SqlType::Decimal),
            // BUG-16: M2M fields have no column on the parent table. They
            // are skipped before reaching this point; the arm exists only
            // to keep the match exhaustive.
            FieldKind::Many2Many(_) => return None,
            FieldKind::ReverseSet(_) => return None,
            FieldKind::OneToOne(_) => return None,
            FieldKind::Unsupported(_) => return None,
        };
        let nullable = if self.is_nullable() {
            quote!(true)
        } else {
            quote!(false)
        };
        Some((sql, nullable))
    }

    fn is_nullable(&self) -> bool {
        matches!(
            self,
            FieldKind::NullableSmallInt
                | FieldKind::NullableInteger
                | FieldKind::NullableBigInt
                | FieldKind::NullableReal
                | FieldKind::NullableDouble
                | FieldKind::NullableBool
                | FieldKind::NullableStr
                | FieldKind::NullableDate
                | FieldKind::NullableTime
                | FieldKind::NullableDateTime
                | FieldKind::NullableUuid
                | FieldKind::NullableJson
                | FieldKind::NullableArray(_)
                | FieldKind::NullableInet
                | FieldKind::NullableCidr
                | FieldKind::NullableMacAddr
                | FieldKind::NullableFullText
                | FieldKind::NullableForeignKey(_)
                | FieldKind::NullableBytes
        )
    }

    /// The diagnostic to emit when this kind is `Unsupported`. Returns
    /// a fixed `&'static str`; non-`Unsupported` kinds shouldn't reach
    /// here, so they get a placeholder.
    fn error_message(&self) -> &'static str {
        match self {
            FieldKind::Unsupported(UnsupportedReason::WideOrUnsignedInt) => {
                "umbra M3 doesn't support 128-bit ints or u64 (no SQL backend handles \
                 them natively); use i64 or u32"
            }
            FieldKind::Unsupported(UnsupportedReason::NullableOfWide) => {
                "umbra M3 doesn't support 128-bit ints or u64 (no SQL backend handles \
                 them natively); use Option<i64> or Option<u32>"
            }
            FieldKind::Unsupported(UnsupportedReason::NotInCatalogue) => {
                "umbra M3 doesn't yet support this field type; see \
                 docs/specs/04-orm-model-and-fields.md for the M3 type catalogue"
            }
            _ => "unreachable: error_message called on a supported FieldKind",
        }
    }
}

/// Inspect a `syn::Type` and pick its `FieldKind`.
///
/// Type detection here is name-based: a path's *last* segment ident is
/// what matters. That means the derive sees through `chrono::DateTime`,
/// `DateTime`, and `::chrono::DateTime` identically — the user can write
/// any of them and the derive does the right thing. Same trick for
/// `uuid::Uuid`, `chrono::NaiveDate`, and `chrono::NaiveTime`.
fn classify_field_type(ty: &Type) -> FieldKind {
    // Plain primitives first. The catalogue spells out which Rust int
    // widths land in which SqlType slot; smaller unsigned ints fold up
    // into the next larger signed slot (u8 -> SmallInt, u16 -> Integer,
    // u32 -> BigInt).
    if type_is_ident(ty, "i8") || type_is_ident(ty, "i16") || type_is_ident(ty, "u8") {
        return FieldKind::SmallInt;
    }
    if type_is_ident(ty, "i32") || type_is_ident(ty, "u16") {
        return FieldKind::Integer;
    }
    if type_is_ident(ty, "i64") || type_is_ident(ty, "u32") {
        return FieldKind::BigInt;
    }
    if type_is_ident(ty, "f32") {
        return FieldKind::Real;
    }
    if type_is_ident(ty, "f64") {
        return FieldKind::Double;
    }
    if type_is_ident(ty, "bool") {
        return FieldKind::Bool;
    }
    if type_is_ident(ty, "String") {
        return FieldKind::Str;
    }
    // BUG-11/12/13: the validator wrappers. All three lower to
    // `SqlType::Text` (the storage shape is plain TEXT); the
    // `text_format` marker carries the discrimination through to
    // OpenAPI / REST / the admin form.
    if type_is_ident(ty, "Slug") {
        return FieldKind::Slug;
    }
    if type_is_ident(ty, "Email") {
        return FieldKind::Email;
    }
    if type_is_ident(ty, "Url") {
        return FieldKind::Url;
    }
    if type_is_ident(ty, "NaiveDate") {
        return FieldKind::Date;
    }
    if type_is_ident(ty, "NaiveTime") {
        return FieldKind::Time;
    }
    if is_datetime_utc(ty) {
        return FieldKind::DateTime;
    }
    if type_is_ident(ty, "Uuid") {
        return FieldKind::Uuid;
    }
    // `serde_json::Value` (the catalogue type for the Json variant).
    // Match by leaf ident so both bare `Value` (the local re-export) and
    // qualified `serde_json::Value` lower to `FieldKind::Json`.
    if is_serde_json_value(ty) {
        return FieldKind::Json;
    }
    // `Vec<u8>` is BLOB / BYTEA, not an array of small ints. Check
    // before the array catalogue so the `u8` element doesn't fall into
    // `Array(SmallInt)`.
    if is_vec_u8(ty) {
        return FieldKind::Bytes;
    }
    // `Vec<T>` for the Phase 4.1 Array catalogue. Postgres-only at
    // runtime; the field.backend system check fires at boot when
    // a model with an Array field is registered against SQLite.
    if let Some(kind) = vec_element_kind(ty) {
        return FieldKind::Array(kind);
    }
    // Phase 4.4 network-address types. `ipnetwork::IpNetwork` is the
    // Rust binding for both INET and CIDR; the default classification
    // is `Inet` (the more general type — host addresses with optional
    // netmask). Users who need the CIDR constraint switch via the
    // future `#[umbra(cidr)]` attribute or by writing the FieldSpec
    // by hand. `mac_address::MacAddress` covers MACADDR.
    if is_ipnetwork(ty) {
        return FieldKind::Inet;
    }
    if is_mac_address(ty) {
        return FieldKind::MacAddr;
    }
    // Phase 4.3 — `umbra::orm::TsVector`. The qualifier is `orm`
    // (matching the umbra facade re-export). Bare `TsVector` won't
    // pick up, same as the other qualified-leaf checks.
    if is_tsvector(ty) {
        return FieldKind::FullText;
    }
    if is_decimal(ty) {
        return FieldKind::Decimal;
    }
    // Gap 14 — `ForeignKey<T>`. Detected by the leaf ident `ForeignKey`
    // with exactly one generic type argument `T`. The qualifier check
    // accepts either `orm::ForeignKey` or bare `ForeignKey` to let user
    // code write `ForeignKey<User>` after `use umbra::orm::ForeignKey`.
    if let Some(inner) = foreign_key_inner(ty) {
        return FieldKind::ForeignKey(Box::new(inner.clone()));
    }
    // Gap 52 — `MultiChoice<E>`. Same leaf-ident matching as
    // `ForeignKey<T>`: bare `MultiChoice<E>` or `orm::MultiChoice<E>`
    // both work. The inner type must itself be `ChoiceField` at use
    // site; the macro only checks the path shape — the trait bound is
    // enforced when the emitted `<E as ChoiceField>::VALUES` reference
    // is type-checked.
    if let Some(inner) = multichoice_inner(ty) {
        return FieldKind::MultiChoice(Box::new(inner.clone()));
    }
    // BUG-16 — `M2M<T>`. Same leaf-ident matching as `ForeignKey<T>`.
    // Bare `M2M<T>` or `orm::M2M<T>` both work. No column on the
    // parent table; the migration engine auto-generates a junction table.
    if let Some(inner) = m2m_inner(ty) {
        return FieldKind::Many2Many(Box::new(inner.clone()));
    }
    // Gap #44 — `ReverseSet<C>` is a reverse-FK collection on the
    // parent. Same "no column on the parent table" shape as M2M.
    if let Some(inner) = reverse_set_inner(ty) {
        return FieldKind::ReverseSet(Box::new(inner.clone()));
    }
    if let Some(inner) = one_to_one_inner(ty) {
        return FieldKind::OneToOne(Box::new(inner.clone()));
    }
    if is_wide_or_unsigned_int(ty) {
        return FieldKind::Unsupported(UnsupportedReason::WideOrUnsignedInt);
    }

    if let Some(inner) = option_inner(ty) {
        // Gap 85 — `Option<M2M<T>>`. M2M is a relation, not a column;
        // there's no nullable variant because the junction-table model
        // already represents "no relation" as the absence of a row.
        // We treat `Option<M2M<T>>` exactly like `M2M<T>` so authors
        // who reflexively wrap relations in `Option` (as you would
        // for a nullable FK) don't hit an "unsupported type" error.
        if let Some(m2m_target) = m2m_inner(inner) {
            return FieldKind::Many2Many(Box::new(m2m_target.clone()));
        }
        if type_is_ident(inner, "i8") || type_is_ident(inner, "i16") || type_is_ident(inner, "u8") {
            return FieldKind::NullableSmallInt;
        }
        if type_is_ident(inner, "i32") || type_is_ident(inner, "u16") {
            return FieldKind::NullableInteger;
        }
        if type_is_ident(inner, "i64") || type_is_ident(inner, "u32") {
            return FieldKind::NullableBigInt;
        }
        if type_is_ident(inner, "f32") {
            return FieldKind::NullableReal;
        }
        if type_is_ident(inner, "f64") {
            return FieldKind::NullableDouble;
        }
        if type_is_ident(inner, "bool") {
            return FieldKind::NullableBool;
        }
        if type_is_ident(inner, "String") {
            return FieldKind::NullableStr;
        }
        if type_is_ident(inner, "NaiveDate") {
            return FieldKind::NullableDate;
        }
        if type_is_ident(inner, "NaiveTime") {
            return FieldKind::NullableTime;
        }
        if is_datetime_utc(inner) {
            return FieldKind::NullableDateTime;
        }
        if type_is_ident(inner, "Uuid") {
            return FieldKind::NullableUuid;
        }
        if is_serde_json_value(inner) {
            return FieldKind::NullableJson;
        }
        if is_vec_u8(inner) {
            return FieldKind::NullableBytes;
        }
        if let Some(kind) = vec_element_kind(inner) {
            return FieldKind::NullableArray(kind);
        }
        if is_ipnetwork(inner) {
            return FieldKind::NullableInet;
        }
        if is_mac_address(inner) {
            return FieldKind::NullableMacAddr;
        }
        if is_tsvector(inner) {
            return FieldKind::NullableFullText;
        }
        if let Some(fk_inner) = foreign_key_inner(inner) {
            return FieldKind::NullableForeignKey(Box::new(fk_inner.clone()));
        }
        if is_wide_or_unsigned_int(inner) {
            return FieldKind::Unsupported(UnsupportedReason::NullableOfWide);
        }
        return FieldKind::Unsupported(UnsupportedReason::NotInCatalogue);
    }

    FieldKind::Unsupported(UnsupportedReason::NotInCatalogue)
}

/// If `ty` is `ForeignKey<T>` (with or without the `orm::` qualifier),
/// return the inner type `T`. Returns `None` for any other type.
///
/// Matches the leaf segment ident `ForeignKey` with exactly one generic
/// type argument. We don't require the `orm::` qualifier so user code
/// that writes `use umbra::orm::ForeignKey; field: ForeignKey<User>` works.
fn foreign_key_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "ForeignKey" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None; // more than one type arg — not our ForeignKey
    }
    Some(inner)
}

/// If `ty` is `MultiChoice<E>` (with or without the `orm::` qualifier),
/// return the inner enum type `E`. Returns `None` otherwise. The shape
/// check mirrors [`foreign_key_inner`] — leaf ident match plus a single
/// type argument.
fn multichoice_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "MultiChoice" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    Some(inner)
}

/// If `ty` is `OneToOne<C>`, return the inner child model type
/// `C`. Returns `None` otherwise. Mirrors [`m2m_inner`] /
/// [`reverse_set_inner`].
fn one_to_one_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "OneToOne" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    Some(inner)
}

/// If `ty` is `ReverseSet<C>` (gap #44), return the inner child
/// model type `C`. Returns `None` otherwise. Mirrors
/// [`m2m_inner`] — leaf ident `ReverseSet` plus one generic arg.
fn reverse_set_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "ReverseSet" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    Some(inner)
}

/// If `ty` is `M2M<T>` (with or without the `orm::` qualifier),
/// return the inner model type `T`. Returns `None` otherwise. Mirrors
/// [`foreign_key_inner`] — leaf ident `M2M` plus one generic arg.
fn m2m_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "M2M" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    Some(inner)
}

/// If `ty` is `Vec<T>` and `T` is one of the [`ArrayElementKind`]
/// Return `true` when `ty` is `Vec<u8>` specifically. Used to route
/// byte payloads to `SqlType::Bytes` before the array catalogue
/// classifies `u8` as a small int.
fn is_vec_u8(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    let Some(last) = path.segments.last() else {
        return false;
    };
    if last.ident != "Vec" {
        return false;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return false;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let Some(inner) = type_args.next() else {
        return false;
    };
    if type_args.next().is_some() {
        return false;
    }
    type_is_ident(inner, "u8")
}

/// catalogue types, return that kind. Returns `None` otherwise
/// (including for `Vec<i128>`, `Vec<Vec<T>>`, `Vec<Option<T>>`,
/// `Vec<NaiveDate>` — all currently off the catalogue).
fn vec_element_kind(ty: &Type) -> Option<ArrayElementKind> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "Vec" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    // Inner type catalogue. Keep these in lockstep with the
    // ArrayElementKind enum and the `umbra::orm::ArrayElement`
    // variants. Date/Time/Timestamptz/Json deliberately not yet
    // recognised — their cross-backend binding semantics need a
    // deliberate pass.
    //
    // `u8` is excluded here: `Vec<u8>` is BLOB / BYTEA (handled by
    // `is_vec_u8` and `FieldKind::Bytes`), not an array of small ints.
    if type_is_ident(inner, "i8") || type_is_ident(inner, "i16") {
        return Some(ArrayElementKind::SmallInt);
    }
    if type_is_ident(inner, "i32") || type_is_ident(inner, "u16") {
        return Some(ArrayElementKind::Integer);
    }
    if type_is_ident(inner, "i64") || type_is_ident(inner, "u32") {
        return Some(ArrayElementKind::BigInt);
    }
    if type_is_ident(inner, "f32") {
        return Some(ArrayElementKind::Real);
    }
    if type_is_ident(inner, "f64") {
        return Some(ArrayElementKind::Double);
    }
    if type_is_ident(inner, "bool") {
        return Some(ArrayElementKind::Boolean);
    }
    if type_is_ident(inner, "String") {
        return Some(ArrayElementKind::Text);
    }
    if type_is_ident(inner, "Uuid") {
        return Some(ArrayElementKind::Uuid);
    }
    None
}

/// The `ArrayElement::Foo` tokens for one `ArrayElementKind`. Used by
/// `sql_type_tokens` to splice the right variant under
/// `SqlType::Array(...)`.
fn array_element_tokens(kind: ArrayElementKind) -> TokenStream2 {
    match kind {
        ArrayElementKind::SmallInt => quote!(::umbra::orm::ArrayElement::SmallInt),
        ArrayElementKind::Integer => quote!(::umbra::orm::ArrayElement::Integer),
        ArrayElementKind::BigInt => quote!(::umbra::orm::ArrayElement::BigInt),
        ArrayElementKind::Real => quote!(::umbra::orm::ArrayElement::Real),
        ArrayElementKind::Double => quote!(::umbra::orm::ArrayElement::Double),
        ArrayElementKind::Boolean => quote!(::umbra::orm::ArrayElement::Boolean),
        ArrayElementKind::Text => quote!(::umbra::orm::ArrayElement::Text),
        ArrayElementKind::Uuid => quote!(::umbra::orm::ArrayElement::Uuid),
    }
}

/// True when `ty` is `ipnetwork::IpNetwork`. Phase 4.4 INET / CIDR
/// catalogue type. Matches the leaf ident `IpNetwork` with the
/// qualifier `ipnetwork` to avoid colliding with other crates that
/// might also define a leaf type called `IpNetwork`.
fn is_ipnetwork(ty: &Type) -> bool {
    is_qualified_leaf(ty, "ipnetwork", "IpNetwork")
}

/// True when `ty` is `mac_address::MacAddress`. Phase 4.4 MACADDR
/// catalogue type.
fn is_mac_address(ty: &Type) -> bool {
    is_qualified_leaf(ty, "mac_address", "MacAddress")
}

/// True when `ty` is `umbra::orm::TsVector` (or `orm::TsVector` with
/// the facade re-export). Phase 4.3 FullText catalogue type. The
/// qualifier check accepts either `orm` (when user writes
/// `umbra::orm::TsVector`) — i.e. the leaf parent matches `orm`.
fn is_tsvector(ty: &Type) -> bool {
    is_qualified_leaf(ty, "orm", "TsVector")
}

/// True when `ty` is `rust_decimal::Decimal`. Closes BUG-10 from
/// `bugs/tests/testBugs.md`. The qualifier `rust_decimal` is part
/// of the match so a bare `Decimal` (which could be another
/// crate's type) doesn't get auto-classified.
fn is_decimal(ty: &Type) -> bool {
    is_qualified_leaf(ty, "rust_decimal", "Decimal")
}

/// True when `ty` is a path ending in `qualifier::leaf` with no
/// generic arguments on the leaf. The qualifier check is positional
/// — the segment immediately before the leaf has to match. Used by
/// `is_serde_json_value`, `is_ipnetwork`, `is_mac_address` so they
/// all share one definition of "qualified-leaf match."
fn is_qualified_leaf(ty: &Type, qualifier: &str, leaf: &str) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    let segments: Vec<&syn::PathSegment> = path.segments.iter().collect();
    let Some(last) = segments.last() else {
        return false;
    };
    if last.ident != leaf || !matches!(last.arguments, PathArguments::None) {
        return false;
    }
    // The qualifier is required so a bare `IpNetwork` (which could
    // be from any crate) isn't silently misclassified. Users opt in
    // by writing `ipnetwork::IpNetwork` explicitly.
    if segments.len() < 2 {
        return false;
    }
    let prev = segments[segments.len() - 2];
    prev.ident == qualifier
}

/// True when `ty` is `serde_json::Value` (regardless of the path
/// prefix). The Phase 4 Json field type opts in via this type — bare
/// `Value` is too ambiguous to pick up automatically, so the leaf
/// segment has to be `Value` AND the segment before it (if any) has
/// to be `serde_json` or `json`. Conservatively false on a bare
/// `Value` ident with no qualifier; users opt in by writing
/// `serde_json::Value` in the struct definition.
fn is_serde_json_value(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    let segments: Vec<&syn::PathSegment> = path.segments.iter().collect();
    let Some(last) = segments.last() else {
        return false;
    };
    if last.ident != "Value" || !matches!(last.arguments, PathArguments::None) {
        return false;
    }
    // Require a path qualifier so the user wrote `serde_json::Value`
    // or `::serde_json::Value`. A bare `Value` is ambiguous (it could
    // be `bytes::Value`, `prost::Value`, etc.) and we don't want to
    // silently misclassify those.
    if segments.len() < 2 {
        return false;
    }
    let prev = segments[segments.len() - 2];
    prev.ident == "serde_json"
}

/// True for the int widths explicitly off the M3 catalogue: `i128`,
/// `u64`, `u128`. Used to pick the targeted diagnostic.
fn is_wide_or_unsigned_int(ty: &Type) -> bool {
    type_is_ident(ty, "i128") || type_is_ident(ty, "u64") || type_is_ident(ty, "u128")
}

/// True when `ty` is a path whose last segment ident equals `name` and
/// carries no generic arguments. Used for plain types like `i64`,
/// `String`, `Uuid`, `NaiveDate`.
fn type_is_ident(ty: &Type, name: &str) -> bool {
    if let Type::Path(TypePath { qself: None, path }) = ty {
        if let Some(last) = path.segments.last() {
            return last.ident == name && matches!(last.arguments, PathArguments::None);
        }
    }
    false
}

/// True when `ty` is `DateTime<Utc>` (regardless of the path prefix).
///
/// The derive only commits to recognising the `chrono::DateTime<chrono::Utc>`
/// shape — `DateTime<Local>` and `NaiveDateTime` aren't in the M3
/// catalogue. We check the outer last segment is `DateTime` with one
/// generic argument whose last segment is `Utc`.
fn is_datetime_utc(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    let Some(last) = path.segments.last() else {
        return false;
    };
    if last.ident != "DateTime" {
        return false;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return false;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let first = match type_args.next() {
        Some(t) => t,
        None => return false,
    };
    if type_args.next().is_some() {
        return false;
    }
    let Type::Path(TypePath { path: inner, .. }) = first else {
        return false;
    };
    inner
        .segments
        .last()
        .is_some_and(|s| s.ident == "Utc" && matches!(s.arguments, PathArguments::None))
}

/// If `ty` is `Option<T>` for some `T`, return a reference to that `T`.
///
/// Name-based the same way `is_datetime_utc` is: the last path segment
/// has to be `Option` with exactly one type argument. Aliased options
/// (e.g. `MyOpt<i64>`) don't match — that's the right call because the
/// derive doesn't know which aliases mean "nullable."
/// True when `ty` is syntactically `Option<...>`. Used to reject
/// `#[umbra(choices)] field: Option<T>` at derive time (v1 limitation).
fn is_option_type(ty: &Type) -> bool {
    option_inner(ty).is_some()
}

fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return None;
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next()?;
    if type_args.next().is_some() {
        return None;
    }
    Some(inner)
}

/// Build the `pub const FOO: ::umbra::orm::column::FooCol<Self> =
/// FooCol::new("foo");` declaration for one field.
///
/// The const name is `SCREAMING_SNAKE_CASE(field_name)`. The column type
/// is chosen by `FieldKind`. Column type idents are produced via
/// `format_ident!` and spliced into a fully-qualified `::umbra::orm::column::...`
/// path so the emitted module needs no `use` imports — every plugin or
/// user crate that derives `Model` gets the same path resolution.
fn column_const_for(
    struct_name: &syn::Ident,
    field_name: &str,
    field: &Field,
    kind: &FieldKind,
) -> TokenStream2 {
    let const_ident = format_ident!("{}", to_screaming_snake_case(field_name));
    let span = field.ty.span();
    let col_ident = match kind {
        FieldKind::SmallInt | FieldKind::Integer | FieldKind::BigInt => format_ident!("IntCol"),
        FieldKind::Real | FieldKind::Double => format_ident!("F64Col"),
        FieldKind::Bool => format_ident!("BoolCol"),
        FieldKind::Str => format_ident!("StrCol"),
        // BUG-11/12/13: the validator wrappers expose the same
        // text-column query surface (`eq`, `ilike`, `contains`,
        // etc.) as a plain `String` field; reuse `StrCol`.
        FieldKind::Slug | FieldKind::Email | FieldKind::Url => format_ident!("StrCol"),
        FieldKind::Date => format_ident!("DateCol"),
        FieldKind::Time => format_ident!("TimeCol"),
        FieldKind::DateTime => format_ident!("DateTimeCol"),
        FieldKind::Uuid => format_ident!("UuidCol"),
        FieldKind::NullableSmallInt | FieldKind::NullableInteger | FieldKind::NullableBigInt => {
            format_ident!("NullableIntCol")
        }
        FieldKind::NullableReal | FieldKind::NullableDouble => format_ident!("NullableF64Col"),
        FieldKind::NullableBool => format_ident!("NullableBoolCol"),
        FieldKind::NullableStr => format_ident!("NullableStrCol"),
        FieldKind::NullableDate => format_ident!("NullableDateCol"),
        FieldKind::NullableTime => format_ident!("NullableTimeCol"),
        FieldKind::NullableDateTime => format_ident!("NullableDateTimeCol"),
        FieldKind::NullableUuid => format_ident!("NullableUuidCol"),
        FieldKind::Json => format_ident!("JsonCol"),
        FieldKind::NullableJson => format_ident!("NullableJsonCol"),
        FieldKind::Array(_) => format_ident!("ArrayCol"),
        FieldKind::NullableArray(_) => format_ident!("NullableArrayCol"),
        FieldKind::Inet => format_ident!("InetCol"),
        FieldKind::NullableInet => format_ident!("NullableInetCol"),
        FieldKind::Cidr => format_ident!("CidrCol"),
        FieldKind::NullableCidr => format_ident!("NullableCidrCol"),
        FieldKind::MacAddr => format_ident!("MacAddrCol"),
        FieldKind::NullableMacAddr => format_ident!("NullableMacAddrCol"),
        FieldKind::FullText => format_ident!("FullTextCol"),
        FieldKind::NullableFullText => format_ident!("NullableFullTextCol"),
        FieldKind::ForeignKey(_) => format_ident!("ForeignKeyCol"),
        FieldKind::NullableForeignKey(_) => format_ident!("NullableForeignKeyCol"),
        FieldKind::Bytes => format_ident!("BytesCol"),
        FieldKind::NullableBytes => format_ident!("NullableBytesCol"),
        FieldKind::Decimal => format_ident!("DecimalCol"),
        // MultiChoice and Many2Many are handled inline by the caller,
        // so these arms are unreachable in practice. We return an empty
        // token stream as a defensive default.
        FieldKind::MultiChoice(_) => return TokenStream2::new(),
        FieldKind::Many2Many(_) => return TokenStream2::new(),
        FieldKind::ReverseSet(_) => return TokenStream2::new(),
        FieldKind::OneToOne(_) => return TokenStream2::new(),
        FieldKind::Unsupported(_) => return TokenStream2::new(),
    };
    quote_spanned! { span =>
        pub const #const_ident: ::umbra::orm::column::#col_ident<super::#struct_name> =
            ::umbra::orm::column::#col_ident::new(#field_name);
    }
}

/// Convert `CamelCase` / `PascalCase` to `snake_case`.
///
/// Rules: insert `_` before any uppercase letter that follows a
/// lowercase letter or a digit, and before the last letter of an
/// uppercase run that's followed by a lowercase letter (so `HTTPRequest`
/// becomes `http_request`, not `httprequest` or `h_t_t_p_request`). All
/// ASCII; non-ASCII characters pass through unchanged. Underscores
/// already in the input are preserved.
fn to_snake_case(camel: &str) -> String {
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

/// Convert `snake_case` (or anything mixed) to `SCREAMING_SNAKE_CASE`.
///
/// Routes through `to_snake_case` first so a struct field accidentally
/// named in camelCase (`publishedAt`) still produces `PUBLISHED_AT`. All
/// ASCII; non-ASCII characters pass through, uppercased where possible.
fn to_screaming_snake_case(name: &str) -> String {
    to_snake_case(name).to_ascii_uppercase()
}

/// Convert `snake_case` (or anything mixed) to `PascalCase`.
///
/// Routes through `to_snake_case` so a struct field already written in
/// PascalCase or camelCase round-trips cleanly. Used to build trait
/// names like `<Child><Field>Reverse` for gap 105's reverse-FK
/// emission. Non-ASCII pass through unchanged.
fn to_pascal_case(name: &str) -> String {
    let snake = to_snake_case(name);
    let mut out = String::with_capacity(snake.len());
    let mut capitalise_next = true;
    for c in snake.chars() {
        if c == '_' {
            capitalise_next = true;
            continue;
        }
        if capitalise_next {
            for u in c.to_uppercase() {
                out.push(u);
            }
            capitalise_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

// =========================================================================
// `#[derive(Form)]` (FEATURES.md follow-on). Lowers a struct + per-field
// `#[form(...)]` attrs into an `impl umbra::forms::Form`. The validate /
// render bodies call into the Field primitives from umbra-core::forms;
// the macro only does compile-time wiring.
// =========================================================================

/// Derive `umbra::forms::Form` on a struct of named fields.
///
/// Field-type dispatch:
///
/// - `String` -> `Field::text(name)` (or `email`/`password` per attr)
/// - `i32` / `i64` / `u32` / `u64` -> `Field::integer(name)`
/// - `f32` / `f64` -> `Field::integer(name)` (numeric, accepts decimals)
/// - `bool` -> `Field::boolean(name)`
/// - `Option<T>` -> the inner type, marked `.optional()`
///
/// Per-field attribute keys (all under `#[form(...)]`):
///
/// - `min_length = N` / `max_length = N` -> add MinLength/MaxLength
/// - `email` -> use `Field::email`, adds the EmailFormat check
/// - `password` -> use `Field::password` (renders as `type="password"`)
/// - `optional` -> skip Required (forced on for `Option<T>`)
#[proc_macro_derive(Form, attributes(form))]
pub fn derive_form(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_form(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Per-field options parsed from `#[form(...)]`.
#[derive(Default)]
struct FormFieldAttr {
    email: bool,
    password: bool,
    optional: bool,
    min_length: Option<usize>,
    max_length: Option<usize>,
}

fn parse_form_attrs(attrs: &[syn::Attribute]) -> syn::Result<FormFieldAttr> {
    let mut out = FormFieldAttr::default();
    for attr in attrs {
        if !attr.path().is_ident("form") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("email") {
                out.email = true;
                Ok(())
            } else if meta.path.is_ident("password") {
                out.password = true;
                Ok(())
            } else if meta.path.is_ident("optional") {
                out.optional = true;
                Ok(())
            } else if meta.path.is_ident("min_length") {
                let lit: syn::LitInt = meta.value()?.parse()?;
                out.min_length = Some(lit.base10_parse()?);
                Ok(())
            } else if meta.path.is_ident("max_length") {
                let lit: syn::LitInt = meta.value()?.parse()?;
                out.max_length = Some(lit.base10_parse()?);
                Ok(())
            } else {
                Err(meta.error(
                    "umbra::Form derive accepts `email`, `password`, \
                     `optional`, `min_length = N`, `max_length = N`",
                ))
            }
        })?;
    }
    Ok(out)
}

/// What kind of value the Rust field holds. Drives both the validator
/// builder selection AND the value-parsing code path.
#[derive(Clone, Copy)]
enum FormFieldKind {
    String,
    Integer,
    Float,
    Bool,
}

fn classify_form_field_type(ty: &syn::Type) -> Option<(FormFieldKind, bool)> {
    // Returns (kind, is_option). Option<T> peels one layer; the inner
    // type is what we classify against.
    if let Some(inner) = option_inner_type(ty) {
        let (kind, _) = classify_form_field_type(inner)?;
        return Some((kind, true));
    }
    if type_is_ident(ty, "String") {
        return Some((FormFieldKind::String, false));
    }
    if type_is_ident(ty, "i32")
        || type_is_ident(ty, "i64")
        || type_is_ident(ty, "u32")
        || type_is_ident(ty, "u64")
        || type_is_ident(ty, "i16")
        || type_is_ident(ty, "u16")
        || type_is_ident(ty, "i8")
        || type_is_ident(ty, "u8")
        || type_is_ident(ty, "isize")
        || type_is_ident(ty, "usize")
    {
        return Some((FormFieldKind::Integer, false));
    }
    if type_is_ident(ty, "f32") || type_is_ident(ty, "f64") {
        return Some((FormFieldKind::Float, false));
    }
    if type_is_ident(ty, "bool") {
        return Some((FormFieldKind::Bool, false));
    }
    None
}

fn option_inner_type(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    for arg in &args.args {
        if let syn::GenericArgument::Type(inner) = arg {
            return Some(inner);
        }
    }
    None
}

// =========================================================================
// `#[task]` attribute macro.
//
// Turns an `async fn name(payload: PayloadType) -> Result<(), String>`
// into:
//   1. The original `async fn name(...)` unchanged.
//   2. A companion `pub fn register_name()` that calls
//      `::umbra_tasks::register_handler("name", wrapper)` where `wrapper`
//      is a generated `async fn(payload_json: &str) -> Result<(), String>`
//      that JSON-deserialises the payload via `serde_json::from_str` and
//      forwards to `name`.
//
// Optional: `#[task(name = "dotted.name")]` overrides the handler name
// used for registration (default: the function's Rust identifier).
//
// Constraints:
//   - Must be `async fn`.
//   - Must take exactly one parameter (the typed payload).
//   - Must return `Result<(), String>`.
//   Violations emit `compile_error!` with a targeted message.
// =========================================================================

/// Mark an `async fn` as an umbra background task.
///
/// The macro emits the original function unchanged and a companion
/// `register_<fn_name>()` function that registers the task handler with
/// `umbra_tasks::register_handler`. Call the companion at boot time from
/// `Plugin::on_ready` or your `main` function.
///
/// ```ignore
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct WelcomeEmailPayload {
///     user_id: i64,
///     locale: String,
/// }
///
/// #[umbra::task]
/// async fn send_welcome(payload: WelcomeEmailPayload) -> Result<(), String> {
///     // ... real work
///     Ok(())
/// }
///
/// // At boot:
/// register_send_welcome();
/// ```
///
/// Override the task name (the key stored in the handler registry) with:
///
/// ```ignore
/// #[umbra::task(name = "blog.send_welcome")]
/// async fn send_welcome(p: WelcomeEmailPayload) -> Result<(), String> { ... }
/// ```
#[proc_macro_attribute]
pub fn task(args: TokenStream, input: TokenStream) -> TokenStream {
    expand_task(args.into(), input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Parse optional `name = "..."` from the attribute args.
struct TaskArgs {
    name_override: Option<String>,
}

fn parse_task_args(args: TokenStream2) -> syn::Result<TaskArgs> {
    let mut out = TaskArgs {
        name_override: None,
    };
    // Empty args are fine — default task name is the fn identifier.
    if args.is_empty() {
        return Ok(out);
    }
    // Parse as a single `name = "literal"` meta item.
    let meta: syn::MetaNameValue = syn::parse2(args.clone()).map_err(|_| {
        syn::Error::new_spanned(
            &args,
            "#[task] attribute accepts `name = \"...\"` or nothing",
        )
    })?;
    if !meta.path.is_ident("name") {
        return Err(syn::Error::new_spanned(
            &meta.path,
            "#[task] only supports `name = \"...\"` as an argument",
        ));
    }
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(lit_str),
        ..
    }) = &meta.value
    else {
        return Err(syn::Error::new_spanned(
            &meta.value,
            "#[task(name = ...)] requires a string literal",
        ));
    };
    out.name_override = Some(lit_str.value());
    Ok(out)
}

fn expand_task(args: TokenStream2, input: TokenStream2) -> syn::Result<TokenStream2> {
    let task_args = parse_task_args(args)?;

    // Parse the input as a function item.
    let func: ItemFn = syn::parse2(input.clone())
        .map_err(|e| syn::Error::new(e.span(), "#[task] can only be applied to functions"))?;

    // --- Constraint 1: must be async ---
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            func.sig.fn_token,
            "#[task] requires an `async fn`; the handler runs asynchronously in the worker",
        ));
    }

    // --- Constraint 2: exactly one parameter ---
    let params: Vec<&syn::FnArg> = func.sig.inputs.iter().collect();
    if params.len() != 1 {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            format!(
                "#[task] requires exactly one parameter (the typed payload); \
                 found {} parameter(s)",
                params.len()
            ),
        ));
    }

    // Extract the parameter type so we can generate the wrapper.
    let payload_ty = match params[0] {
        syn::FnArg::Typed(pat_ty) => &*pat_ty.ty,
        syn::FnArg::Receiver(_) => {
            return Err(syn::Error::new_spanned(
                params[0],
                "#[task] cannot be applied to a method (self parameter is not allowed)",
            ));
        }
    };

    // --- Constraint 3: must return Result<(), String> ---
    let is_correct_return = match &func.sig.output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => is_result_unit_string(ty),
    };
    if !is_correct_return {
        return Err(syn::Error::new_spanned(
            &func.sig.output,
            "#[task] requires `-> Result<(), String>` as the return type (matches \
             the umbra-tasks handler contract)",
        ));
    }

    let fn_name = &func.sig.ident;
    let fn_name_str = fn_name.to_string();
    let task_name = task_args.name_override.as_deref().unwrap_or(&fn_name_str);

    // Generated companion: `pub fn register_<fn_name>()`
    let register_fn_name = format_ident!("register_{}", fn_name);

    // The wrapper closes over nothing — it just deserialises and calls
    // the original function. The handler registry stores `&'static str`
    // keys so we need a `&'static str` literal. Since `task_name` may
    // come from the attribute it could be a runtime String; we use a
    // string literal OR `Box::leak` for the override case.
    //
    // For the default (fn ident) case we emit a literal. For the
    // override case we need to produce a `'static str` at registration
    // time; `Box::leak(task_name.into_boxed_str())` does it cleanly and
    // the task name is a one-time-registration so the leak is
    // acceptable (same cost as a static).
    let task_name_tokens: TokenStream2 = {
        // Both branches emit a `&'static str` expression.
        let s = task_name;
        quote! { #s }
    };

    let output = quote! {
        // 1. Original function unchanged.
        #func

        // 2. Companion registration function.
        pub fn #register_fn_name() {
            ::umbra_tasks::register_handler(
                #task_name_tokens,
                |payload_json: &str| {
                    // Copy the payload string so the future can own it
                    // (register_handler requires 'static futures).
                    let owned = payload_json.to_owned();
                    async move {
                        let payload: #payload_ty =
                            ::umbra_tasks::_serde_json::from_str(&owned)
                                .map_err(|e| format!("payload deserialise error: {e}"))?;
                        #fn_name(payload).await
                    }
                },
            );
        }
    };

    Ok(output)
}

/// True when `ty` is `Result<(), String>` (any qualifier depth).
///
/// Checks: outer path last segment == `Result`, two generic args, first
/// is the unit type `()`, second is a path whose last segment is `String`.
fn is_result_unit_string(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    let Some(last) = path.segments.last() else {
        return false;
    };
    if last.ident != "Result" {
        return false;
    }
    let PathArguments::AngleBracketed(ref args) = last.arguments else {
        return false;
    };
    let type_args: Vec<&GenericArgument> = args.args.iter().collect();
    if type_args.len() != 2 {
        return false;
    }
    // First arg must be the unit type `()`.
    let GenericArgument::Type(ok_ty) = type_args[0] else {
        return false;
    };
    if !matches!(ok_ty, Type::Tuple(t) if t.elems.is_empty()) {
        return false;
    }
    // Second arg must be a path ending in `String`.
    let GenericArgument::Type(err_ty) = type_args[1] else {
        return false;
    };
    type_is_ident(err_ty, "String")
}

fn expand_form(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    struct_name,
                    "umbra::Form can only be derived on structs with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "umbra::Form can only be derived on structs with named fields",
            ));
        }
    };

    let mut field_builders: Vec<TokenStream2> = Vec::new();
    let mut validate_body: Vec<TokenStream2> = Vec::new();
    let mut struct_inits: Vec<TokenStream2> = Vec::new();

    for field in fields.iter() {
        let field_ident = field.ident.as_ref().unwrap();
        let field_name = field_ident.to_string();
        let attrs = parse_form_attrs(&field.attrs)?;
        let Some((kind, is_option)) = classify_form_field_type(&field.ty) else {
            return Err(syn::Error::new_spanned(
                &field.ty,
                "umbra::Form derive: unsupported field type. v1 accepts \
                 String, i8..i64 / u8..u64 / isize / usize, f32 / f64, bool, \
                 and Option<T> of any of those.",
            ));
        };
        let is_optional = is_option || attrs.optional;

        // Build the Field constructor chain. For each field the macro
        // emits a `let <ident>_field = ::umbra::forms::Field::<ctor>(name)
        //     [.min_length(N)] [.max_length(N)] [.optional()];`.
        let ctor = match (kind, attrs.email, attrs.password) {
            (FormFieldKind::String, true, _) => quote!(email),
            (FormFieldKind::String, _, true) => quote!(password),
            (FormFieldKind::String, _, _) => quote!(text),
            (FormFieldKind::Integer, _, _) => quote!(integer),
            (FormFieldKind::Float, _, _) => quote!(float),
            (FormFieldKind::Bool, _, _) => quote!(boolean),
        };
        let mut chain = quote! {
            ::umbra::forms::Field::#ctor(#field_name)
        };
        if let Some(n) = attrs.min_length {
            chain = quote! { #chain.min_length(#n) };
        }
        if let Some(n) = attrs.max_length {
            chain = quote! { #chain.max_length(#n) };
        }
        if is_optional {
            chain = quote! { #chain.optional() };
        }
        let field_var = format_ident!("_{}_field", field_ident);
        field_builders.push(quote! {
            let #field_var: ::umbra::forms::Field = #chain;
        });

        // Validation step.
        let raw_var = format_ident!("_{}_raw", field_ident);
        let parsed_var = format_ident!("_{}_parsed", field_ident);
        validate_body.push(quote! {
            let #raw_var: String = data
                .get(#field_name)
                .cloned()
                .unwrap_or_default();
            #field_var.validate(&#raw_var, &mut errs);
        });

        // Parsing step. Even on validation failure, we still try to
        // parse so the parse error is collected too. Macro emits one
        // of:
        //   - Option<String>: empty -> None, else Some(raw)
        //   - String: raw
        //   - Option<Int>: empty -> None, else Some(parse::<T>())
        //   - Int: parse::<T>() or 0 on failure (errs already pushed)
        //   - bool: matches!(raw.as_str(), "true" | "on" | "1")
        let parse_expr = match (kind, is_option) {
            (FormFieldKind::String, true) => quote! {
                if #raw_var.is_empty() { None } else { Some(#raw_var.clone()) }
            },
            (FormFieldKind::String, false) => quote! { #raw_var.clone() },
            (FormFieldKind::Integer, true) => quote! {
                if #raw_var.is_empty() {
                    None
                } else {
                    match #raw_var.parse() {
                        Ok(v) => Some(v),
                        Err(_) => {
                            errs.add(#field_name, format!("{} must be a whole number", #field_name));
                            None
                        }
                    }
                }
            },
            (FormFieldKind::Integer, false) => quote! {
                match #raw_var.parse() {
                    Ok(v) => v,
                    Err(_) => Default::default(),
                }
            },
            (FormFieldKind::Float, true) => quote! {
                if #raw_var.is_empty() {
                    None
                } else {
                    match #raw_var.parse() {
                        Ok(v) => Some(v),
                        Err(_) => {
                            errs.add(#field_name, format!("{} must be a number", #field_name));
                            None
                        }
                    }
                }
            },
            (FormFieldKind::Float, false) => quote! {
                match #raw_var.parse() {
                    Ok(v) => v,
                    Err(_) => Default::default(),
                }
            },
            (FormFieldKind::Bool, true) => quote! {
                if #raw_var.is_empty() {
                    None
                } else {
                    Some(matches!(#raw_var.as_str(), "true" | "on" | "1"))
                }
            },
            (FormFieldKind::Bool, false) => quote! {
                matches!(#raw_var.as_str(), "true" | "on" | "1")
            },
        };
        validate_body.push(quote! {
            let #parsed_var = { #parse_expr };
        });

        struct_inits.push(quote! { #field_ident: #parsed_var });
    }

    let field_builders_iter = field_builders.iter();
    let field_builders_iter2 = field_builders.iter();
    let field_var_idents: Vec<syn::Ident> = fields
        .iter()
        .map(|f| format_ident!("_{}_field", f.ident.as_ref().unwrap()))
        .collect();

    let output = quote! {
        impl ::umbra::forms::Form for #struct_name {
            fn validate(
                data: &::std::collections::HashMap<::std::string::String, ::std::string::String>,
            ) -> ::std::result::Result<Self, ::umbra::forms::ValidationErrors> {
                let mut errs = ::umbra::forms::ValidationErrors::new();
                #(#field_builders_iter)*
                #(#validate_body)*
                errs.into_result()?;
                Ok(Self { #(#struct_inits),* })
            }

            fn fields() -> ::std::vec::Vec<::umbra::forms::Field> {
                #(#field_builders_iter2)*
                vec![ #(#field_var_idents),* ]
            }
        }
    };
    Ok(output)
}

// =========================================================================
// `#[derive(Choices)]` — closed-set enums as model field types.
// =========================================================================

/// Derive the `ChoiceField` trait, `sqlx::Type` (+ Encode/Decode for
/// Postgres and SQLite), `Display`, and `FromStr` for a unit-variant
/// enum, so it can be used directly as a model field via
/// `#[umbra(choices)]`.
///
/// Accepted struct-level modifiers:
///
/// - `#[choices(rename_all = "lowercase")]` — case style for the
///   DB-stored variant names. Variants: `lowercase`, `UPPERCASE`,
///   `snake_case`, `SCREAMING_SNAKE_CASE`, `kebab-case`. Default:
///   `lowercase`.
///
/// Variant-level: each variant gets one DB value (derived from the
/// variant name + `rename_all`) and one human label (the variant name
/// verbatim). `#[choices(value = "...")]` and `#[choices(label = "...")]`
/// on a single variant override its DB value and label respectively.
#[proc_macro_derive(Choices, attributes(choices))]
pub fn derive_choices(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_choices(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy)]
enum RenameAll {
    Lowercase,
    Uppercase,
    SnakeCase,
    ScreamingSnakeCase,
    KebabCase,
    None,
}

fn apply_rename(s: &str, rule: RenameAll) -> String {
    match rule {
        RenameAll::None => s.to_string(),
        RenameAll::Lowercase => s.to_ascii_lowercase(),
        RenameAll::Uppercase => s.to_ascii_uppercase(),
        RenameAll::SnakeCase => to_snake_case(s),
        RenameAll::ScreamingSnakeCase => to_screaming_snake_case(s),
        RenameAll::KebabCase => to_snake_case(s).replace('_', "-"),
    }
}

fn expand_choices(input: DeriveInput) -> syn::Result<TokenStream2> {
    let enum_name = &input.ident;

    let variants = match &input.data {
        Data::Enum(e) => &e.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                enum_name,
                "umbra::Choices can only be derived on enums",
            ));
        }
    };

    // Parse the struct-level rename rule. Default: lowercase (the
    // sensible default for human-readable enum-as-string columns).
    let mut rename = RenameAll::Lowercase;
    for attr in &input.attrs {
        if !attr.path().is_ident("choices") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                rename = match lit.value().as_str() {
                    "lowercase" => RenameAll::Lowercase,
                    "UPPERCASE" => RenameAll::Uppercase,
                    "snake_case" => RenameAll::SnakeCase,
                    "SCREAMING_SNAKE_CASE" => RenameAll::ScreamingSnakeCase,
                    "kebab-case" => RenameAll::KebabCase,
                    "none" => RenameAll::None,
                    other => {
                        return Err(meta.error(format!(
                            "umbra::Choices: unknown `rename_all = \"{other}\"`. Known: \
                             lowercase, UPPERCASE, snake_case, SCREAMING_SNAKE_CASE, \
                             kebab-case, none"
                        )));
                    }
                };
                Ok(())
            } else {
                Err(meta
                    .error("umbra::Choices accepts only struct-level `rename_all = \"...\"` today"))
            }
        })?;
    }

    // Walk variants. Each must be unit (no fields). Per-variant
    // `#[choices(value = "...", label = "...")]` overrides apply.
    let mut variant_idents: Vec<&syn::Ident> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for v in variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                &v.ident,
                "umbra::Choices variants must be unit (no fields)",
            ));
        }
        let mut value: Option<String> = None;
        let mut label: Option<String> = None;
        for attr in &v.attrs {
            if !attr.path().is_ident("choices") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("value") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    value = Some(lit.value());
                    Ok(())
                } else if meta.path.is_ident("label") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    label = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta.error(
                        "umbra::Choices variant attr accepts `value = \"...\"` or `label = \"...\"`",
                    ))
                }
            })?;
        }
        let raw = v.ident.to_string();
        let v_value = value.unwrap_or_else(|| apply_rename(&raw, rename));
        let v_label = label.unwrap_or_else(|| raw.clone());
        variant_idents.push(&v.ident);
        values.push(v_value);
        labels.push(v_label);
    }

    // Check for duplicates in the DB value list — silent duplicates
    // would round-trip to whichever variant FromStr matches first.
    for i in 0..values.len() {
        for j in (i + 1)..values.len() {
            if values[i] == values[j] {
                return Err(syn::Error::new_spanned(
                    variant_idents[j],
                    format!(
                        "umbra::Choices: duplicate DB value `{}` (also used by `{}`). \
                         Use `#[choices(value = \"...\")]` to disambiguate.",
                        values[i], variant_idents[i],
                    ),
                ));
            }
        }
    }

    let values_lits: Vec<_> = values.iter().map(|s| quote!(#s)).collect();
    let labels_lits: Vec<_> = labels.iter().map(|s| quote!(#s)).collect();
    let from_arms: Vec<_> = values
        .iter()
        .zip(variant_idents.iter())
        .map(|(v, ident)| quote! { #v => ::core::option::Option::Some(#enum_name::#ident) })
        .collect();
    let as_str_arms: Vec<_> = variant_idents
        .iter()
        .zip(values.iter())
        .map(|(ident, v)| quote! { #enum_name::#ident => #v })
        .collect();

    let enum_name_str = enum_name.to_string();
    let invalid_msg = format!("invalid value for {}", enum_name_str);

    Ok(quote! {
        impl ::umbra::orm::ChoiceField for #enum_name {
            const VALUES: &'static [&'static str] = &[ #(#values_lits),* ];
            const LABELS: &'static [&'static str] = &[ #(#labels_lits),* ];

            fn as_str(&self) -> &'static str {
                match self {
                    #(#as_str_arms),*
                }
            }

            fn from_str_ok(s: &str) -> ::core::option::Option<Self> {
                match s {
                    #(#from_arms),* ,
                    _ => ::core::option::Option::None,
                }
            }
        }

        impl ::core::fmt::Display for #enum_name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                <Self as ::umbra::orm::ChoiceField>::as_str(self).fmt(f)
            }
        }

        impl ::core::str::FromStr for #enum_name {
            type Err = ::std::string::String;
            fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                <Self as ::umbra::orm::ChoiceField>::from_str_ok(s)
                    .ok_or_else(|| ::std::format!("{}: `{}`", #invalid_msg, s))
            }
        }

        // sqlx Type impls — round-trip as TEXT on both backends.
        impl ::umbra::_sqlx::Type<::umbra::_sqlx::Sqlite> for #enum_name {
            fn type_info() -> <::umbra::_sqlx::Sqlite as ::umbra::_sqlx::Database>::TypeInfo {
                <::std::string::String as ::umbra::_sqlx::Type<::umbra::_sqlx::Sqlite>>::type_info()
            }
        }
        impl ::umbra::_sqlx::Type<::umbra::_sqlx::Postgres> for #enum_name {
            fn type_info() -> <::umbra::_sqlx::Postgres as ::umbra::_sqlx::Database>::TypeInfo {
                <::std::string::String as ::umbra::_sqlx::Type<::umbra::_sqlx::Postgres>>::type_info()
            }
        }
        impl<'q> ::umbra::_sqlx::Encode<'q, ::umbra::_sqlx::Sqlite> for #enum_name {
            fn encode_by_ref(
                &self,
                buf: &mut <::umbra::_sqlx::Sqlite as ::umbra::_sqlx::Database>::ArgumentBuffer<'q>,
            ) -> ::core::result::Result<
                ::umbra::_sqlx::encode::IsNull,
                ::std::boxed::Box<dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync>,
            > {
                <&str as ::umbra::_sqlx::Encode<'q, ::umbra::_sqlx::Sqlite>>::encode_by_ref(
                    &<Self as ::umbra::orm::ChoiceField>::as_str(self),
                    buf,
                )
            }
        }
        impl<'q> ::umbra::_sqlx::Encode<'q, ::umbra::_sqlx::Postgres> for #enum_name {
            fn encode_by_ref(
                &self,
                buf: &mut <::umbra::_sqlx::Postgres as ::umbra::_sqlx::Database>::ArgumentBuffer<'q>,
            ) -> ::core::result::Result<
                ::umbra::_sqlx::encode::IsNull,
                ::std::boxed::Box<dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync>,
            > {
                <&str as ::umbra::_sqlx::Encode<'q, ::umbra::_sqlx::Postgres>>::encode_by_ref(
                    &<Self as ::umbra::orm::ChoiceField>::as_str(self),
                    buf,
                )
            }
        }
        impl<'r> ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Sqlite> for #enum_name {
            fn decode(
                value: <::umbra::_sqlx::Sqlite as ::umbra::_sqlx::Database>::ValueRef<'r>,
            ) -> ::core::result::Result<
                Self,
                ::std::boxed::Box<dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync>,
            > {
                let s = <&str as ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Sqlite>>::decode(value)?;
                <Self as ::umbra::orm::ChoiceField>::from_str_ok(s)
                    .ok_or_else(|| ::std::format!("{}: `{}`", #invalid_msg, s).into())
            }
        }
        impl<'r> ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Postgres> for #enum_name {
            fn decode(
                value: <::umbra::_sqlx::Postgres as ::umbra::_sqlx::Database>::ValueRef<'r>,
            ) -> ::core::result::Result<
                Self,
                ::std::boxed::Box<dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync>,
            > {
                let s = <&str as ::umbra::_sqlx::Decode<'r, ::umbra::_sqlx::Postgres>>::decode(value)?;
                <Self as ::umbra::orm::ChoiceField>::from_str_ok(s)
                    .ok_or_else(|| ::std::format!("{}: `{}`", #invalid_msg, s).into())
            }
        }
    })
}
