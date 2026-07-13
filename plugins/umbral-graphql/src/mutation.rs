//! Writes.
//!
//! # Why this is a separate opt-in from `expose`
//!
//! A read you got wrong leaks data. A write you got wrong *destroys* it. So exposing a model
//! for reading does not make it writable — you say so again, per model, with `mutable`.
//! Nothing is writable until you name it twice.
//!
//! # Everything goes through the ORM
//!
//! `insert_json` / `update_json` / `delete` — the same dynamic write path REST and the admin
//! use. That is not laziness; it is the only way these mutations inherit, for free:
//!
//! - **mass-assignment defence.** `#[umbral(privileged)]` columns (`is_staff`, ownership FKs)
//!   are stripped from any body that did not explicitly authorize them. A client cannot make
//!   itself an admin by adding a field to the mutation.
//! - **validators, cleaners, defaults, `auto_now`,** and the signals other plugins listen on.
//! - **the field policy.** The row echoed back after a write is a serialized response like
//!   any other, so `private` / `secret` columns do not come back in it.
//!
//! Hand-rolling SQL here would have quietly opted out of all four.

use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputObject, InputValue, Object, TypeRef,
};
use serde_json::Value as Json;
use umbral::migrate::{Column, ModelMeta};
use umbral::orm::DynQuerySet;

use crate::schema::{Exposed, is_visible, scalar_for, type_name};

/// Can this column be SET by a client?
///
/// Narrower than "is it visible". A column can be perfectly readable and still be none of a
/// client's business to write:
///
/// - the **primary key** is the database's to assign;
/// - `auto_now` / `auto_now_add` / `auto_user` columns are the framework's to assign — putting
///   them in the input would invite a client to backdate its own `created_at`;
/// - `#[umbral(privileged)]` columns are stripped by the ORM anyway, so offering them in the
///   input would be a *lie*: the client sets `is_staff: true`, gets no error, and nothing
///   happens. An input field that silently does nothing is worse than no input field.
pub(crate) fn is_writable(e: &Exposed, col: &Column) -> bool {
    if !is_visible(e, &col.name) {
        return false;
    }
    !(col.primary_key
        || col.privileged
        || col.noedit
        || col.auto_now
        || col.auto_now_add
        || col.auto_user
        || col.auto_user_add)
}

/// Must the client supply this on create?
///
/// NOT NULL, no default, and nothing else is going to fill it in.
fn required_on_create(col: &Column) -> bool {
    !col.nullable && col.default.is_empty()
}

/// `ProductInput` (create) and `ProductPatch` (update).
///
/// Two types, not one, because they mean different things. On create, a NOT NULL column with
/// no default is **required** — and saying so in the schema is the difference between the
/// client's tooling catching it and the database catching it. On update, every field is
/// optional: that is what a patch IS, and marking them required would force a client to
/// re-send the whole row to change one field, which is how you get lost-update bugs.
pub(crate) fn input_types(e: &Exposed) -> (InputObject, InputObject) {
    let tname = type_name(&e.meta);
    let mut create = InputObject::new(format!("{tname}Input"));
    let mut patch = InputObject::new(format!("{tname}Patch"));

    for col in &e.meta.fields {
        if !is_writable(e, col) {
            continue;
        }
        let scalar = scalar_for(col);
        create = create.field(if required_on_create(col) {
            InputValue::new(col.name.clone(), TypeRef::named_nn(scalar))
        } else {
            InputValue::new(col.name.clone(), TypeRef::named(scalar))
        });
        patch = patch.field(InputValue::new(col.name.clone(), TypeRef::named(scalar)));
    }
    (create, patch)
}

/// The GraphQL input object, as a JSON body the ORM's write path understands.
///
/// Only keys the client actually sent survive. An absent key and an explicit `null` are
/// different things on a patch — absent means "leave it alone", null means "set it to null" —
/// and collapsing them would make it impossible to ever null a column out.
fn body_from_args(
    obj: &async_graphql::dynamic::ObjectAccessor<'_>,
    e: &Exposed,
) -> async_graphql::Result<serde_json::Map<String, Json>> {
    let mut body = serde_json::Map::new();
    for col in &e.meta.fields {
        if !is_writable(e, col) {
            continue;
        }
        let Some(v) = obj.get(&col.name) else {
            continue; // absent: not part of this write at all
        };
        body.insert(col.name.clone(), v.as_value().clone().into_json()?);
    }
    Ok(body)
}

