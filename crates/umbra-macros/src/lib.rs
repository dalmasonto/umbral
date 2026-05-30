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
    Data, DeriveInput, Field, Fields, GenericArgument, PathArguments, Type, TypePath,
    parse_macro_input,
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
/// - The struct must have a field named `id` of type `i64`. That field
///   becomes the primary key.
/// - The supported field types are `i64`, `String`,
///   `chrono::DateTime<chrono::Utc>`, and `Option<chrono::DateTime<chrono::Utc>>`.
/// - No `#[umbra(...)]` attributes yet. Foreign derive attributes
///   (`#[serde(...)]`, `#[sqlx(...)]`, …) are ignored.
#[proc_macro_derive(Model)]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_model(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
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

    // M3 hardcodes the primary key as `id: i64`. The error message hints
    // at the future flexibility so a user porting from Django doesn't
    // think they have to rename every PK to `id` forever.
    let id_field = fields
        .iter()
        .find(|f| f.ident.as_ref().is_some_and(|i| i == "id"));
    let id_field = match id_field {
        Some(f) => f,
        None => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "umbra M3 requires an `id: i64` primary-key field \
                 (M3+ supports custom PK names and types)",
            ));
        }
    };
    if !type_is_ident(&id_field.ty, "i64") {
        return Err(syn::Error::new_spanned(
            &id_field.ty,
            "umbra M3 requires an `id: i64` primary-key field \
             (M3+ supports custom PK names and types)",
        ));
    }

    let table_name = to_snake_case(&struct_name.to_string());
    let module_name = format_ident!("{}", table_name);

    // Field-spec entries for the trait's FIELDS const, and column-const
    // declarations for the sibling module. Built side by side so the
    // declaration order matches between the two.
    let mut field_specs: Vec<TokenStream2> = Vec::new();
    let mut column_consts: Vec<TokenStream2> = Vec::new();

    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        let is_primary_key = field_name == "id";

        let kind = classify_field_type(&field.ty);

        let (sql_ty_tokens, nullable_lit) = match &kind {
            FieldKind::Int => (quote!(::umbra::orm::SqlType::BigInt), quote!(false)),
            FieldKind::Str => (quote!(::umbra::orm::SqlType::Text), quote!(false)),
            FieldKind::DateTime => (quote!(::umbra::orm::SqlType::Timestamptz), quote!(false)),
            FieldKind::NullableDateTime => {
                (quote!(::umbra::orm::SqlType::Timestamptz), quote!(true))
            }
            FieldKind::UnsupportedNullable => {
                // Emit a typed error at the field's span and keep going
                // so the user sees every problematic field at once.
                let err = syn::Error::new_spanned(
                    &field.ty,
                    "umbra M3 doesn't yet ship a nullable column type for this field; \
                     use Option<DateTime<Utc>> only",
                )
                .to_compile_error();
                field_specs.push(err.clone());
                column_consts.push(err);
                continue;
            }
            FieldKind::Unsupported => {
                let err = syn::Error::new_spanned(
                    &field.ty,
                    "umbra M3 doesn't yet support this field type; \
                     see docs/specs/04-orm-model-and-fields.md for the M3 type catalogue",
                )
                .to_compile_error();
                field_specs.push(err.clone());
                column_consts.push(err);
                continue;
            }
        };

        let pk_lit = if is_primary_key {
            quote!(true)
        } else {
            quote!(false)
        };

        field_specs.push(quote! {
            ::umbra::orm::FieldSpec {
                name: #field_name_str,
                ty: #sql_ty_tokens,
                primary_key: #pk_lit,
                nullable: #nullable_lit,
                supported_backends: &[],
            }
        });

        column_consts.push(column_const_for(struct_name, &field_name_str, field, &kind));
    }

    // Sibling module name collision with the struct ident is harmless
    // because Rust's type and value namespaces are separate, but the
    // module-inception clippy lint trips when the snake_case happens to
    // equal the struct ident (e.g. a struct already named `comment`).
    // Silence it the same way `post.rs` does for parity with the M2
    // hand-written shape.
    let output = quote! {
        impl ::umbra::orm::Model for #struct_name {
            type PrimaryKey = i64;
            const TABLE: &'static str = #table_name;
            const FIELDS: &'static [::umbra::orm::FieldSpec] = &[
                #(#field_specs),*
            ];
            fn primary_key(&self) -> i64 {
                self.id
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
            use ::umbra::orm::column::{
                DateTimeCol, IntCol, NullableDateTimeCol, StrCol,
            };

            #(#column_consts)*
        }
    };

    Ok(output)
}

