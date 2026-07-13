//! Dashboard API + the two built-in widgets.

use axum::extract::State;
use minijinja::context;
use umbral::orm::DynQuerySet;
use umbral::web::{HeaderMap, IntoResponse, Json, Path, Response, StatusCode};

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::find_model;
use crate::engine::render;
use crate::error::AdminError;
use crate::models;
use crate::util::is_htmx;
use crate::widgets::{
    BarPayload, CatalogEntry, ChartPoint, FeedItem, FeedPayload, Series, Span, Widget,
    WidgetDataFn, WidgetKind, WidgetPayload,
};

// =========================================================================
// Built-in widgets
// =========================================================================

/// `Models by plugin` bar chart — counts every model the migration
/// registry knows about, grouped by plugin. Cheap to compute and
/// always present.
pub fn builtin_total_models_widget() -> Widget {
    Widget {
        key: "umbral_total_models",
        title: "Models by Plugin".to_string(),
        kind: WidgetKind::Bar,
        default_span: Span { cols: 4, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let points = models_by_plugin_points();
            WidgetPayload::Bar(BarPayload {
                series: vec![Series {
                    name: "models".to_string(),
                    points,
                }],
                x_type: "plugin".to_string(),
            })
        }),
    }
}

fn models_by_plugin_points() -> Vec<ChartPoint> {
    let mut assigned = std::collections::HashSet::new();
    let mut points: Vec<ChartPoint> = Vec::new();

    for plugin in umbral::migrate::registered_plugins() {
        let models = umbral::migrate::models_for_plugin(&plugin);
        for model in &models {
            assigned.insert(model.table.clone());
        }
        if !models.is_empty() {
            points.push(ChartPoint {
                x: plugin,
                y: models.len() as f64,
            });
        }
    }

    let app_count = umbral::migrate::registered_models()
        .into_iter()
        .filter(|model| !assigned.contains(&model.table))
        .count();
    if app_count > 0 {
        points.push(ChartPoint {
            x: "app".to_string(),
            y: app_count as f64,
        });
    }

    points.sort_by(|a, b| match (a.x.as_str(), b.x.as_str()) {
        ("app", "app") => std::cmp::Ordering::Equal,
        ("app", _) => std::cmp::Ordering::Greater,
        (_, "app") => std::cmp::Ordering::Less,
        _ => a.x.cmp(&b.x),
    });
    points
}

/// `Recent signups` feed — last 5 `auth_user` rows ordered by
/// `date_joined`. Gracefully degrades to an empty list if the table
/// is absent (e.g. an admin-only install where `AuthPlugin` isn't
/// registered), so this widget never breaks the dashboard.
///
/// Goes through [`DynQuerySet`] keyed off the `auth_user` `ModelMeta`
/// — that way the widget works against any custom user model
/// `AuthPlugin::<U>` registers, not just the built-in `AuthUser`. If
/// the registry doesn't know about an `auth_user` table (the
/// degraded-install case), the widget returns an empty feed.
pub fn builtin_recent_users_widget() -> Widget {
    Widget {
        key: "umbral_recent_users",
        title: "Recent Signups".to_string(),
        kind: WidgetKind::Feed,
        default_span: Span { cols: 4, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let items = match find_model("auth_user") {
                Some((_, meta)) => {
                    let rows = DynQuerySet::for_meta(&meta)
                        .select_cols(&["username".to_string(), "date_joined".to_string()])
                        .order_by_col("date_joined", true)
                        .limit(5)
                        .fetch_as_strings()
                        .await;
                    match rows {
                        Ok(rows) => rows
                            .into_iter()
                            .map(|r| FeedItem {
                                actor: r.get("username").cloned().unwrap_or_default(),
                                verb: "signed".to_string(),
                                object: "up".to_string(),
                                object_link: None,
                                at: r.get("date_joined").cloned().unwrap_or_default(),
                            })
                            .collect(),
                        Err(e) => {
                            tracing::debug!(error = %e, "umbral_recent_users: auth_user fetch failed; empty feed");
                            vec![]
                        }
                    }
                }
                None => vec![],
            };
            // Auto-resolve "View all →" to the admin's auth_user
            // changelist — works for any UserModel registered with
            // AuthPlugin since the table name is read from the
            // ModelMeta we already looked up.
            let mut payload = FeedPayload::new(items);
            if let Some((_, meta)) = find_model("auth_user") {
                payload.view_all_url = Some(format!(
                    "{}/{}/",
                    crate::branding::current().base_path,
                    meta.table,
                ));
            }
            WidgetPayload::Feed(payload)
        }),
    }
}

// =========================================================================
// API handlers
// =========================================================================

