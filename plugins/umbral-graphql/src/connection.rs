//! Cursor pagination — Relay connections.
//!
//! # What is wrong with `limit` / `offset`
//!
//! Nothing, on a table nobody is writing to. On a live one it is quietly, invisibly broken:
//! `OFFSET` is **positional**. Fetch page 1 (rows 1–20), somebody inserts a row that sorts
//! near the top, fetch page 2 (`OFFSET 20`), and the row that *was* #20 is now #21 — so the
//! client sees it twice and never sees the row that took its place. A delete does the mirror
//! image: a row is skipped entirely. Nothing errors. The client just silently receives a
//! slightly wrong list, forever.
//!
//! It also does not scale: `OFFSET 100000` makes the database walk and discard 100 000 rows
//! before returning the one page you wanted.
//!
//! # What a cursor is
//!
//! Not a row number — the **sort key of the last row you saw**. The next page is then
//! "everything that sorts after this", which is a `WHERE`, not a count:
//!
//! ```sql
//! WHERE (created_at, id) > ('2026-07-01', 91) ORDER BY created_at, id LIMIT 20
//! ```
//!
//! Stable under concurrent writes (a row inserted elsewhere in the ordering cannot shift the
//! boundary), and it uses the index instead of scanning past it.
//!
//! # Three things that are easy to get wrong
//!
//! 1. **Tie-breaking.** Sort by `created_at` alone and two rows sharing a timestamp straddle
//!    the page boundary — one is served twice, one never. So the key is always
//!    `(sort_col, primary_key)`, and the primary key is unique, so the ordering is total.
//! 2. **The cursor must remember its ordering.** A cursor minted under `created_at ASC` is
//!    meaningless under `price DESC`; replaying it would silently return the wrong window. So
//!    the ordering is encoded IN the cursor and a mismatch is an error, not a guess.
//! 3. **`hasNextPage` needs `first + 1` rows.** Fetch exactly `first` and you cannot tell a
//!    full last page from a page with more behind it.

use async_graphql::dynamic::{Field, FieldFuture, FieldValue, InputValue, Object, TypeRef};
use base64::Engine;
use serde_json::Value as Json;
use umbral::migrate::ModelMeta;
use umbral::orm::{Cmp, DynQuerySet, typed_cmp_condition, typed_eq_condition};

use crate::schema::{Exposed, is_visible, type_name};

/// An opaque page boundary.
///
/// Opaque **on purpose**: a client that parses the cursor is a client that will break when we
/// change how paging works, and one that *forges* one is a client probing your data. It is
/// base64 rather than encrypted because it contains nothing secret — it is the sort key of a
/// row that client was just shown.
#[derive(serde::Serialize, serde::Deserialize)]
struct Cursor {
    /// The column the page was ordered by.
    c: String,
    /// Descending?
    d: bool,
    /// The last row's value for that column, as a string.
    v: String,
    /// The last row's primary key — the tie-breaker.
    p: String,
}

impl Cursor {
    fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(s: &str) -> async_graphql::Result<Self> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| async_graphql::Error::new("malformed cursor"))?;
        serde_json::from_slice(&raw).map_err(|_| async_graphql::Error::new("malformed cursor"))
    }
}

