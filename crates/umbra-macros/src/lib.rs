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
}

/// Field-level `#[umbra(...)]` attribute parsed from a struct field.
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
}

fn parse_umbra_field_attr(attrs: &[syn::Attribute]) -> syn::Result<UmbraFieldAttr> {
    let mut parsed = UmbraFieldAttr {
        noform: false,
        noedit: false,
        primary_key: false,
        is_string_repr: false,
        max_length: 0,
        choices_ty: None,
        default: None,
        unique: false,
        on_delete: None,
        on_update: None,
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
                     `noform`, `noedit`, `string` (or `string = true`), \
                     `max_length = N`, `choices`, `default = \"...\"`, \
                     `unique`, `on_delete = \"...\"`, and \
                     `on_update = \"...\"`"
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

fn parse_umbra_struct_attr(attrs: &[syn::Attribute]) -> syn::Result<UmbraStructAttr> {
    let mut parsed = UmbraStructAttr {
        table: None,
        plugin: None,
        display: None,
        icon: None,
        database: None,
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
            } else {
                Err(meta.error(
                    "umbra::Model derive accepts struct-level `table = \"...\"`, `plugin = \"...\"`, \
                     `display = \"...\"`, `icon = \"...\"`, `database = \"...\"`; and field-level `noform` and `noedit`. \
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

    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        // PK detection: the field this iteration is on is the PK iff it
        // matches the one `id_field` resolved above — either explicitly
        // tagged `#[umbra(primary_key)]` or named `id` as the default.
        let is_primary_key = field_name == pk_field_name;

        let kind = classify_field_type(&field.ty);

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

        field_specs.push(quote! {
            ::umbra::orm::FieldSpec {
                name: #field_name_str,
                ty: #sql_ty_tokens,
                primary_key: #pk_lit,
                nullable: #nullable_lit,
                supported_backends: &[],
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
    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        let kind = classify_field_type(&field.ty);
        match &kind {
            FieldKind::ForeignKey(inner_ty) => {
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
        }

        impl #struct_name {
            pub fn objects() -> ::umbra::orm::Manager<Self> {
                ::umbra::orm::Manager::default()
            }
        }

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
    if is_wide_or_unsigned_int(ty) {
        return FieldKind::Unsupported(UnsupportedReason::WideOrUnsignedInt);
    }

    if let Some(inner) = option_inner(ty) {
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
        // MultiChoice is handled inline by the caller (emits a StrCol),
        // so this arm is unreachable in practice. We return an empty
        // token stream as a defensive default.
        FieldKind::MultiChoice(_) => return TokenStream2::new(),
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
