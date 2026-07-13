//! Build a GraphQL schema from the model registry.
//!
//! # Why this is derived from `ModelMeta` and not from the OpenAPI spec
//!
//! The obvious shortcut is to convert the OpenAPI document (which already exists) into a
//! GraphQL schema. It is cheap, and it produces something nobody wants: a flat, RPC-shaped
//! schema of `getPost` / `listPosts`. That is GraphQL in name only. Nobody adopts GraphQL
//! to make the same call with different syntax; they adopt it to ask for a **graph**:
//!
//! ```graphql
//! { post(id: 1) { title author { username } comments { body } } }
//! ```
//!
//! umbral already HAS that graph. `Column::fk_target` names the table a foreign key points
//! at; invert those edges and you have every reverse relation; `ModelMeta::m2m_relations`
//! carries the rest. The registry is a graph, and it is the same registry `typegen`,
//! `gen-client` and OpenAPI already read. So we derive from it directly.
//!
//! # How a row travels
//!
//! Every resolver passes a `serde_json::Value` row down as the parent value. A scalar
//! field reads its key out of that object. A relation field reads the FK column out of it
//! and asks the loader for the target row — batched, so a list of 100 posts costs ONE
//! query for their authors, not 100.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputValue, Object, Schema, SchemaError, TypeRef,
};
use async_graphql::{Name, Value};
use serde_json::Value as Json;
use umbral::migrate::{Column, ModelMeta};
use umbral::orm::SqlType;

use crate::loader::Loaders;

/// A caller-supplied read gate: `true` lets the query through.
pub type AccessFn = Arc<dyn Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync>;

/// A model exposed to GraphQL, plus how it may be read.
#[derive(Clone)]
pub struct Exposed {
    pub meta: ModelMeta,
    /// `None` = readable by anyone. `Some(f)` = readable only when `f(identity)` is true.
    pub access: Option<AccessFn>,
    /// Columns of this model that are omitted from the schema entirely. See
    /// [`crate::GraphqlPlugin::hide`].
    pub hidden: Vec<String>,
    /// `None` = READ-ONLY. `Some(f)` = writable when `f(identity)` is true.
    ///
    /// A separate opt-in from `access` on purpose: a read you got wrong leaks data, a write
    /// you got wrong destroys it. Exposing a model does not make it writable.
    pub writable: Option<AccessFn>,
}

/// Whether a column appears in the schema at all.
///
/// Two independent reasons it might not:
///
/// 1. The operator hid it (`GraphqlPlugin::hide`) — configuration.
/// 2. It is hard-denied in core (`password_hash`) — NOT configuration. No combination of
///    builder calls can re-expose it, and that is the point.
///
/// The field is omitted from the SCHEMA, not merely resolved to null: a field that exists
/// and always returns null still tells an attacker the column is there, and still shows up
/// in introspection and in GraphiQL's autocomplete. Absent is a stronger statement than
/// empty.
pub(crate) fn is_visible(e: &Exposed, col_name: &str) -> bool {
    if let Some(col) = e.meta.fields.iter().find(|c| c.name == col_name) {
        // Declared on the MODEL: `#[umbral(secret)]` (and every `Masked<T>`) can never be
        // served, and `#[umbral(private)]` is not served here because GraphQL has no way to
        // establish that a given caller has unlocked it — `DynQuerySet::allow_private` is a
        // per-read decision, and the client picks the query shape. Absent, not null.
        if umbral::orm::is_secret_column(col) || col.private {
            return false;
        }
    }
    if umbral::orm::is_hard_denied_field(col_name) {
        return false;
    }
    !e.hidden.iter().any(|h| h == col_name)
}

/// The GraphQL type name for a model. `Model::NAME` is already PascalCase.
pub(crate) fn type_name(meta: &ModelMeta) -> String {
    meta.name.clone()
}