/// Render a JSON cell as the string form a cursor round-trips.
fn cell_to_string(v: Option<&Json>) -> String {
    match v {
        Some(Json::String(s)) => s.clone(),
        Some(Json::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `WHERE (sort, pk) > (v, p)` — the keyset predicate, spelled out.
///
/// SQL row-value comparison (`(a, b) > (x, y)`) would say this in one line, but SQLite and
/// Postgres disagree about how much of it they support, so it is expanded into the equivalent
/// disjunction, which every backend gets right:
///
/// ```sql
/// sort > v  OR  (sort = v  AND  pk > p)
/// ```
///
/// The second half is the tie-breaker, and it is the whole reason rows with identical sort
/// keys do not fall through the crack between two pages.
fn keyset(
    meta: &ModelMeta,
    sort: &str,
    pk: &str,
    cur: &Cursor,
) -> async_graphql::Result<sea_query::Condition> {
    let cmp = if cur.d { Cmp::Lt } else { Cmp::Gt };

    let after_sort = typed_cmp_condition(meta, sort, &cur.v, cmp)
        .ok_or_else(|| async_graphql::Error::new("cursor does not match this column's type"))?;
    let same_sort = typed_eq_condition(meta, sort, &cur.v)
        .ok_or_else(|| async_graphql::Error::new("cursor does not match this column's type"))?;
    let after_pk = typed_cmp_condition(meta, pk, &cur.p, cmp)
        .ok_or_else(|| async_graphql::Error::new("cursor carries an unusable primary key"))?;

    Ok(sea_query::Condition::any()
        .add(after_sort)
        .add(sea_query::Condition::all().add(same_sort).add(after_pk)))
}

/// `PageInfo`, shared by every connection in the schema.
pub(crate) fn page_info_type() -> Object {
    Object::new("PageInfo")
        .field(Field::new(
            "hasNextPage",
            TypeRef::named_nn(TypeRef::BOOLEAN),
            |ctx| {
                FieldFuture::new(async move {
                    let p = ctx.parent_value.try_downcast_ref::<PageInfo>()?;
                    Ok(Some(async_graphql::Value::Boolean(p.has_next)))
                })
            },
        ))
        .field(Field::new(
            "endCursor",
            TypeRef::named(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let p = ctx.parent_value.try_downcast_ref::<PageInfo>()?;
                    Ok(p.end.clone().map(async_graphql::Value::String))
                })
            },
        ))
}

#[derive(Clone)]
pub(crate) struct PageInfo {
    has_next: bool,
    end: Option<String>,
}

/// One row plus the cursor that points *at* it.
#[derive(Clone)]
pub(crate) struct Edge {
    node: Json,
    cursor: String,
}

/// The whole page.
#[derive(Clone)]
pub(crate) struct Conn {
    edges: Vec<Edge>,
    info: PageInfo,
}

/// `ProductEdge` and `ProductConnection` for one model.
pub(crate) fn types_for(e: &Exposed) -> (Object, Object) {
    let tname = type_name(&e.meta);

    let edge = Object::new(format!("{tname}Edge"))
        .field(Field::new("node", TypeRef::named_nn(&tname), |ctx| {
            FieldFuture::new(async move {
                let edge = ctx.parent_value.try_downcast_ref::<Edge>()?;
                Ok(Some(FieldValue::owned_any(edge.node.clone())))
            })
        }))
        .field(Field::new(
            "cursor",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let edge = ctx.parent_value.try_downcast_ref::<Edge>()?;
                    Ok(Some(async_graphql::Value::String(edge.cursor.clone())))
                })
            },
        ));

    let conn = Object::new(format!("{tname}Connection"))
        .field(Field::new(
            "edges",
            TypeRef::named_nn_list_nn(format!("{tname}Edge")),
            |ctx| {
                FieldFuture::new(async move {
                    let c = ctx.parent_value.try_downcast_ref::<Conn>()?;
                    Ok(Some(FieldValue::list(
                        c.edges.clone().into_iter().map(FieldValue::owned_any),
                    )))
                })
            },
        ))
        .field(Field::new(
            "pageInfo",
            TypeRef::named_nn("PageInfo"),
            |ctx| {
                FieldFuture::new(async move {
                    let c = ctx.parent_value.try_downcast_ref::<Conn>()?;
                    Ok(Some(FieldValue::owned_any(c.info.clone())))
                })
            },
        ));

    (edge, conn)
}

