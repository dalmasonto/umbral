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
/// exactly one key: `table = "..."` to override the snake_case-of-
/// struct-name default for the SQL table name. More keys land as
/// plugin authors need them (per-field `max_length`, `db_index`,
/// `default`, `choices`, `on_delete` — all deferred until a real
/// plugin needs each).
struct UmbraStructAttr {
    table: Option<String>,
}

fn parse_umbra_struct_attr(attrs: &[syn::Attribute]) -> syn::Result<UmbraStructAttr> {
    let mut parsed = UmbraStructAttr { table: None };
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
            } else {
                Err(meta.error(
                    "umbra::Model derive only accepts `table = \"...\"` at M3.1; \
                     other attributes (max_length, db_index, default, choices, on_delete) \
                     land as plugin authors need them",
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

    // M3 fixes the primary key to a field named `id` and accepts the
    // three PK types `i32`, `i64`, and `uuid::Uuid`. The error message
    // hints at the future flexibility so a user porting from Django
    // doesn't think they have to rename every PK to `id` forever.
    let id_field = fields
        .iter()
        .find(|f| f.ident.as_ref().is_some_and(|i| i == "id"));
    let id_field = match id_field {
        Some(f) => f,
        None => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "umbra M3 requires an `id` primary-key field of type \
                 i32, i64, or uuid::Uuid (M3+ supports custom PK names)",
            ));
        }
    };
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

    // The default table name is snake_case of the struct name; a
    // struct-level `#[umbra(table = "...")]` overrides it. Plugin
    // authors hit the override path when they want a prefix the
    // snake_case round-trip can't produce (e.g. struct `User` with
    // table `auth_user`).
    let struct_attr = parse_umbra_struct_attr(&input.attrs)?;
    let table_name = struct_attr
        .table
        .unwrap_or_else(|| to_snake_case(&struct_name.to_string()));
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
        let is_primary_key = field_name == "id";

        let kind = classify_field_type(&field.ty);

        let (sql_ty_tokens, nullable_lit) = match kind.sql_type_tokens() {
            Some((ty, nullable)) => (ty, nullable),
            None => {
                // `Unsupported` lands here. Emit a typed error at the
                // field's span and keep going so the user sees every
                // problematic field at once.
                let err =
                    syn::Error::new_spanned(&field.ty, kind.error_message()).to_compile_error();
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

        field_specs.push(quote! {
            ::umbra::orm::FieldSpec {
                name: #field_name_str,
                ty: #sql_ty_tokens,
                primary_key: #pk_lit,
                nullable: #nullable_lit,
                supported_backends: &[],
                fk_target: #fk_target_tokens,
            }
        });

        column_consts.push(column_const_for(struct_name, &field_name_str, field, &kind));
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
                fk_id_arms.push(quote! {
                    #field_name_str => ::core::option::Option::Some(self.#field_name.id()),
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
                    #field_name_str => self.#field_name.as_ref().map(|fk| fk.id()),
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
            fn primary_key(&self) -> #pk_ty_tokens {
                // `.clone()` works for every PK type the trait accepts
                // (the bound is `Clone`, not `Copy`). For `i32`, `i64`,
                // `Uuid`, etc. the optimiser folds the clone back into
                // a copy; for `String` the clone is the work the call
                // site would have done anyway.
                self.id.clone()
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

/// If `ty` is `Vec<T>` and `T` is one of the [`ArrayElementKind`]
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
    if type_is_ident(inner, "i8") || type_is_ident(inner, "i16") || type_is_ident(inner, "u8") {
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