/// `GET /admin/api/dashboard/catalog` — list widgets the user may add to
/// the dashboard.
pub(crate) async fn dashboard_catalog(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/catalog").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    // gaps3 #6: omit widgets the user can't load. Otherwise a user without a
    // widget's codename sees it in the "add widget" catalog, adds it, then
    // gets a 403 on the data fetch (the data endpoint IS gated). Same
    // per-widget `permission` check `dashboard_widget_data` enforces.
    let mut entries: Vec<CatalogEntry> = Vec::with_capacity(state.widget_catalog.len());
    for w in state.widget_catalog.iter() {
        if let Some(code) = w.permission {
            if !crate::permcheck::has_codename(&user, code).await {
                continue;
            }
        }
        entries.push(CatalogEntry {
            key: w.key,
            title: w.title.clone(),
            kind: w.kind.as_str().to_string(),
            default_span: w.default_span.clone(),
        });
    }
    Json(entries).into_response()
}

/// `GET /admin/api/dashboard/layout` — user's saved layout or default.
/// The body is returned as raw JSON because we round-trip it through
/// the prefs row as a string.
pub(crate) async fn dashboard_layout_get(headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/layout").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let prefs = match models::fetch_or_default(user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_get failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "layout error").into_response();
        }
    };
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(prefs.dashboard_layout))
        .unwrap_or_else(|_| (StatusCode::OK, "[]").into_response())
}

/// `PUT /admin/api/dashboard/layout` — save the user's layout. Body
/// must be a JSON array of widget instances; non-JSON 400s. Validity
/// of the array shape is the client's problem until we lock down a
/// schema for it.
pub(crate) async fn dashboard_layout_put(headers: HeaderMap, body: String) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/layout").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid JSON layout").into_response();
    }
    let mut prefs = match models::fetch_or_default(user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_put fetch failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "layout error").into_response();
        }
    };
    prefs.dashboard_layout = body;
    match models::upsert(prefs).await {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_put save failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "layout save error").into_response()
        }
    }
}