/// Plural query field for a model: `post` -> `posts`, `category` -> `categories`.
///
/// English, not a naive `+ "s"`. The first version produced `categorys`, which is the kind
/// of thing a user sees in GraphiQL and immediately distrusts the rest of the schema over.
fn list_field_name(meta: &ModelMeta) -> String {
    plural(&meta.name)
}

/// See [`list_field_name`].
pub(crate) fn plural(model_name: &str) -> String {
    let s = snake(model_name);
    let last = s.chars().last().unwrap_or('x');
    let penult = s.chars().rev().nth(1).unwrap_or('x');

    if last == 'y' && !matches!(penult, 'a' | 'e' | 'i' | 'o' | 'u') {
        // category -> categories, but day -> days
        format!("{}ies", &s[..s.len() - 1])
    } else if s.ends_with('s')
        || s.ends_with('x')
        || s.ends_with('z')
        || s.ends_with("ch")
        || s.ends_with("sh")
    {
        // address -> addresses, box -> boxes
        format!("{s}es")
    } else {
        format!("{s}s")
    }
}

pub(crate) fn snake_name(pascal: &str) -> String {
    snake(pascal)
}

fn snake(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Map a column's SQL type to a GraphQL scalar.
///
/// Dates, UUIDs and Decimal cross the wire as `String` — the same choice `typegen` makes
/// for TypeScript, so a client sees one consistent shape whichever API it talks to. A
/// `Json` column is `String` too (serialised), because GraphQL has no `Any`.
pub(crate) fn scalar_for(col: &Column) -> &'static str {
    match umbral::migrate::fk_effective_type(col) {
        SqlType::SmallInt | SqlType::Integer => TypeRef::INT,
        // GraphQL's Int is i32. A BigInt does not fit, and silently truncating someone's
        // primary key is not an option — so it goes over as a String.
        SqlType::BigInt | SqlType::ForeignKey => TypeRef::STRING,
        SqlType::Real | SqlType::Double => TypeRef::FLOAT,
        SqlType::Boolean => TypeRef::BOOLEAN,
        _ => TypeRef::STRING,
    }
}

/// Turn one JSON cell into a GraphQL value of the column's scalar type.
fn json_to_gql(col: &Column, v: &Json) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    match scalar_for(col) {
        TypeRef::INT => v
            .as_i64()
            .map(|n| Value::Number(n.into()))
            .unwrap_or(Value::Null),
        TypeRef::FLOAT => v
            .as_f64()
            .and_then(async_graphql::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        TypeRef::BOOLEAN => v.as_bool().map(Value::Boolean).unwrap_or(Value::Null),
        _ => match v {
            Json::String(s) => Value::String(s.clone()),
            other => Value::String(other.to_string().trim_matches('"').to_string()),
        },
    }
}

/// The parent row a resolver receives.
type Row = Json;

