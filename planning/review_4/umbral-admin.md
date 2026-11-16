# review_4 · umbral-admin

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 28,018 LOC (`plugins/umbral-admin`) |
| **Reviewer** | claude (review_4 wave agent) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 4 high · 2 medium · 3 low · 1 info — 10 new, 3 known |

## Summary

- `umbral-admin` is the batteries-included auto-CRUD surface: per-model changelists/forms/action sheets, inline cell edit + inline formsets, bulk actions, FK picker, command palette, dashboard + widgets, custom views, audit history, and user prefs — all driven off `ModelMeta` with no per-model Rust.
- Overall state is strong where it counts: permission gating is centralized (`permcheck`), applied to every mutating route, and CSRF participation is complete; the plugin writes through the ORM only (no raw SQL outside the documented test-only DDL helper). The historic escalation class is genuinely closed at the ORM terminal.
- The recurring defect class is **drift between sibling write/read implementations**: the mass-assignment skip-set is computed in `rows.rs` but *not* in `inlines.rs` (ADMIN-1), and the changelist search/filter/sort surface never got the visibility fix its REST twin received (ADMIN-2, gaps4 #4).
- Both documented password flows are dead at HEAD: the dedicated Change Password endpoint 400s on every call (ADMIN-3 — a direct consequence of the gaps4 #1 hardening), and create-with-password computes an argon2 hash and then drops it from the INSERT (ADMIN-4 — broken since the feature shipped). Zero test coverage on either.
- Fix first: make the skip-set a single ORM-derived value every write terminal applies (kills the ADMIN-1/ADMIN-4 class), give the password column an authorized server-side writer (ADMIN-3/4), and gate admin search/filter/sort on serialize-visible columns (ADMIN-2).

## Findings

### ADMIN-1 · HIGH · security · self-verified
**Inline formset child writes never apply the mass-assignment skip-set — a crafted parent POST writes any non-privileged child column, including `noform`/sensitive/model-readonly ones.**

- **Where**: `plugins/umbral-admin/src/inlines.rs:372-410` (skip-set), `inlines.rs:299-345` (`parse_inline_rows`, no allow-list)
- **Evidence**:
  ```rust
  // Skip readonly fields on writes. The FK is NOT skipped — we set
  // it explicitly to the parent PK below, and the parsed row's
  // `values` never carry the FK (it's excluded from the inline
  // form fields), so there's no mass-assignment surface to guard.
  let skip: Vec<String> = inline.readonly_fields.clone();
  ```
  then, per child row:
  ```rust
  DynQuerySet::for_meta(&child)
      .insert_form_in_tx(tx, &form, &skip)
  ```
  Contrast the parent path, which uses the full derived set: `let skip = readonly_set(model, cfg);` (`rows.rs:240`, `rows.rs:261`).
- **Impact**: `inline.readonly_fields` is only what the developer typed on `AdminModel::inline(...)`, whereas `readonly_set` (rows.rs:296-337) additionally covers sensitive names (`password_hash`, `password`, `salt`, `secret*` — `config.rs:554-556`), every `#[umbral(readonly)]` model column, and every `noform`/`noedit` column. `parse_inline_rows` collects any posted key matching `inline-<child-table>-<i>-<field>` with no field allow-list, and `save_inlines_in_tx` never resolves the child's own `AdminConfig` or checks any child permission. Concrete trace: parent `Order` registered with an inline `order_line`; a staff user holding only `change_order` POSTs `/admin/order/new/` with the extra pair `inline_order_line-0-gift_note=...` (a `noform` child column) or `inline_order_line-0-api_secret=...` (a `secret*` column) — it is written. The `privileged` leg is still blocked, because the ORM's intrinsic guard runs inside `insert_form_in_tx`, so this sits one rung below gaps4 #1; but `noform`/sensitive/readonly columns are precisely what that guard family exists to protect. Second effect on *normal* saves: every non-skipped column is written unconditionally (`form.get(&col.name).unwrap_or("")`, `dynamic.rs:1806`), so an unrendered child column the narrow skip-set fails to cover is blanked (overriding a DB default) or, for a typed column that cannot parse `""`, fails the whole parent save.
- **Fix**: compute the child's skip-set with the same helper the parent uses — resolve the child's `AdminConfig` and call `readonly_set(&child, child_cfg)` at `inlines.rs:376`; the comment's claim that there is "no mass-assignment surface" is refuted by `parse_inline_rows`' prefix sweep. Durable fix: derive the skip-set from `ColumnMeta` inside the ORM write terminal so no call site can forget it (see out-of-scope note 2).

### ADMIN-2 · HIGH · security · self-verified
**The changelist `?search=` (and `?filter_<col>=` / `?sort=<col>`) surface is a blind extraction oracle over hidden plaintext columns — the read path redacts them, the query path does not.**

- **Where**: `plugins/umbral-admin/src/rows.rs:93-97` (search), `plugins/umbral-admin/src/pagination.rs:89-110` (filters), `crates/umbral-core/src/orm/dynamic.rs:751-759` (search builder), `dynamic.rs:542-554` (`visible_select_cols`)
- **Evidence**:
  ```rust
  if let Some(term) = search_term {
      // Pass cfg.search_fields when present; empty slice means
      // "search every searchable column" (DynQuerySet::search default).
      let restrict = effective_search_fields(cfg, model);
      qs = qs.search(restrict, term);
  }
  ```
  and the search builder's column sweep:
  ```rust
  for col in &self.meta.fields {
      ...
      SqlType::Text => Some(
          Expr::expr(Func::upper(Expr::col(Alias::new(&col.name))))
              .like(sea_query::LikeExpr::new(like_pat.clone()).escape('\\')),
      ),
  ```
  while the value redaction lives one layer away:
  ```rust
  pub(crate) fn visible_select_cols(&self) -> Vec<String> {
      self.select_cols.iter().filter(|name| { ... .is_none_or(|c| self.may_serialize(c)) }).cloned().collect()
  }
  ```
- **Impact**: a staff user with View on a model opens `/admin/<t>?search=<prefix>` and binary-searches any hidden plaintext Text column (a `secret*`/sensitive column, a `noform` internal note) by watching row counts — `search` LIKEs *every* Text column with no `may_serialize` filter, and the response then renders only visible columns, so the oracle is invisible in the UI. `?filter_<field>=<value>` (accepted for any field name, `pagination.rs:89-110`) is an exact-match oracle, and `?sort=<col>` orders by columns the read layer refuses to serialise. `has_search` is a UI flag only — the endpoint accepts `search` regardless of config. Scope note: `Masked` columns store ciphertext, so they are not a useful oracle; the exposure is plaintext hidden columns, which is exactly the set `visible_select_cols` hides. This is the same class gaps4 #4 fixed for REST by building a `queryable_fields` set — the admin (a `DynQuerySet` consumer) never got the sibling fix, and nothing in the admin layer can reach that set today.
- **Fix**: build the admin search/filter/sort field list from the columns the caller may serialise — expose the REST `queryable_fields`/`may_serialize` policy on `DynQuerySet` and intersect it in `rows.rs:93-97`, `pagination.rs:89-110`, and the order-clause builder.

### ADMIN-3 · HIGH · correctness · self-verified
**The dedicated change-password endpoint always fails: it drives `update_one` on a `noform` column, which update_one now refuses — the documented feature is dead.**

- **Where**: `plugins/umbral-admin/src/handlers/sheet.rs:517-520`; guard at `crates/umbral-core/src/orm/dynamic.rs:1493-1498`; column at `plugins/umbral-auth/src/lib.rs:322-329`
- **Evidence**: the handler
  ```rust
  if let Err(e) = DynQuerySet::for_meta(&model)
      .filter_eq_string(&pk.name, &id)
      .update_one(pw_col, &hash)
      .await
  ```
  versus the gaps4 #1 guard `update_one` now carries:
  ```rust
  if col_meta.noform || col_meta.noedit {
      return Err(DynError::Write(WriteError::Validator {
          field: col_meta.name.clone(),
          message: "column is not editable (noform/noedit)".to_string(),
      }));
  }
  ```
  and the target column: `#[umbral(noform, signal_skip)] pub password_hash: String` — the `noform` flag exists precisely so password management goes through *this* endpoint.
- **Impact**: every `POST /admin/{table}/{id}/change-password` for the canonical model (an auth user, `password_field("password_hash")` — registered by `examples/shop/src/main.rs:145` and `examples/derive-demo/src/main.rs:226`) returns a 400 `Validator` error before any write. The password sheet in the UI cannot work, and the admin docs’ Change Password flow is unusable. This is a regression introduced by the fix that closed the inline-edit escalation: the fix hardened the one terminal every *generic* caller uses, and the one legitimate specialized caller was never given an authorized path. Secondary, folded in: `serde_urlencoded::from_str(&body).unwrap_or_default()` (`sheet.rs:482`) silently converts a malformed body into "Password cannot be empty" instead of a parse error.
- **Fix**: give the handler an explicitly authorized single-column write — e.g. an `allow_privileged`-style or dedicated `update_password` terminal that hashes and writes a server-owned column without tripping the generic noform guard — and add the missing test (no test anywhere sets `password_field`); parse the body strictly (`map_err` to `BadInput`).

### ADMIN-4 · HIGH · correctness · self-verified
**Create-with-password hashes the password and then excludes the column from the INSERT — the hash is computed and dropped, so creation either fails (NOT NULL) or silently stores no password.**

- **Where**: `plugins/umbral-admin/src/rows.rs:218-246`; skip semantics at `crates/umbral-core/src/orm/dynamic.rs:1764-1766`; sensitive defaults at `plugins/umbral-admin/src/config.rs:554-556` and `config.rs:630-639`
- **Evidence**:
  ```rust
  let hash = umbral_auth::hash_password_async(plaintext)
      .await
      .map_err(|e| AdminError::BadInput(format!("password hashing failed: {e}")))?;
  let mut owned = form.clone();
  owned.insert(pw_col.to_string(), hash);
  form_owned = owned;
  &form_owned
  ```
  followed by
  ```rust
  let skip = readonly_set(model, cfg);
  ...
  .insert_form_in_tx(tx, form, &skip)
  ```
- **Impact**: `readonly_set` → `effective_readonly_fields` always includes every sensitive-named column, and `password_hash` matches `is_sensitive_column` (`config.rs:554-556`); `build_insert_form_query` skips every column in `skip` (`dynamic.rs:1764-1766`). So the freshly computed hash never reaches the statement. On the real schema — `password_hash text NOT NULL` (umbral-auth 0001 migration; live in `examples/shop/shop.db` and `examples/derive-demo`) — "Create user" with a password fails with a NOT NULL violation *after* paying argon2 cost; on a nullable schema it silently creates a user who cannot log in. The admin config even documents the intent: “On create forms, render a ‘Password’ + ‘Confirm password’ pair that hashes the value on save” (`config.rs:545-546`). Broken since feature birth (`2eec662f`); no test covers it; `examples/shop/src/main.rs:145` and `examples/derive-demo/src/main.rs:226` are live consumers.
- **Fix**: on the create path, remove the configured password column from the skip-set after `readonly_set` (its value is server-produced, never trusted input — `skip.retain(|c| c != pw_col)` in `insert_row_in_tx`), mirroring the authorized-writer pattern ADMIN-3 needs.

### ADMIN-5 · MEDIUM · security
**Built-in bulk “Delete selected” performs a hard delete gated only by the broad Change permission, while the sibling `delete_permanently` gets an explicit Delete escalation nine lines away.**

- **Where**: `plugins/umbral-admin/src/config.rs:224-261`; dispatch at `plugins/umbral-admin/src/handlers/actions.rs:70-93`; UI at `templates/_macros/data_table.html:1075-1085`
- **Evidence** (the action, with no `.permission(...)`):
  ```rust
  match umbral::orm::DynQuerySet::for_meta(&meta)
      .filter_in_strings(&pk_name, &inv.ids)
      .delete()
      .await
  ```
  and the precedent it diverges from:
  ```rust
  if action.key() == "delete_permanently" {
      if let Err(r) =
          crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Delete)
  ```
- **Impact**: a staff user holding `change_<model>` but not `delete_<model>` can permanently erase rows in one request (`POST /admin/<t>/actions/delete_selected`) although the single-row `DELETE` route requires the Delete codename — verified: only `delete_permanently` is escalated. The optional `Action::permission` defaults to `None`, so nothing else backstops it. UI half: the bulk-toolbar button (`data_table.html:1075-1085`, `id="bulk-delete-btn"`) is not wrapped in the `perms.can_delete` check the per-row buttons honour (471, 509), so the button renders for users the server will reject per-row.
- **Fix**: give `delete_selected` the same Delete escalation as `delete_permanently` (or default its `permission` to the model's delete codename) inside `run_action`, and gate the bulk button on `perms.can_delete` like the row buttons.

### ADMIN-6 · MEDIUM · security · self-verified
**The urlencoded admin form write path accepts arbitrary `FileField`/`ImageField` keys — `validate_storage_key` runs only in serde `Deserialize`, and no admin write seam calls it.**

- **Where**: `plugins/umbral-admin/src/handlers/crud.rs:49-57`; validation at `crates/umbral-core/src/orm/file_field.rs:61-88` (only caller: its serde impl, `file_field.rs:224-228`); consumer at `plugins/umbral-storage/src/media.rs:596-600`
- **Evidence**: `body_to_pairs` hands raw strings straight to the write terminals:
  ```rust
  } else {
      let s = String::from_utf8(body.to_vec())
          .map_err(|_| AdminError::BadInput("request body was not valid UTF-8".to_string()))?;
      serde_urlencoded::from_str(&s).map_err(|e| AdminError::BadInput(e.to_string()))
  }
  ```
  and the validator's docstring names the exact seam it fails to cover: “a client that controls the serialised form of a `FileField` / `ImageField` (a REST write, a deserialised admin payload) could otherwise point a record’s key at an absolute path or a `..`-traversal string”.
- **Impact**: a staff user POSTs any admin form with `file_key=/etc/passwd` or `file_key=../../secret` (values simply pass through `serde_urlencoded` → `insert_form`/`update_form` → `FileField::from(String)`, unvalidated); multipart writes mint safe keys via `parse_and_store_multipart`, and inline cell-edit refuses file/image columns, so the urlencoded parent-form submit is the exposed seam. The stored hostile key then flows to consumers that interpolate it raw — `format!("{base}{mount}/{key}")` in `plugins/umbral-storage/src/media.rs:596-600` — so download/proxy paths can be steered outside the intended prefix. Static trace; the storage-side containment of such a key needs runtime confirmation (routed to the storage slice). Overlaps core slice MD-2.
- **Fix**: real fix is core-side — call `validate_storage_key` in the dynamic form→value conversion (or make an unvalidated `FileField` impossible to construct); meanwhile the admin path should refuse file/image columns carrying non-minted keys.

### ADMIN-7 · LOW · security
**Logout discards the session-destroy result, so a storage error still shows the user a logged-out redirect.**

- **Where**: `plugins/umbral-admin/src/auth.rs:262-267`
- **Evidence**:
  ```rust
  let mut response = Redirect::to(&format!("{base}/login")).into_response();
  let _ = umbral_sessions::logout(&headers, response.headers_mut()).await;
  response
  ```
- **Impact**: a DB error during session deletion is swallowed by callers (`let _ =`); on a shared machine the user sees the login page while the server-side session row survives, and no trace of the failure is emitted. The CLAUDE.md “swallowed secondary error” pattern — the recovery path itself must log.
- **Fix**: `if let Err(e) = … { tracing::error!(error = %e, "admin: logout failed") }` and still redirect; whether logout should clear the cookie regardless is a sessions-side question (out-of-scope note 5).

### ADMIN-8 · LOW · correctness
**Bulk selection runs `parseInt(cb.value, 10)` on row PKs, so bulk actions/delete break for String/Uuid PKs and can target the wrong row for numeric-leading strings.**

- **Where**: `templates/_macros/data_table.html:818-820` and `888-890`
- **Evidence**:
  ```js
  var selected = Array.from(document.querySelectorAll('.row-cb:checked')).map(function(cb) {
    return parseInt(cb.value, 10);
  });
  ```
- **Impact**: with the PK refactor (String/Uuid primary keys are supported end-to-end), a Uuid-pk row yields `NaN` → `JSON.stringify([NaN])` → `[null]` (silently no-ops server-side), and a String pk like `123abc` is truncated to `123` — for `delete_selected` or any bulk action that means acting on a *different* row. The server-side string-pk suite (`tests/phase3_actions_str_pk.rs`) exercises the endpoint, not this JS, so nothing catches it.
- **Fix**: send the raw checkbox value (`return cb.value;`) — `filter_in_strings`/`ActionInvocation.ids` are string-typed already.

### ADMIN-9 · LOW · drift
**`dashboard_catalog` gates on `widget.permission` only, skipping the view-level `widget_gates` check its sibling `gate_widget` enforces — view-gated widgets are listed, then 403 on fetch.**

- **Where**: `plugins/umbral-admin/src/handlers/dashboard.rs:169-175` (catalog) vs `dashboard.rs:254` / `242-266` (`gate_widget`)
- **Evidence**:
  ```rust
  // gaps3 #6: omit widgets the user can't load. ... Same
  // per-widget `permission` check `dashboard_widget_data` enforces.
  for w in state.widget_catalog.iter() {
      if let Some(code) = w.permission {
          if !crate::permcheck::has_codename(&user, code).await {
              continue;
          }
      }
  ```
  but the data/export gate is stricter: `if let Some(code) = state.widget_gates.get(key) {` — the view-level codename recorded at build time.
- **Impact**: a widget contributed by a custom view carrying `.permission("view_x")` is inserted into `widget_gates` (lib.rs:729-758) but may have no `widget.permission`; the catalog offers it to a staff user lacking `view_x`, who adds it to the dashboard and then gets 403s from `dashboard_widget_data`. gaps3 #6 fixed only the `permission` leg; the comment's “Same check” claim is false at this commit.
- **Fix**: one shared predicate both call sites use — `can_view_widget(state, user, w)` checking `widget_gates` ∪ `widget.permission` — so the catalog and the fetch gate cannot drift again.

### ADMIN-10 · INFO · perf
**Palette search runs one sequential query per viewable model per keystroke.**

- **Where**: `plugins/umbral-admin/src/handlers/palette.rs:93` (`for (plugin_name, model) in discover_models()`), `:139` (`.search(&search_fields, query_term)`)
- One observation: each model with View permission costs a `DynQuerySet` fetch (limit ≤ 10) issued sequentially; a project with N viewable models pays N round-trips per palette input event. A `join_all`/`FuturesUnordered` fan-out (or early-exit once MAX_RESULTS is reached) would bound it. No action required.

## Strengths

- **Permission gating is centralized and uniformly applied.** `permcheck::require_codename(_msg)` is fail-closed on DB error with explicit superuser semantics, and every mutating handler opens with `require_staff` + the action's codename; `fk_picker` requires View on both endpoints, palette gates per-model View before querying, history requires View, custom views enforce the view's codename, and `Action::permission` is enforced before dispatch (`actions.rs:70-82`) with a correct explicit Delete escalation for `delete_permanently` — the pattern ADMIN-5 asks to extend.
- **CSRF participation is complete.** The htmx layer sends `X-CSRF-Token` from a body-level `hx-headers` (`wrapper.html:493`), every admin `fetch` posts it, and login self-verifies the token with a constant-time compare plus `sanitise_next` (with unit tests) — no admin route opts out.
- **ORM discipline.** Zero raw SQL outside the documented test-only DDL helper (`models.rs:552/567`); row writes go through `DynQuerySet` terminals (`insert_form_in_tx` / `update_form_in_tx` / `update_one` / `delete_in_tx`), so backend routing stays in core.
- **`upload.rs` is exemplary upload hardening**: staff gate, multipart-only, 5-MIME allow-list, magic-byte sniff (`sniff_raster_image`), SVG markup rejection, 10 MiB cap.
- **The historic fixes genuinely hold**: the gaps4 #1 guard is present at both altitudes (handler refusals *and* `update_one`'s intrinsic privileged/noform refusal, `dynamic.rs:1486-1498`), and `Masked` edit re-seal (gaps4 #5) is handled in `build_update_form_query` — this review found their *siblings*, not regressions of the fixes themselves (ADMIN-3 excepted, which is a consequence of the #1 fix).
- **Escaping pipeline**: minijinja autoescape on all templates, `escape_js`/`html_escape` helpers with unit tests, no `safe_string` misuse found in the `#dt-table`/widget rendering path.

## Clean areas checked

- **Raw SQL / ORM rule**: `grep -rn "sqlx::query" plugins/umbral-admin/src/` → only `models.rs:552/567` inside `ensure_tables_for_tests` (the CLAUDE.md test-time DDL exception, explicitly documented).
- **Permission matrix**: create/update/delete/sheet/actions/inline_edit/upload/history/prefs/custom_view all gated; `fk_picker` (both endpoints) requires View; palette requires per-model View; dashboard data/export via `gate_widget`; anonymous access redirected.
- **CSRF**: header + form-field acceptance via `SecurityPlugin`; htmx wires the token; login constant-time compare with tests; no admin route exempted.
- **Search/filter injection**: LIKE wildcards escaped (`escape_like_literal` + `.escape('\\')`); `order_by_col` silently drops unknown sort columns (no SQL error/injection — the concern is ADMIN-2's visibility half, not injection); `filter_eq_string` fails closed on unknown columns.
- **Mass-assignment on the main form paths**: parent `readonly_set` covers config + sensitive + model-readonly + noform/noedit, the ORM `privileged` guard is intrinsic in `insert_form`/`update_form`, and `allow_privileged_if` is correctly scoped to superusers — no bypass found beyond ADMIN-1 (child formsets) and ADMIN-4 (the password drop).
- **Inline cell-edit**: file/image columns refused, privileged/noform refused (`cell_edit_post` + `update_one`) — gaps4 #1 verified held.
- **Transactionality**: parent + inline children + M2M writes share one transaction (`save_parent_and_inlines`, `apply_m2m_selections_in_tx`) — gaps4 #14 verified held.
- **List rendering perf**: FK display resolves labels in batch rather than per row (no N+1 in the row loop), pagination caps page size, and the count query shares the filter builder with the fetch.
- **Audit/prefs models**: ORM-only writes; `object_id` is a string so Uuid/String pks survive (gaps3 #59 verified held).
- **Template escaping**: autoescape + `escape_js` covered by unit tests; the `#dt-table` conditional-render pipeline uses them consistently.
- **Tests (structural)**: inline happy-path + rollback suites exist (`tests/inlines.rs`), actions with string pks are covered server-side — but nothing tests `password_field` on either flow and nothing tests formset mass-assignment, which is exactly where three of the four HIGHs would have failed loudly.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps4 #1` — Admin inline cell-edit → superuser escalation `[fixed/verified held — guard present at both altitudes; the tracker text calling `update_one` "the one" write terminal that skipped the guard is incomplete — the surviving caller-level sibling is ADMIN-1]`
- `gaps4 #4` — REST list filter/search blind oracle over hidden columns `[fixed/verified held — the admin twin of the same class is unfixed, see ADMIN-2]`
- `gaps4 #5` — `Masked<T>` admin edit crypto-shreds the secret `[fixed/verified held]`
- `gaps4 #7` — Dynamic IN filters fail open `[fixed/verified held]`
- `gaps4 #14` — Admin M2M writes not atomic with the parent save `[fixed/verified held]`
- `gaps4 #61` — Make the admin frontend as clean as the Supabase dashboard `[open]`
- `gaps3 #6` — Dashboard widget catalog filters by `widget.permission` `[fixed/verified held — the `permission` leg only; the view-level `widget_gates` leg is ADMIN-9]`
- `gaps3 #7` — Custom-view paths validated at build `[fixed/verified held]`
- `gaps3 #8` — Per-widget permission checks batched `[fixed/verified held]`
- `gaps3 #59` — Hardcoded `i64` PK assumptions in plugins (audit history) `[fixed/verified held]`
- `gaps3 #76` — Admin-authored widgets: a card is a recorded query, not Rust code `[open]`
- `gaps3 #77` — Cross-table widgets (customers against sales) `[open]`
- `gaps2 #79` — Plugin completeness: `Action::permission` enforced before handlers `[fixed/verified held]`
- `review_3/claude` — `check_action_perm` second copy of `permcheck::require_codename` `[fixed/verified held — now `require_codename_msg` at `actions.rs:72-82`]`
- `review_3/claude` — `fetch_rows_filtered` dead branch `[fixed/verified held]`
- `review_3/codex` — admin M2M write not atomic with parent (`06_admin_m2m_write_not_atomic_with_parent.md`) `[fixed/verified held]`

## Out-of-scope notes

1. `crates/umbral-core/src/orm/dynamic.rs:751-773` — `search` LIKEs every Text column and `filter_eq_string`/`order_by_col` accept any known column name with no visibility policy; the gaps4 #4 `queryable_fields` set exists on the REST handler but is not reachable from other `DynQuerySet` consumers (admin slice; see ADMIN-2).
2. `crates/umbral-core/src/orm/dynamic.rs:1764-1766, 1806` — `build_insert_form_query`/`build_update_form_query` have no intrinsic noform guard (only the caller-passed `skip` + `privileged`); every admin write's mass-assignment safety rests on `readonly_set` being passed correctly at each call site — ADMIN-1 and ADMIN-4 are the two sites that weren't. A `ColumnMeta`-derived intrinsic guard would make the class impossible.
3. `crates/umbral-core/src/orm/file_field.rs:61-88, 224-228` — `validate_storage_key` runs only in serde `Deserialize`; the dynamic form write path never calls it (overlaps core slice finding MD-2; admin seam in ADMIN-6).
4. `plugins/umbral-auth/src/lib.rs:322-329` — the `password_hash` doc-comment says password management goes through the dedicated admin Change Password flow; that flow is currently dead (ADMIN-3/ADMIN-4). Fix the flow — the intent is right.
5. `plugins/umbral-sessions` — `logout()` returns a `Result` the admin caller discards (ADMIN-7); whether logout should still clear client state on a server-side error is a sessions-side API question.
6. `plugins/umbral-storage/src/media.rs:596-600` — `url()` interpolates the stored key raw (`format!("{base}{mount}/{key}")`); containment for hostile keys belongs at the storage boundary (ADMIN-6 routing).
7. `planning/gaps6.md #22` — `max_length` unvalidated → PG `varchar` 500; the admin truncates for display only (`rows.rs:143-155`) and inherits the 500 on over-long input.
8. `examples/shop/src/main.rs:145`, `examples/derive-demo/src/main.rs:226` — both register `.password_field("password_hash")`; they are live consumers of the ADMIN-3/ADMIN-4 breakage and are the natural smoke tests once fixed.