/// `GET /admin/api/dashboard/widgets/{key}/data` — compute and return
/// one widget's payload. Returns either JSON (API consumers) or an
/// HTML fragment (HTMX swap).
pub(crate) async fn dashboard_widget_data(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/widgets/.../data").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(widget) = state.widget_catalog.iter().find(|w| w.key == key.as_str()) else {
        return AdminError::NotFound(format!("no widget `{key}`")).into_response();
    };

    // Security gate: if this widget belongs to a permission-gated custom view,
    // the requesting user must hold the view's codename — the same check the
    // page handler enforces. Without this, a staff user blocked from the page
    // could bypass `.with_permission(...)` by calling the data endpoint directly.
    if let Some(code) = state.widget_gates.get(key.as_str()) {
        if let Err(r) = crate::permcheck::require_codename(&user, code).await {
            return r;
        }
    }

    // Per-widget permission gate (independent of any view-level gate above).
    // A widget with `permission: Some(codename)` may only be fetched by a
    // user holding that codename, regardless of which page the widget lives on.
    // Graceful no-op: `require_codename` allows all when PermissionsPlugin is absent.
    if let Some(code) = widget.permission {
        if let Err(r) = crate::permcheck::require_codename(&user, code).await {
            return r;
        }
    }

    // Per-request parameters parsed from the query string.
    // Closures registered via `WidgetDataFn::with_params` read
    // these to vary the response (`?period=7d`, etc.); closures
    // registered via plain `::new` see them dropped.
    let mut params = crate::widgets::WidgetParams::from_query(query.as_deref().unwrap_or(""));

    // gaps2 #11 round 2 — period resolution priority:
    //
    //   1. URL `?period=` (explicit user click on a chip THIS visit).
    //   2. User's saved override at
    //      `preferences.dashboard.widget_periods.<key>`.
    //   3. Widget's registration-time `default_period`.
    //
    // When the URL carries an explicit `?period=`, we ALSO persist
    // it as the user's new preference — chip clicks become sticky
    // across reloads / tabs / devices without any extra UI surface
    // or HTMX wiring.
    if let Some(explicit) = params.period.clone() {
        if let Err(e) = models::set_widget_period(user.id, &key, &explicit).await {
            tracing::warn!(
                user = user.id,
                widget = %key,
                period = %explicit,
                error = %e,
                "gaps2 #11: failed to persist widget period (continuing render)"
            );
        }
    } else {
        if let Ok(Some(saved)) = models::get_widget_period(user.id, &key).await {
            params.period = Some(saved);
        } else if let Some(default) = widget.default_period {
            params.period = Some(default.to_string());
        }
    }

    // Declarative filters resolve on the same ladder the period does:
    //
    //   1. an explicit value in THIS request's query string,
    //   2. the value this user last picked (sticky, persisted),
    //   3. the filter's registration-time default.
    //
    // An explicit pick is persisted so it survives a reload — the same deal
    // period chips already got, extended to every control. The resolved value
    // is written back into `params` so the data closure sees the user's choice
    // even when the URL is bare, which is what makes a bookmarked /admin land
    // on the dashboard you left rather than a reset one.
    let mut filters = widget.effective_filters();
    let saved = models::get_widget_filters(user.id, &key)
        .await
        .unwrap_or_default();

    for filter in &mut filters {
        match &filter.kind {
            crate::widgets::WidgetFilterKind::DateRange => {
                // A date range is two params, and a half-specified range is not
                // a range — only persist and apply it when both ends are given.
                if let (Some(s), Some(e)) = (params.start.clone(), params.end.clone()) {
                    let _ = models::set_widget_filter(user.id, &key, "start", &s).await;
                    let _ = models::set_widget_filter(user.id, &key, "end", &e).await;
                } else {
                    if params.start.is_none() {
                        params.start = saved.get("start").cloned();
                    }
                    if params.end.is_none() {
                        params.end = saved.get("end").cloned();
                    }
                }
                filter.active_start = params.start.clone();
                filter.active_end = params.end.clone();
            }
            crate::widgets::WidgetFilterKind::Period { .. } => {
                // Resolved above; mirror it so the chip strip highlights.
                filter.active = params.period.clone().or(filter.default.clone());
            }
            crate::widgets::WidgetFilterKind::Choice { .. } => {
                let fk = filter.key.clone();
                if let Some(explicit) = params.raw.get(&fk).cloned() {
                    if let Err(e) = models::set_widget_filter(user.id, &key, &fk, &explicit).await {
                        tracing::warn!(
                            user = user.id, widget = %key, filter = %fk, error = %e,
                            "admin: failed to persist widget filter (continuing render)"
                        );
                    }
                    filter.active = Some(explicit);
                } else {
                    let resolved = saved.get(&fk).cloned().or_else(|| filter.default.clone());
                    if let Some(v) = &resolved {
                        params.raw.insert(fk, v.clone());
                    }
                    filter.active = resolved;
                }
            }
        }
    }

    // Each control must carry the OTHER controls' current values in its URL.
    // Without this, clicking "30d" on a widget filtered to status=paid would
    // navigate to `?period=30d` alone and silently drop the status — the filter
    // strip would lie about what the user is looking at.
    let snapshot: Vec<(String, Option<String>, Option<String>, Option<String>)> = filters
        .iter()
        .map(|f| {
            (
                f.key.clone(),
                f.active.clone(),
                f.active_start.clone(),
                f.active_end.clone(),
            )
        })
        .collect();
    for filter in &mut filters {
        let mut carry = String::new();
        for (other_key, active, start, end) in &snapshot {
            if *other_key == filter.key {
                continue;
            }
            if let (Some(s), Some(e)) = (start, end) {
                carry.push_str(&format!(
                    "&start={}&end={}",
                    crate::util::urlencoding_simple(s),
                    crate::util::urlencoding_simple(e)
                ));
            } else if let Some(v) = active {
                carry.push_str(&format!(
                    "&{}={}",
                    crate::util::urlencoding_simple(other_key),
                    crate::util::urlencoding_simple(v)
                ));
            }
        }
        filter.carry_lead = carry.trim_start_matches('&').to_string();
        filter.carry = carry;
    }

    let data_fn = widget.data.0.clone();
    let payload = data_fn(user, params.clone()).await;

    if is_htmx(&headers) {
        let kind = widget.kind.as_str().to_string();
        let title = widget.title.clone();
        let payload_json = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        // Pass the active period through to the template so the
        // chip strip can highlight the current selection.
        let active_period = params.period.clone().unwrap_or_default();
        let widget_key = widget.key.to_string();
        let filters_json = serde_json::to_value(&filters).unwrap_or(serde_json::Value::Null);
        // Declared vs. synthesized: a line chart with no declared filters keeps
        // its own inline chip strip (the historic behaviour). One that declares
        // filters hands rendering to the generic strip and suppresses its chips,
        // so a widget never shows two competing period strips.
        let has_declared_filters = !widget.filters.is_empty();
        match render(
            "admin/widget_data.html",
            context!(
                kind                 => kind,
                title                => title,
                payload              => payload_json,
                widget_key           => widget_key,
                active_period        => active_period,
                filters              => filters_json,
                has_declared_filters => has_declared_filters,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Json(serde_json::json!({
            "key": key,
            "kind": widget.kind.as_str(),
            "title": widget.title,
            "payload": serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null),
        }))
        .into_response()
    }
}
