//! Subscriptions — live data over WebSocket or SSE.
//!
//! # Where the events come from
//!
//! Nowhere new. The ORM already fires `post_save:<table>` and `post_delete:<table>` on every
//! write, whichever path did the writing — typed `Manager`, `DynQuerySet`, a REST endpoint, an
//! admin form, a GraphQL mutation. Subscriptions just listen. A row changed by *anything* is
//! a row your subscribers hear about, which is the property that makes this trustworthy: no
//! write path can forget to publish, because publishing is not the write path's job.
//!
//! # The trap: the signal payload is NOT safe to forward
//!
//! `post_save` carries `{ "instance": <the model, serde-serialized>, "created": bool }`. That
//! JSON comes from `#[derive(Serialize)]`, which knows nothing about `#[umbral(private)]`,
//! `#[umbral(secret)]`, `Masked<T>`, or a plugin's `hide` list. Forwarding it straight to
//! subscribers would ship every one of those fields down the socket — the entire field policy,
//! defeated, because the data went out over a WebSocket instead of a response body.
//!
//! So the event carries **only the primary key**, and the row is re-read through
//! `DynQuerySet` before it is handed to a subscriber. Same redaction as every other read,
//! same relations, same loaders. The socket is a transport, not an exemption.
//!
//! # Deletes are a different shape
//!
//! A deleted row cannot be re-read, so `<model>Deleted` yields an **ID**, not an object. The
//! alternative — echoing the last-known row from the signal payload — would be exactly the
//! leak described above, and a "row" that no longer exists is a lie besides.

use std::sync::{Arc, OnceLock};

use async_graphql::dynamic::{
    FieldValue, InputValue, Subscription, SubscriptionField, SubscriptionFieldFuture, TypeRef,
};
use futures_util::StreamExt;
use tokio::sync::broadcast;

use crate::schema::{Exposed, type_name};

/// The primary key out of a `post_save` / `post_delete` payload.
fn pk_of(payload: &serde_json::Value, meta_pk: &str) -> Option<String> {
    let inst = payload.get("instance")?;
    Some(crate::schema::id_string(inst.get(meta_pk)?))
}

/// One row changed.
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub table: String,
    pub pk: String,
    pub deleted: bool,
}

/// The process-wide fan-out.
///
/// Capacity is bounded: a subscriber that stops reading must not be able to grow this without
/// limit. `broadcast` drops the oldest messages for a lagging receiver, which is the right
/// trade — a slow client missing an update is survivable; the server running out of memory
/// because of one is not.
fn bus() -> &'static broadcast::Sender<Change> {
    static BUS: OnceLock<broadcast::Sender<Change>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(1024).0)
}

/// Subscribe to the ORM's write signals for one table and republish the primary keys.
///
/// Called once per subscribable model at startup.
pub(crate) fn wire_signals(table: &str) {
    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table);
    let Some(meta) = meta else {
        tracing::error!(table = %table, "umbral-graphql: subscribable table is not a model");
        return;
    };
    let pk = crate::loader::pk_name(&meta);

    // FOUR signals, not two — the ORM's write paths do not all speak the same one, and a
    // subscription that listens to only half of them silently misses updates:
    //
    //   insert_json            -> post_save        (per-row, `{ instance, created }`)
    //   typed Manager/QuerySet -> post_save / post_delete
    //   update_json            -> bulk_post_save   (`{ ids, created }`)
    //   delete                 -> bulk_post_delete (`{ ids }`)
    //
    // The dynamic update/delete paths are predicate-based and can touch N rows, so they
    // report a LIST of ids rather than N instances — re-reading every row just to announce it
    // would be a query per row on a path whose whole point is to avoid that. Both vocabularies
    // are legitimate; the subscriber has to speak both.
    //
    // This was found by a test asserting that an ordinary `update_json` reaches a subscriber.
    // It did not. Subscriptions would have shipped working for creates and silently dead for
    // every edit made through REST or the admin.
    for (signal, deleted) in [("post_save", false), ("post_delete", true)] {
        let name = format!("{signal}:{table}");
        let table = table.to_string();
        let pk = pk.clone();
        umbral::signals::subscribe_async(&name, move |payload: &serde_json::Value| {
            let table = table.clone();
            // Extract BEFORE the async block: the handler is handed a borrowed payload and the
            // future it returns must not hold that borrow.
            //
            // ONLY the primary key crosses this line. The signal's `instance` is a serde dump
            // of the model and carries private/secret columns; the subscriber gets a freshly
            // read, redacted row instead. See the module docs.
            let id = pk_of(payload, &pk);
            async move {
                if let Some(id) = id {
                    // A send error means nobody is listening. Not a problem.
                    let _ = bus().send(Change {
                        table,
                        pk: id,
                        deleted,
                    });
                }
            }
        });
    }

    for (signal, deleted) in [("bulk_post_save", false), ("bulk_post_delete", true)] {
        let name = format!("{signal}:{table}");
        let table = table.to_string();
        umbral::signals::subscribe_async(&name, move |payload: &serde_json::Value| {
            let table = table.clone();
            let ids: Vec<String> = payload
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(crate::schema::id_string).collect())
                .unwrap_or_default();
            async move {
                for id in ids {
                    let _ = bus().send(Change {
                        table: table.clone(),
                        pk: id,
                        deleted,
                    });
                }
            }
        });
    }
}

