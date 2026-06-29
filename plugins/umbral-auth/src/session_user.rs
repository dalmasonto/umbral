//! `AuthUser`-aware session helpers — moved from umbral-sessions so
//! sessions can stay free of any user-model dependency.
//!
//! The split mirrors the dep arrow: `umbral-auth` depends on
//! `umbral-sessions` (it needs cookie + session-table primitives),
//! `umbral-sessions` does not depend on `umbral-auth` (it knows
//! nothing about users). All the AuthUser hydration happens here.
//!
//! ## What this module owns
//!
//! - [`current_user`] — read the cookie, hydrate the [`AuthUser`].
//! - [`login`] / [`login_with_request`] — the one-call shape
//!   for credential check + session creation + cookie set +
//!   `last_login` bump.
//! - [`logout`] — re-exported convenience; same as
//!   `umbral_sessions::logout` plus a forwarding doc-comment.
//! - [`SessionAuthentication`] — the `umbral-rest` `Authentication`
//!   impl that produces an `Identity` for the permission layer
//!   (was in `umbral-sessions`; needs AuthUser to populate
//!   `is_staff`).
//! - [`User`] / [`OptionalUser`] — axum extractors that pull
//!   `AuthUser` from the request.
//! - [`user_context_layer`] — middleware that injects the current
//!   user into `umbral::templates::CURRENT_USER` so HTML templates
//!   can write `{% if user.is_authenticated %}` uniformly.
//!
//! ## Custom user models
//!
//! Everything in here is hard-bound to [`AuthUser`]. Apps using a
//! custom [`UserModel`] roll their own helpers — the building
//! blocks are all `pub`:
//!
//! - `umbral_sessions::current_user_id_str(&headers)` → user PK as
//!   a string (already user-agnostic).
//! - Their own user lookup against that PK.
//! - Their own `Identity` builder.
//!
//! [`UserModel`]: crate::UserModel

use crate::{AuthUser, auth_user};
use async_trait::async_trait;
use axum_core::extract::FromRequestParts;
use http::StatusCode;
use http::request::Parts;
use umbral::auth::{Authentication, Identity};
use umbral::web::HeaderMap;
use umbral_sessions::SessionError;

// =========================================================================
// current_user — the AuthUser-flavored wrapper around
// umbral_sessions::current_session.
// =========================================================================

/// Read the request's session cookie, look up the session row, then
/// hydrate the [`AuthUser`] it points at. Returns `None` for any
/// of: no cookie, expired session, anonymous session
/// (`user_id IS NULL`), parse failure on a non-i64 user_id, missing
/// user row, or inactive user.
///
/// One DB read (session row) + one DB read (user row). The
/// `is_active` predicate is part of the user query, so a deactivated
/// account silently looks anonymous from this helper's perspective
/// without an explicit second filter at the call site.
pub async fn current_user(headers: &HeaderMap) -> Result<Option<AuthUser>, SessionError> {
    let Some(user_id_str) = umbral_sessions::current_user_id_str(headers).await? else {
        return Ok(None);
    };
    // Session.user_id is text (gap #59) — parse back to AuthUser's
    // i64 PK. A non-parseable value means the session was written
    // by a different UserModel impl; from AuthUser's perspective
    // that's anonymous.
    let Ok(user_id) = user_id_str.parse::<i64>() else {
        return Ok(None);
    };
    let user: Option<AuthUser> = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await?;
    Ok(user)
}

// =========================================================================
// login / login_with_request — credential check ran outside, we just
// mint the session + cookie + bump last_login.
// =========================================================================

/// Convenience: [`login_with_request`] with an empty request
/// HeaderMap. Use when the handler doesn't already have a
/// `HeaderMap` extractor and you're not worried about preserving
/// an anonymous session's `data` (flash messages, cart) across the
/// login.
pub async fn login(
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    login_with_request(&HeaderMap::new(), response_headers, user).await
}