/// The `Mutation` root: `createProduct`, `updateProduct`, `deleteProduct` per writable model.
///
/// Returns `None` when nothing is writable — a schema advertising an empty `Mutation` type is
/// an invitation to try.
pub(crate) fn build(exposed: &[Exposed]) -> Option<(Object, Vec<InputObject>)> {
    let writable: Vec<&Exposed> = exposed.iter().filter(|e| e.writable.is_some()).collect();
    if writable.is_empty() {
        return None;
    }

    let mut mutation = Object::new("Mutation");
    let mut inputs = Vec::new();

    for e in writable {
        let tname = type_name(&e.meta);
        let snake = crate::schema::snake_name(&tname);
        let (create_in, patch_in) = input_types(e);
        inputs.push(create_in);
        inputs.push(patch_in);

        // ---- create ------------------------------------------------------
        let ec = (*e).clone();
        mutation = mutation.field(
            Field::new(
                format!("create{tname}"),
                TypeRef::named_nn(&tname),
                move |ctx| {
                    let e = ec.clone();
                    FieldFuture::new(async move {
                        crate::guard_write(&ctx, &e)?;
                        let data = ctx.args.try_get("data")?;
                        let body = body_from_args(&data.object()?, &e)?;
                        // insert_json: strips privileged columns, runs cleaners/validators,
                        // fires signals, and hands back a row that is already redacted.
                        let row = DynQuerySet::for_meta(&e.meta)
                            .insert_json(&body)
                            .await
                            .map_err(|err| async_graphql::Error::new(err.to_string()))?;
                        Ok(Some(FieldValue::owned_any(Json::Object(row))))
                    })
                },
            )
            .argument(InputValue::new(
                "data",
                TypeRef::named_nn(format!("{tname}Input")),
            )),
        );

        // ---- update ------------------------------------------------------
        let eu = (*e).clone();
        mutation = mutation.field(
            Field::new(
                format!("update{tname}"),
                TypeRef::named(&tname),
                move |ctx| {
                    let e = eu.clone();
                    FieldFuture::new(async move {
                        crate::guard_write(&ctx, &e)?;
                        let id = ctx.args.try_get("id")?.string()?.to_string();
                        let data = ctx.args.try_get("data")?;
                        let body = body_from_args(&data.object()?, &e)?;
                        let pk = crate::loader::pk_name(&e.meta);

                        let n = DynQuerySet::for_meta(&e.meta)
                            .filter_eq_string(&pk, &id)
                            .update_json(&body)
                            .await
                            .map_err(|err| async_graphql::Error::new(err.to_string()))?;
                        if n == 0 {
                            return Ok(None);
                        }
                        // Read the row BACK rather than echoing the request body: defaults,
                        // `auto_now`, cleaners and database triggers all mean the stored row
                        // is not always the row you sent, and the client should see what is
                        // actually there.
                        let loaders = ctx.data::<crate::loader::Loaders>()?;
                        let got = loaders.load_by_pk(&e.meta, id).await?;
                        Ok(got.map(FieldValue::owned_any))
                    })
                },
            )
            .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID)))
            .argument(InputValue::new(
                "data",
                TypeRef::named_nn(format!("{tname}Patch")),
            )),
        );

        // ---- delete ------------------------------------------------------
        let ed = (*e).clone();
        mutation = mutation.field(
            Field::new(
                format!("delete{tname}"),
                TypeRef::named_nn(TypeRef::BOOLEAN),
                move |ctx| {
                    let e = ed.clone();
                    FieldFuture::new(async move {
                        crate::guard_write(&ctx, &e)?;
                        let id = ctx.args.try_get("id")?.string()?.to_string();
                        let pk = crate::loader::pk_name(&e.meta);
                        // filter_eq_string FAILS CLOSED on a type mismatch (gaps3 #56): a
                        // non-numeric id against an integer pk yields `1=0`, never a
                        // predicate-less DELETE that empties the table.
                        let n = DynQuerySet::for_meta(&e.meta)
                            .filter_eq_string(&pk, &id)
                            .delete()
                            .await
                            .map_err(|err| async_graphql::Error::new(err.to_string()))?;
                        Ok(Some(async_graphql::Value::Boolean(n > 0)))
                    })
                },
            )
            .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID))),
        );

        let _ = snake;
    }

    Some((mutation, inputs))
}

/// The model this mutation writes, for the error message.
pub(crate) fn meta_name(meta: &ModelMeta) -> &str {
    &meta.name
}