/// The `Subscription` root, or `None` when no model opted in.
pub(crate) fn build(exposed: &[Exposed]) -> Option<Subscription> {
    let subs: Vec<&Exposed> = exposed.iter().filter(|e| e.subscribable).collect();
    if subs.is_empty() {
        return None;
    }

    let mut root = Subscription::new("Subscription");

    for e in subs {
        let tname = type_name(&e.meta);
        let snake = crate::schema::snake_name(&tname);

        // ---- <model>Changed: create + update, as a full node -------------
        let ec = (*e).clone();
        root =
            root.field(
                SubscriptionField::new(
                    format!("{snake}Changed"),
                    TypeRef::named_nn(&tname),
                    move |ctx| {
                        let e = ec.clone();
                        SubscriptionFieldFuture::new(async move {
                            // A subscription is a long-lived READ, so it is gated exactly like
                            // one. Checked ONCE, at subscribe time — see the note below.
                            crate::guard(&ctx, e.access.as_ref(), &e.meta)?;
                            let only: Option<String> = ctx
                                .args
                                .get("id")
                                .and_then(|v| v.string().ok().map(|s| s.to_string()));

                            let rx = bus().subscribe();
                            let table = e.meta.table.clone();
                            let meta = Arc::new(e.meta.clone());

                            Ok(tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(
                                move |ev| {
                                    let table = table.clone();
                                    let only = only.clone();
                                    let meta = meta.clone();
                                    async move {
                                        // A lagging subscriber yields Err(Lagged(n)) — it
                                        // missed messages. Skip, do not kill the stream: a
                                        // dropped update is recoverable, a dead socket is not.
                                        let ev = ev.ok()?;
                                        if ev.table != table || ev.deleted {
                                            return None;
                                        }
                                        if only.as_ref().is_some_and(|id| id != &ev.pk) {
                                            return None;
                                        }
                                        // Re-read through the ORM: redacted, current, and a
                                        // real node in the graph (its relations resolve).
                                        let row = crate::loader::load_one_json(&meta, &ev.pk)
                                            .await
                                            .ok()??;
                                        Some(Ok(FieldValue::owned_any(serde_json::Value::Object(
                                            row,
                                        ))))
                                    }
                                },
                            ))
                        })
                    },
                )
                .argument(InputValue::new("id", TypeRef::named(TypeRef::ID))),
            );

        // ---- <model>Deleted: the id, because the row is gone -------------
        let ed = (*e).clone();
        root = root.field(SubscriptionField::new(
            format!("{snake}Deleted"),
            TypeRef::named_nn(TypeRef::ID),
            move |ctx| {
                let e = ed.clone();
                SubscriptionFieldFuture::new(async move {
                    crate::guard(&ctx, e.access.as_ref(), &e.meta)?;
                    let rx = bus().subscribe();
                    let table = e.meta.table.clone();
                    Ok(
                        tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |ev| {
                            let table = table.clone();
                            async move {
                                let ev = ev.ok()?;
                                (ev.deleted && ev.table == table)
                                    .then(|| Ok(async_graphql::Value::String(ev.pk)))
                            }
                        }),
                    )
                })
            },
        ));
    }

    Some(root)
}

/// Test-only: publish a change without going through a write.
#[doc(hidden)]
pub fn publish_for_tests(table: &str, pk: &str, deleted: bool) {
    let _ = bus().send(Change {
        table: table.to_string(),
        pk: pk.to_string(),
        deleted,
    });
}