/// Mint an authenticated session for `user`, rotate the cookie, and
/// bump `auth_user.last_login`. The session-fixation defense fires
/// inside `umbral_sessions::login_user_id`: any anonymous session
/// the request carried is destroyed before the new authenticated
/// row is written.
///
/// `last_login` is a best-effort update: a failure logs a warning
/// but doesn't invalidate the login (the session was created and
/// the cookie was set, so the user is in).
pub async fn login_with_request(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    let token = umbral_sessions::login_user_id(
        request_headers,
        response_headers,
        Some(user.id.to_string()),
    )
    .await?;

    let mut patch = serde_json::Map::new();
    patch.insert(
        "last_login".to_string(),
        serde_json::to_value(chrono::Utc::now()).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = AuthUser::objects()
        .filter(auth_user::ID.eq(user.id))
        .update_values(patch)
        .await
    {
        tracing::warn!(
            error = ?e,
            user_id = user.id,
            "umbral-auth::login: failed to update last_login (session still active)",
        );
    }
    Ok(token)
}

// =========================================================================
// SessionAuthentication — produce an `Identity` for the REST
// permission layer.
// =========================================================================

/// The session-cookie authenticator for `umbral-rest`. Reads the
/// cookie, hydrates the [`AuthUser`], turns it into an [`Identity`]
/// with `is_staff` set. Same shape `current_user` produces, packaged
/// for `RestPlugin::authenticate`.
///
/// Was in `umbral-sessions` before the de-coupling; now here so it
/// can name `AuthUser`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionAuthentication;

impl SessionAuthentication {
    /// Convenience constructor identical to `Default::default()`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Authentication for SessionAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        let user = current_user(headers).await.ok().flatten()?;
        // `user.id_string()` is the UserModel-level stringifier —
        // it stays correct when an app swaps AuthUser for a custom
        // user model with a non-i64 PK. `Identity::user_id` is
        // String regardless because Identity must be uniform across
        // user models.
        Some(
            Identity::user(crate::UserModel::id_string(&user))
                .with_staff(user.is_staff)
                .with_superuser(user.is_superuser)
                .with_extra("auth", serde_json::json!("session")),
        )
    }

    fn security_scheme(&self) -> Option<(String, serde_json::Value)> {
        // Standard "session cookie" scheme. The actual cookie name
        // (`umbral_session`) is documented in the description so
        // Swagger UI users know what they're authorising with.
        Some((
            "SessionAuth".to_string(),
            serde_json::json!({
                "type": "apiKey",
                "in": "cookie",
                "name": "umbral_session",
                "description": "umbral session cookie. Set by `POST /api/auth/login`; cleared by `/logout`."
            }),
        ))
    }
}

// =========================================================================
// User / OptionalUser axum extractors. Same shapes that used to live
// in umbral-sessions::extractors.
// =========================================================================

/// Required-user extractor. 401 on anonymous requests.
///
/// ```ignore
/// async fn dashboard(User(user): User) -> Html<String> {
///     Html(format!("Welcome, {}!", user.username))
/// }
/// ```
#[derive(Debug, Clone)]
pub struct User(pub AuthUser);

/// Optional-user extractor. Anonymous requests get `None`.
///
/// ```ignore
/// async fn home(OptionalUser(maybe): OptionalUser) -> Html<String> {
///     match maybe {
///         Some(u) => Html(format!("Hi, {}", u.username)),
///         None    => Html("<a href=\"/login\">Log in</a>".into()),
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct OptionalUser(pub Option<AuthUser>);

impl<S> FromRequestParts<S> for User
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match current_user(&parts.headers).await.ok().flatten() {
            Some(u) => Ok(User(u)),
            None => Err((StatusCode::UNAUTHORIZED, "authentication required")),
        }
    }
}

impl<S> FromRequestParts<S> for OptionalUser
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalUser(
            current_user(&parts.headers).await.ok().flatten(),
        ))
    }
}

// =========================================================================
// Template-injection middleware. Stash the current user under the
// `umbral::templates::CURRENT_USER` task-local so HTML renders can
// pick it up as `{{ user }}`.
// =========================================================================

/// Install a per-request lazy resolver on the
/// [`umbral::templates::CURRENT_USER_LAZY`] channel.
///
/// The resolver clones the request headers and runs **at most once**,
/// only when a template actually accesses `{{ user }}`. Requests that
/// never render a template (JSON/API responses) pay zero DB reads.
/// When the resolver does run it performs the session + user lookup
/// and memoizes the result for the rest of the request.
///
/// Opt in via [`crate::AuthPlugin::with_user_in_templates`] when the
/// app is HTML-heavy; leave off for REST-only services.
pub async fn user_context_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Install a LAZY resolver instead of resolving eagerly. The closure runs
    // (at most once) only if a template actually reads `user`; a JSON/API
    // response that never renders the template pays nothing.
    let headers = req.headers().clone();
    let lazy = umbral::templates::LazyUser::new(move || {
        let headers = headers.clone();
        async move {
            match current_user(&headers).await {
                Ok(Some(u)) => serialize_authenticated_with_relations(&u).await,
                _ => anonymous_user_value(),
            }
        }
    });
    umbral::templates::with_current_user_lazy(lazy, next.run(req)).await
}