/// Build the whole schema from the exposed models.
pub fn build(exposed: &[Exposed]) -> Result<Schema, SchemaError> {
    let by_table: BTreeMap<String, Exposed> = exposed
        .iter()
        .map(|e| (e.meta.table.clone(), e.clone()))
        .collect();

    let mut query = Object::new("Query");
    let mut objects: Vec<Object> = Vec::new();

    for e in exposed {
        let meta = e.meta.clone();
        let tname = type_name(&meta);
        let mut obj = Object::new(&tname);

        // ---- scalar fields -------------------------------------------------
        for col in &meta.fields {
            if !is_visible(e, &col.name) {
                continue;
            }
            // An FK to an EXPOSED model is owned by the relation loop below: it emits the
            // object under the bare name (`author`) and the raw id under `author_id`.
            // Emitting a scalar here too would collide on the name — and async-graphql
            // panics on a duplicate field, which is how this was caught.
            //
            // An FK to an UNEXPOSED model stays a plain scalar: you still see the id, you
            // just cannot traverse into a model the operator withheld.
            if col
                .fk_target
                .as_ref()
                .is_some_and(|t| by_table.contains_key(t))
            {
                continue;
            }
            let key = col.name.clone();
            let c = col.clone();
            let ty = if col.nullable {
                TypeRef::named(scalar_for(col))
            } else {
                TypeRef::named_nn(scalar_for(col))
            };
            obj = obj.field(Field::new(col.name.clone(), ty, move |ctx| {
                let key = key.clone();
                let c = c.clone();
                FieldFuture::new(async move {
                    let row = ctx.parent_value.try_downcast_ref::<Row>()?;
                    Ok(Some(json_to_gql(&c, row.get(&key).unwrap_or(&Json::Null))))
                })
            }));
        }

        // ---- forward FK: `post.author` -> the AuthUser object ---------------
        //
        // The column is `author` (or `author_id`); the edge is `fk_target`. We only expose
        // the relation when the TARGET is exposed too — otherwise GraphQL would become a
        // side door into a model the operator deliberately withheld.
        for col in &meta.fields {
            let Some(target_table) = col.fk_target.as_ref() else {
                continue;
            };
            let Some(target) = by_table.get(target_table) else {
                continue;
            };
            // Hiding an FK column drops the raw id AND the object edge. Keeping the edge
            // would make `hide` decorative: `{ product { category { id } } }` hands back the
            // very id you just hid, one hop further out.
            if !is_visible(e, &col.name) {
                continue;
            }
            let target_meta = target.meta.clone();
            let target_type = type_name(&target_meta);
            let fk_col = col.name.clone();
            // `author_id` -> object `author`; `author` -> object `author`. Either way the
            // OBJECT gets the bare name (that is the point of GraphQL) and the raw id is
            // always available under `<name>_id` for a client that just wants the key.
            let field_name = fk_col.strip_suffix("_id").unwrap_or(&fk_col).to_string();
            let id_field = format!("{field_name}_id");
            obj = obj.field(Field::new(id_field, TypeRef::named(TypeRef::STRING), {
                let fk_col = fk_col.clone();
                move |ctx| {
                    let fk_col = fk_col.clone();
                    FieldFuture::new(async move {
                        let row = ctx.parent_value.try_downcast_ref::<Row>()?;
                        Ok(Some(scalar_id(row.get(&fk_col))))
                    })
                }
            }));

            let nullable = col.nullable;
            obj = obj.field(Field::new(
                field_name,
                if nullable {
                    TypeRef::named(&target_type)
                } else {
                    TypeRef::named_nn(&target_type)
                },
                move |ctx| {
                    let fk_col = fk_col.clone();
                    let target_meta = target_meta.clone();
                    FieldFuture::new(async move {
                        let row = ctx.parent_value.try_downcast_ref::<Row>()?;
                        let Some(raw) = row.get(&fk_col) else {
                            return Ok(None);
                        };
                        if raw.is_null() {
                            return Ok(None);
                        }
                        let id = id_string(raw);
                        // Batched. A list of 100 posts costs ONE query for their authors.
                        let loaders = ctx.data::<Loaders>()?;
                        let got = loaders.load_by_pk(&target_meta, id).await?;
                        Ok(got.map(FieldValue::owned_any))
                    })
                },
            ));
        }

        // ---- reverse FK: `author.posts` -> [Post] ---------------------------
        //
        // Derived by INVERTING the forward edges: any exposed model with a column whose
        // fk_target is this table is a child of it. Nothing extra needs to be declared —
        // the registry already knows.
        for other in exposed {
            if other.meta.table == meta.table {
                continue;
            }
            for col in &other.meta.fields {
                if col.fk_target.as_deref() != Some(meta.table.as_str()) {
                    continue;
                }
                // The child hid the FK that forms this edge, so the edge does not exist.
                // Otherwise `author.posts` would quietly reinstate a relation the operator
                // severed from the other side.
                if !is_visible(other, &col.name) {
                    continue;
                }
                let child_meta = other.meta.clone();
                let child_type = type_name(&child_meta);
                let fk_col = col.name.clone();
                let field = list_field_name(&child_meta);
                // The parent's pk column is known HERE, at schema-build time — no runtime
                // registry lookup, and no way for the resolver to guess wrong.
                let parent_pk = meta
                    .pk_column()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "id".to_string());
                obj = obj.field(Field::new(
                    field,
                    TypeRef::named_nn_list_nn(&child_type),
                    move |ctx| {
                        let child_meta = child_meta.clone();
                        let fk_col = fk_col.clone();
                        let parent_pk = parent_pk.clone();
                        FieldFuture::new(async move {
                            let row = ctx.parent_value.try_downcast_ref::<Row>()?;
                            let pk = pk_of(row, &parent_pk);
                            let loaders = ctx.data::<Loaders>()?;
                            let rows = loaders.load_children(&child_meta, &fk_col, pk).await?;
                            Ok(Some(FieldValue::list(
                                rows.into_iter().map(FieldValue::owned_any),
                            )))
                        })
                    },
                ));
            }
        }

        objects.push(obj);

        // ---- Query.post(id:) ------------------------------------------------
        let single_meta = meta.clone();
        let single_access = e.access.clone();
        query = query.field(
            Field::new(snake(&tname), TypeRef::named(&tname), move |ctx| {
                let m = single_meta.clone();
                let access = single_access.clone();
                FieldFuture::new(async move {
                    crate::guard(&ctx, access.as_ref(), &m)?;
                    let id = ctx.args.try_get("id")?.string()?.to_string();
                    let loaders = ctx.data::<Loaders>()?;
                    let got = loaders.load_by_pk(&m, id).await?;
                    Ok(got.map(FieldValue::owned_any))
                })
            })
            .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID))),
        );

        // ---- Query.posts(limit:, offset:) -------------------------------------
        let list_meta = meta.clone();
        let list_access = e.access.clone();
        query = query.field(
            Field::new(
                list_field_name(&meta),
                TypeRef::named_nn_list_nn(&tname),
                move |ctx| {
                    let m = list_meta.clone();
                    let access = list_access.clone();
                    FieldFuture::new(async move {
                        crate::guard(&ctx, access.as_ref(), &m)?;
                        let limit = ctx
                            .args
                            .get("limit")
                            .and_then(|v| v.u64().ok())
                            .unwrap_or(crate::DEFAULT_LIMIT)
                            .min(crate::MAX_LIMIT);
                        let offset = ctx
                            .args
                            .get("offset")
                            .and_then(|v| v.u64().ok())
                            .unwrap_or(0);
                        let rows = crate::loader::fetch_list(&m, limit, offset).await?;
                        Ok(Some(FieldValue::list(
                            rows.into_iter().map(FieldValue::owned_any),
                        )))
                    })
                },
            )
            .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
            .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT))),
        );
    }

    // The Mutation root exists only if something is actually writable. A schema that
    // advertises an empty `Mutation` type is an invitation to go looking for a way in.
    let mutation = crate::mutation::build(exposed);
    let mut schema = Schema::build("Query", mutation.as_ref().map(|_| "Mutation"), None);
    for o in objects {
        schema = schema.register(o);
    }
    if let Some((m, inputs)) = mutation {
        for i in inputs {
            schema = schema.register(i);
        }
        schema = schema.register(m);
    }
    schema.register(query).finish()
}

fn pk_of(row: &Json, pk: &str) -> String {
    id_string(row.get(pk).unwrap_or(&Json::Null))
}

/// A primary key as a string, whatever its JSON shape.
///
/// The ORM lifted the i64 PK assumption (gaps3 #59); GraphQL must not put it back. An id
/// is a `String` on the wire for exactly that reason: an i64 does not fit GraphQL's Int,
/// and a Uuid or String key was never an integer to begin with.
pub(crate) fn id_string(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn scalar_id(v: Option<&Json>) -> Value {
    match v {
        Some(j) if !j.is_null() => Value::String(id_string(j)),
        _ => Value::Null,
    }
}

/// Silence an unused import in some cfgs.
#[allow(dead_code)]
fn _name(n: &str) -> Name {
    Name::new(n)
}