/// The classification a field's Rust type lands in for M3.
///
/// This is the single switchboard for the type → column-type mapping. As
/// the M3+ derive's catalogue grows (BoolCol, FloatCol, UuidCol, …) the
/// new variants land here and the `match` arms in `expand_model` plus
/// `column_const_for` extend with them. The mapping table:
///
/// | Rust field type                          | FieldKind             | SqlType     | Column type            |
/// |------------------------------------------|-----------------------|-------------|------------------------|
/// | `i64`                                    | `Int`                 | `BigInt`    | `IntCol<Self>`         |
/// | `String`                                 | `Str`                 | `Text`      | `StrCol<Self>`         |
/// | `chrono::DateTime<chrono::Utc>`          | `DateTime`            | `Timestamptz` | `DateTimeCol<Self>`  |
/// | `Option<chrono::DateTime<chrono::Utc>>`  | `NullableDateTime`    | `Timestamptz` | `NullableDateTimeCol<Self>` |
/// | `Option<i64>`, `Option<String>`, …       | `UnsupportedNullable` | (error)     | (error)                |
/// | anything else                            | `Unsupported`         | (error)     | (error)                |
enum FieldKind {
    Int,
    Str,
    DateTime,
    NullableDateTime,
    /// `Option<T>` of a type we recognise as a base column but for which
    /// there's no nullable column type yet. Emits a targeted error.
    UnsupportedNullable,
    /// Everything else (f64, bool, Uuid, custom types, …). Emits a
    /// catch-all "not in the M3 catalogue" error.
    Unsupported,
}

/// Inspect a `syn::Type` and pick its `FieldKind`.
///
/// Type detection here is name-based: a path's *last* segment ident is
/// what matters. That means the derive sees through `chrono::DateTime`,
/// `DateTime`, and `::chrono::DateTime` identically — the user can write
/// any of them and the derive does the right thing.
fn classify_field_type(ty: &Type) -> FieldKind {
    if type_is_ident(ty, "i64") {
        return FieldKind::Int;
    }
    if type_is_ident(ty, "String") {
        return FieldKind::Str;
    }
    if is_datetime_utc(ty) {
        return FieldKind::DateTime;
    }
    if let Some(inner) = option_inner(ty) {
        if is_datetime_utc(inner) {
            return FieldKind::NullableDateTime;
        }
        if type_is_ident(inner, "i64") || type_is_ident(inner, "String") {
            return FieldKind::UnsupportedNullable;
        }
        return FieldKind::Unsupported;
    }
    FieldKind::Unsupported
}

/// True when `ty` is a path whose last segment ident equals `name` and
/// carries no generic arguments. Used for plain types like `i64`,
/// `String`.
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

/// Build the `pub const FOO: FooCol<Self> = FooCol::new("foo");`
/// declaration for one field.
///
/// The const name is `SCREAMING_SNAKE_CASE(field_name)`. The column type
/// is chosen by `FieldKind`. Unsupported variants are caught upstream
/// and never reach here; the match arms for them are unreachable in
/// practice but kept exhaustive so adding a new `FieldKind` is a compile
/// error here too.
fn column_const_for(
    struct_name: &syn::Ident,
    field_name: &str,
    field: &Field,
    kind: &FieldKind,
) -> TokenStream2 {
    let const_ident = format_ident!("{}", to_screaming_snake_case(field_name));
    let span = field.ty.span();
    match kind {
        FieldKind::Int => quote_spanned! { span =>
            pub const #const_ident: IntCol<super::#struct_name> = IntCol::new(#field_name);
        },
        FieldKind::Str => quote_spanned! { span =>
            pub const #const_ident: StrCol<super::#struct_name> = StrCol::new(#field_name);
        },
        FieldKind::DateTime => quote_spanned! { span =>
            pub const #const_ident: DateTimeCol<super::#struct_name> = DateTimeCol::new(#field_name);
        },
        FieldKind::NullableDateTime => quote_spanned! { span =>
            pub const #const_ident: NullableDateTimeCol<super::#struct_name> =
                NullableDateTimeCol::new(#field_name);
        },
        FieldKind::UnsupportedNullable | FieldKind::Unsupported => TokenStream2::new(),
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