/// Depth cap for the recursive relation expansion in
/// [`serialize_authenticated_with_relations`]. Closes gap2 #14:
/// templates can write `user.customer.loyalty_points` and get the
/// resolved value without the handler having to declare the prefetch.
///
/// Why 2: covers the common case `user.<one_to_one>.<scalar>` (1 hop
/// to load the child, scalars are free) AND `user.<one_to_one>.<fk>`
/// (2 hops if the template wants to walk back into another object).
/// Beyond 2 the query budget grows with the graph fan-out and the
/// "templates pay for every relation, every request" trade-off
/// stops being honest.
const USER_RELATION_DEPTH: usize = 2;

async fn serialize_authenticated_with_relations(user: &AuthUser) -> umbral::templates::Value {
    let mut json = match serde_json::to_value(user) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(true),
    );

    // gap2 #14: recursively expand reverse-O2O and forward-FK
    // relations on the serialized user, up to `USER_RELATION_DEPTH`
    // hops, with `(table, pk)` cycle detection so
    // `user.customer.user.customer...` terminates.
    //
    // PK lift Pass C: the visited set keys on `(table_name,
    // pk_json_key(value))` so non-i64 user PKs (UUID-keyed
    // AuthUser variants, codename-keyed permissions, etc.) ride
    // through the same cycle detector. The pre-fix shape was
    // `HashSet<(String, i64)>` which silently coerced everything
    // to i64 and broke for any UserModel impl with a non-i64 PK.
    //
    // The auth_user table must exist in the registry (AuthPlugin
    // registers it during App::build); if for some reason it's
    // missing we silently fall back to the un-expanded user JSON
    // rather than failing the request.
    let registered = umbral::migrate::registered_models();
    if let Some(meta) = registered.iter().find(|m| m.table == "auth_user") {
        let mut visited: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let seed_pk = serde_json::Value::Number(user.id.into());
        visited.insert(("auth_user".to_string(), pk_json_key(&seed_pk)));
        expand_relations(
            meta,
            &registered,
            &mut json,
            USER_RELATION_DEPTH,
            &mut visited,
        )
        .await;
    }

    umbral::templates::Value::from_serialize(serde_json::Value::Object(json))
}