/// `Query.productsConnection(first:, after:, orderBy:, desc:)`.
pub(crate) fn query_field(e: &Exposed) -> Field {
    let tname = type_name(&e.meta);
    let field_name = format!("{}Connection", crate::schema::plural(&e.meta.name));
    let ex = e.clone();

    Field::new(
        field_name,
        TypeRef::named_nn(format!("{tname}Connection")),
        move |ctx| {
            let e = ex.clone();
            FieldFuture::new(async move {
                crate::guard(&ctx, e.access.as_ref(), &e.meta)?;

                let pk = crate::loader::pk_name(&e.meta);
                let first = ctx
                    .args
                    .get("first")
                    .and_then(|v| v.u64().ok())
                    .unwrap_or(crate::DEFAULT_LIMIT)
                    .min(crate::MAX_LIMIT);

                let after = ctx
                    .args
                    .get("after")
                    .and_then(|v| v.string().ok().map(|s| s.to_string()));
                let cursor = after.as_deref().map(Cursor::decode).transpose()?;

                // The ordering comes from the cursor when there is one. A client that pages
                // with `after` AND a different `orderBy` is asking for a window that does not
                // exist; honouring one and ignoring the other would silently serve the wrong
                // rows, so it is an error.
                let (sort, desc) = match &cursor {
                    Some(c) => {
                        let asked = ctx.args.get("orderBy").and_then(|v| v.string().ok());
                        if let Some(a) = asked {
                            if a != c.c {
                                return Err(async_graphql::Error::new(
                                    "this cursor was issued for a different `orderBy` — a cursor \
                                     is a position in ONE ordering, and means nothing in another",
                                ));
                            }
                        }
                        (c.c.clone(), c.d)
                    }
                    None => (
                        ctx.args
                            .get("orderBy")
                            .and_then(|v| v.string().ok())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| pk.clone()),
                        ctx.args
                            .get("desc")
                            .and_then(|v| v.boolean().ok())
                            .unwrap_or(false),
                    ),
                };

                // You cannot order by a column you cannot see. Beyond the obvious, a cursor
                // built on a hidden column would leak its values back out in the cursor
                // itself — base64 is not encryption.
                if !is_visible(&e, &sort) || !e.meta.fields.iter().any(|c| c.name == sort) {
                    return Err(async_graphql::Error::new(format!(
                        "cannot order by `{sort}`"
                    )));
                }

                // A cursor page is a read like any other, so it carries the same field policy.
                // Wiring the unlock into the list resolver and forgetting this one is exactly
                // the kind of hole a per-resolver policy invites — there are four separate
                // paths through the ORM in this plugin, and a test caught this one missing.
                let unlocks = crate::privacy::from_ctx(&ctx);
                let unlocked = unlocks.for_table(&e.meta.table);
                let refs: Vec<&str> = unlocked.iter().map(String::as_str).collect();
                let mut qs = DynQuerySet::for_meta(&e.meta).allow_private(&refs);
                if let Some(c) = &cursor {
                    qs = qs.filter_condition(keyset(&e.meta, &sort, &pk, c)?);
                }
                // ORDER BY (sort, pk) — the pk is the tie-breaker that makes the ordering
                // total. Without it, rows sharing a sort value straddle the page boundary.
                let rows = qs
                    .order_by_col(&sort, desc)
                    .order_by_col(&pk, desc)
                    // first + 1: the extra row is how we know whether there IS a next page.
                    // It is fetched and discarded, never shown.
                    .limit(first + 1)
                    .fetch_as_json()
                    .await
                    .map_err(|err| async_graphql::Error::new(err.to_string()))?;

                let has_next = rows.len() as u64 > first;
                let edges: Vec<Edge> = rows
                    .into_iter()
                    .take(first as usize)
                    .map(|r| {
                        let row = Json::Object(r);
                        let cur = Cursor {
                            c: sort.clone(),
                            d: desc,
                            v: cell_to_string(row.get(&sort)),
                            p: cell_to_string(row.get(&pk)),
                        };
                        Edge {
                            cursor: cur.encode(),
                            node: row,
                        }
                    })
                    .collect();

                let info = PageInfo {
                    has_next,
                    end: edges.last().map(|e| e.cursor.clone()),
                };
                Ok(Some(FieldValue::owned_any(Conn { edges, info })))
            })
        },
    )
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)))
    .argument(InputValue::new("after", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("orderBy", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("desc", TypeRef::named(TypeRef::BOOLEAN)))
}