/// Recursive depth-bounded expansion of a row's FK relations
/// (gap2 #14). Mutates `row` in place to:
///
/// - Replace every forward-FK integer id with the resolved target
///   row (when known to the model registry).
/// - Inject every reverse-OneToOne candidate (child models with a
///   UNIQUE FK pointing at `meta`) under the child table's name as
///   the key — so `Customer { user: ForeignKey<AuthUser> (unique) }`
///   surfaces as `user.customer` on the parent.
///
/// `visited` carries `(table, pk)` pairs already loaded in this
/// expansion. New rows are checked against it before recursion and
/// inserted before descending, so any cycle in the FK graph
/// terminates at the first revisit.
///
/// One query per loaded relation per request — the middleware's
/// query budget grows by `count(relations within depth)`, not by
/// the fan-out of subsequent template renders. Sparse relation
/// graphs (the common case) add 1-3 queries; pathological graphs
/// hit the depth cap and stop.
fn expand_relations<'a>(
    meta: &'a umbral::migrate::ModelMeta,
    registered: &'a [umbral::migrate::ModelMeta],
    row: &'a mut serde_json::Map<String, serde_json::Value>,
    depth: usize,
    visited: &'a mut std::collections::HashSet<(String, String)>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        if depth == 0 {
            return;
        }

        // -- Forward FKs: replace integer / string / UUID ids with
        // the full target row. Mirrors the dynamic
        // `select_related_dyn` semantics (gap2 #15) but driven by
        // the registry walk here so the user middleware doesn't
        // need to know which columns to expand ahead of time.
        //
        // PK lift Pass C: read the FK value as `serde_json::Value`
        // (not i64) so non-integer-PK targets (codename-keyed
        // permissions, UUID-keyed custom user models, etc.) flow
        // through. The cycle key uses `pk_json_key` so a numeric
        // 42 and a string "42" stay in different buckets.
        let forward_fks: Vec<(String, String, serde_json::Value)> = meta
            .fields
            .iter()
            .filter_map(|col| {
                let target_table = col.fk_target.as_deref()?;
                let fk_val = row.get(&col.name)?.clone();
                if fk_val.is_null() {
                    return None;
                }
                Some((col.name.clone(), target_table.to_string(), fk_val))
            })
            .collect();
        for (col_name, target_table, fk_val) in forward_fks {
            let visit_key = (target_table.clone(), pk_json_key(&fk_val));
            if visited.contains(&visit_key) {
                continue;
            }
            let Some(target_meta) = registered.iter().find(|m| m.table == target_table) else {
                continue;
            };
            let Some(target_pk) = target_meta.pk_column() else {
                continue;
            };
            let fetched = umbral::orm::DynQuerySet::for_meta(target_meta)
                .filter_eq_string(&target_pk.name, &json_value_to_pk_string(&fk_val))
                .first_as_json()
                .await;
            let Ok(Some(mut target_row)) = fetched else {
                continue;
            };
            visited.insert(visit_key);
            expand_relations(target_meta, registered, &mut target_row, depth - 1, visited).await;
            row.insert(col_name, serde_json::Value::Object(target_row));
        }

        // -- Reverse-O2O: child models with a UNIQUE FK to this
        // table get injected under the child's table name. Naming
        // convention uses the lower-case-model-name idiom
        // (`Customer { user: FK<User> (unique) }` → `user.customer`).
        let Some(parent_pk_col) = meta.pk_column() else {
            return;
        };
        // PK lift Pass C: parent PK as `serde_json::Value`, not i64.
        let parent_pk = match row.get(&parent_pk_col.name).cloned() {
            Some(v) if !v.is_null() => v,
            _ => return,
        };
        let candidates: Vec<(&umbral::migrate::ModelMeta, String)> = registered
            .iter()
            .filter_map(|child| {
                // Need exactly one UNIQUE FK pointing at this
                // table; ambiguous matches (e.g. `primary_user` +
                // `backup_user` both UNIQUE FKs to auth_user) are
                // skipped — there's no single right answer for
                // which one becomes `user.customer`.
                let mut matches = child
                    .fields
                    .iter()
                    .filter(|c| c.fk_target.as_deref() == Some(&meta.table) && c.unique);
                let first = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                Some((child, first.name.clone()))
            })
            .collect();
        for (child_meta, fk_col_name) in candidates {
            // Don't clobber a same-named scalar on the parent — if
            // a model genuinely names a column the same as its
            // child table (rare), the existing column wins.
            if row.contains_key(&child_meta.table) {
                continue;
            }
            let fetched = umbral::orm::DynQuerySet::for_meta(child_meta)
                .filter_eq_string(&fk_col_name, &json_value_to_pk_string(&parent_pk))
                .first_as_json()
                .await;
            let Ok(Some(mut child_row)) = fetched else {
                continue;
            };
            let Some(child_pk_col) = child_meta.pk_column() else {
                continue;
            };
            // PK lift Pass C: child PK as `serde_json::Value`.
            let Some(child_pk) = child_row.get(&child_pk_col.name).cloned() else {
                continue;
            };
            if child_pk.is_null() {
                continue;
            }
            let visit_key = (child_meta.table.clone(), pk_json_key(&child_pk));
            if visited.contains(&visit_key) {
                continue;
            }
            visited.insert(visit_key);
            expand_relations(child_meta, registered, &mut child_row, depth - 1, visited).await;
            row.insert(
                child_meta.table.clone(),
                serde_json::Value::Object(child_row),
            );
        }
    })
}

/// PK lift Pass C: stable cycle-key for the `visited` HashSet in
/// [`expand_relations`]. `serde_json::Value` isn't `Hash`, so we
/// flatten to a namespaced `String` per shape. Mirrors the
/// `pk_json_key` helper in `umbral-core::orm::dynamic` — kept local
/// here to avoid widening `umbral-core`'s pub surface for one tiny
/// helper. If a third call site needs the same namespacing, the
/// two should converge into one canonical pub fn in the facade.
fn pk_json_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) => format!("n:{n}"),
        serde_json::Value::String(s) => format!("s:{s}"),
        other => format!("o:{other}"),
    }
}

/// Render a PK JSON value as the string `DynQuerySet::filter_eq_string`
/// expects to bind against. `filter_eq_string` already coerces per the
/// column's `SqlType` so the right operand type lands on the wire —
/// we just need to hand it the value's `Display` form.
fn json_value_to_pk_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn anonymous_user_value() -> umbral::templates::Value {
    let mut json = serde_json::Map::new();
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(false),
    );
    umbral::templates::Value::from_serialize(serde_json::Value::Object(json))
}

// logout is now a proper `pub async fn` in `crate` (lib.rs) that wraps
// `umbral_sessions::logout` and maps the error to `AuthError::Session`.
// It is the single reusable logout for all surfaces. The old forwarding
// alias that returned `SessionError` has been removed.
