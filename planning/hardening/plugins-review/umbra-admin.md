# umbral-admin — holistic review (hardening)

Scope: `plugins/umbral-admin/{src,tests,templates,build.rs}` (~17.5k LOC src+tests). Read-only. Net-new findings only; already-filed items cross-referenced. Severity ∈ {Critical, required, Important, Optional, Nit, FYI}.

## Verdict: Has gaps

The admin is a genuinely deep, polished CRUD surface — sheets, HTMX inline edit, FK/M2M pickers, filter facets, command palette, dashboard widgets, audit history, per-table prefs, password-field handling, file/image upload, custom row/bulk actions, configurable base path. It clears the "Django admin baseline" comfortably on the *single-model* axes. The gaps are (1) a stubbed-but-advertised feature: **inline editing of related children** (`InlineModel` is stored and never rendered — Django's flagship `TabularInline`/`StackedInline`); (2) a pervasive **i64-PK assumption** that silently drops/mis-handles String/Uuid PKs in bulk actions, audit log, and history; and (3) two **`.at()` custom-base-path bugs** where handler-emitted HTML hardcodes `/admin/...`. Soft-delete on the dynamic path (admin's own delete buttons hard-delete) is real but lands on the ORM (gaps2 #35), not this plugin.

## Completeness

Django-admin features and their umbral-admin status:

| Feature | Status | Evidence |
|---|---|---|
| List/detail/create/edit/delete | Shipped | `handlers/{list,crud,sheet}.rs` |
| `list_display` / `list_filter` / `search_fields` / `ordering` / `list_per_page` | Shipped | `config.rs:337-403`, `handlers/list.rs` |
| `readonly_fields` (+ auto sensitive-column readonly) | Shipped | `config.rs:386-398`, `view.rs:148-160` |
| Bulk + row actions, custom actions, `delete_selected` | Shipped | `config.rs:130-258`, `handlers/actions.rs` |
| FK widget (paginated searchable picker) | Shipped | `handlers/fk_picker.rs` |
| M2M widget (checkbox list) | Shipped but unbounded | `view.rs:474-570` — see already-#72 |
| Inline cell edit (double-click → HTMX) | Shipped | `handlers/inline_edit.rs` |
| Audit log / history timeline | Shipped (i64-only) | `models.rs`, `handlers/history.rs` |
| Dashboard widgets + per-user layout | Shipped | `handlers/dashboard.rs`, `widgets.rs` |
| Password-field special handling | Shipped | `handlers/sheet.rs:363`, `view.rs:247-265` |
| File/Image upload | Shipped | `handlers/crud.rs:43-56`, `files.rs` |
| Per-action permissions (`view/add/change/delete_<model>`) | Shipped (handler+template) | `permcheck.rs`, gated on Change for actions |
| **Inline editing of CHILDREN (`TabularInline`)** | **STUB** | `config.rs:265-274,405-408` — see below |
| **Sidebar per-model `view_` permission filter** | **MISSING** | `registry.rs:111` `apps(_viewer)` ignores viewer |
| **`Action::permission(codename)` enforcement** | **STUB** | `config.rs:142-144,207` — field stored, never checked |
| Date-hierarchy / drill-down | MISSING | no `date_hierarchy` anywhere |
| `save_and_add_another` / `save_and_continue` | Partial | `_save_continue` exists (`crud.rs:574`); no "add another" |
| Admin custom pages/views (`get_urls`) | MISSING | registry has no custom-route hook |
| `list_editable` (edit in changelist) | Partial | per-cell inline edit only; no bulk row save |
| `autocomplete_fields` / `raw_id_fields` | Partial | FK picker is the autocomplete; no `raw_id` mode |
| `fieldsets` / field grouping | MISSING | flat field list only |
| `prepopulated_fields` (slug from title) | MISSING (admin) | slug_from is ORM-side, not wired to admin JS |

### Completeness gaps (detail)

- **`InlineModel` is an advertised stub — no inline child editing. (`config.rs:265-274`, builder `config.rs:405-408`, public re-export `lib.rs:80`). Important.** `AdminModel::inlines(Vec<InlineModel>)` accepts and stores the value (`config.rs:291`), but **no handler or template ever reads `.inlines`** (grep: only def/store/test sites). The struct is even commented `// InlineModel (phase 2 stub)`. A user who writes `.inlines(vec![InlineModel{...}])` (the natural Django `TabularInline` port) gets a silent no-op — the parent edit form renders with zero child rows and no error. This is the single biggest Django-parity gap. Matches the deferred gaps2 #50 spirit (child inline-edit) but #50 is scoped to *cell* inline-edit on children; the whole inline-children *surface* is absent. Fix: render an inline child table on the detail/edit page driven by `inlines`, or remove the builder + type until it ships (a stored-but-ignored builder is a "fix don't patch" violation — it hides the missing feature). gap: NEW.

- **Sidebar shows every model to every staff user regardless of `view_<model>` permission. (`registry.rs:111`, `apps(&self, _viewer: &AuthUser)`). Important.** The doc-comment (`registry.rs:22-27`) admits it: "Today … passes every entry through for any staff user … When umbral-permissions lands (gap 33), add a `view_<table>` permission check per entry." Permissions HAVE landed (`permcheck.rs` is in use), but `apps()` still ignores `_viewer`. Effect: a staff user without `view_secret_model` still sees it listed in the sidebar (clicking through *is* gated by the changelist's `require(View)` at `list.rs:425`, so it's information-disclosure of model existence, not data access). Fix: filter `merged` through `permcheck::check(viewer, plugin, table, View)` before grouping. gap: NEW.

- **`Action::permission(codename)` is stored but never enforced. (`config.rs:142-144` field, `:207` builder).** The doc-comment says "Full umbral-permissions integration deferred (gap 33); today gated on `is_staff` only." Bulk actions ARE gated on the model-level `change_<model>` perm now (`actions.rs:38-43,151-156`, "WEB-7"), but a per-action custom codename (e.g. a "publish" action requiring `blog.publish_post`) is silently ignored — any staff user with `change_` can fire it. Fix: when `action.permission` is `Some`, check that codename in `run_action`/`dispatch_action`. gap: NEW (relates to deferred gap 33).

- **Audit log + history are i64-PK only. (`models.rs:485` `object_id INTEGER`, `handlers/history.rs:33` `id.parse::<i64>()`).** History 400s for any String/Uuid-PK model, and `log(... id.parse::<i64>().ok())` at `crud.rs:563,670,726`, `inline_edit.rs:172`, `sheet.rs:431` records `NULL` object_id for non-i64 PKs — the timeline silently loses the object linkage. Tied to the PK-lift (MEMORY project_primary_key_refactor); flagged in correctness-domain FYI but the admin schema column is the concrete blocker. gap: NEW (PK-lift family).

- **`on_ready` is a no-op (`lib.rs:788-793`).** Correct by design (tables come from the migration engine via `models()`), but the docs-audit flagged `plugins/admin.mdx` falsely claims "`on_ready` runs DDL." The code is right; the doc is wrong — already in the doc-fix batch. FYI.

## Findings

### Critical
- *(none net-new in this plugin)*. The admin's own Delete buttons permanently hard-delete soft-delete models and changelists list trashed rows — but the root cause is `DynQuerySet` ignoring `meta.soft_delete`, **already gaps2 #35** (correctness-domain Critical). All admin delete paths (`crud.rs:664,720`, `actions.rs delete_selected` via `config.rs:233`) inherit the fix automatically once #35 lands. No admin-local change needed.

### required
- **Inline cell-edit form hardcodes `/admin/...`, breaking inline edit under `.at()`. (`handlers/inline_edit.rs:85`, `hx-post="/admin/{table}/{id}/cell/{field}"`).** Every other URL in the plugin routes through `crate::branding::current().base_path`, but this handler-emitted HTML fragment does not. Mount the admin at `.at("/backoffice")` and double-click-to-edit POSTs to `/admin/...` → 404, inline edit silently dead. Fix: interpolate `crate::branding::current().base_path` into the `hx-post`. gap: NEW.
- **FK-picker pagination buttons hardcode `/admin/api/...`, breaking picker paging under `.at()`. (`handlers/fk_picker.rs:201`, `hx-get="/admin/api/{table}/{field}/options?..."`).** Same class as above: Previous/Next in the FK searchable picker target `/admin/...` regardless of the configured base. First page renders (mounted route), but paging 404s. Fix: prefix with `current().base_path`. gap: NEW.

### Important
- **Bulk actions silently drop non-i64 selected ids. (`handlers/actions.rs:49-53,167-172`, `selected.parse::<i64>().ok()` / `ids.as_i64()`).** `ActionInvocation.ids: Vec<i64>` (`config.rs:99`) and the `delete_selected` builtin uses `filter_in_i64` (`config.rs:234`). On a String/Uuid-PK model, every selected row whose PK isn't an integer is `filter_map`-dropped — the user selects 5 rows, the action fires on 0, and the toast still reads "Deleted N row(s)" (N from `inv.ids.len()`, which is already-filtered, so it's accurate-but-empty). Silent no-op bulk delete. Fix: make `ids` PK-type-agnostic (carry `Vec<String>` + dispatch like `dynamic.rs` filter_in_strings), as the M2M/junction lift did. gap: NEW (PK-lift family; sibling of forms_runtime.rs:226 already-#73).
- **`permcheck` model-level gate has zero test coverage.** No test in `tests/*.rs` exercises `permcheck::require` / `AdminPerms::load` denying a non-superuser without `change_<model>` (grep: the only `permission:` hits are widget `permission: None`). The whole #75 per-action authz surface — the thing standing between a low-privilege staff user and every model's edit/delete — is untested. A `<`/`>`-style regression or a `check()` returning the wrong default would pass CI. Fix: add a behavioral test (real user, real grant, assert 403 without / 200 with). gap: NEW.

### Optional
- **`umbral-permissions` and `umbral-security` are runtime deps but absent from `Plugin::dependencies()` (`lib.rs:493-497` lists only `["auth","sessions"]`).** Both are used unconditionally in the request path (`permcheck.rs`, `auth.rs:138,143,207`). They degrade gracefully (permissions → no-op fallback; security → self-mint CSRF), so it's not a crash — but the dependency graph under-declares what the admin needs, and an app that registers admin without security gets self-minted CSRF cookies it may not expect. Fix: either declare them or document the soft-dependency explicitly. gap: NEW.
- **`AdminPerms::load` fires 4 serial permission queries / page; plus `require(View)` before it (~12-14 round-trips per non-superuser changelist).** `permcheck.rs:118-125` runs View/Add/Change/Delete `check()`s sequentially, each up to 3 queries. **Already filed #72** (performance-scalability Important). Cross-ref only.
- **M2M form candidate list loads the entire target table (no LIMIT). (`view.rs:511`).** **Already filed #72** (performance Critical). Cross-ref only.
- **`#[allow(clippy::too_many_arguments)]` on `build_row_ctx` (`rows.rs:93`) and the 1232-LOC `widgets.rs` / 1146-LOC `view.rs`.** Cohesion: `view.rs` mixes sidebar shaping, form-field building, M2M-form DB fetches, validation, and detail-view mapping — five concerns. `widgets.rs` bundles 11 widget payload types + catalog + data-fn plumbing. Not in the >2800-LOC #78 split set, but candidates for the same treatment when the surface stabilizes. gap: NEW (minor; fold into #78 spirit).

### Nit
- **`confirm_delete_dialog` uses raw `#{id}` as the display label (`sheet.rs:227-228`)** with a "FK label resolution lands later" comment — the changelist already resolves FK labels (`list.rs:resolve_fk_label`); the confirm dialog could reuse it for a friendlier "Delete *Coffee Beans*?" prompt. gap: NEW.
- **`change_password_handler` hashes the password BEFORE the permission check (`sheet.rs:398` hash, `:407` permcheck).** Wasted argon2 work on an unauthorized request (a cheap DoS lever — argon2 is deliberately expensive). Reorder: permcheck before hash. gap: NEW.

### FYI
- **Plugin-contract: clean.** Imports the `umbral` facade for all core types; the direct `umbral-auth`/`umbral-sessions`/`umbral-permissions`/`umbral-security` deps are sibling-plugin API consumption (legitimate — the admin's login *is* auth+sessions), not core-internal reach-around. Owns its 2 migrations via `models()` (`lib.rs:521-526`). The only raw `sqlx::query` hits are `ensure_tables_for_tests` (`models.rs:464,479`) — the exact CLAUDE.md-sanctioned test-DDL exception. No `sqlx::query` on any production path.
- **`inline_edit.rs:163` silent empty-string write on malformed body — already gaps2 #73 / static-analysis #1.** Cross-ref.
- **`actions.rs` audit `summary` includes `selected_ids: Vec<i64>` — same i64 truncation as the action itself; folds into the bulk-action PK-lift fix above.**

## Tests

Strong breadth (21 test files, ~150 test fns): CRUD round-trip, changelist search/sort/filter/pagination, FK picker, inline cell edit (get/post/readonly-403), bulk action multi-id, audit-row writes, history timeline, dashboard catalog/layout/widget-data, palette, user prefs round-trip, multipart file upload + image preview, cross-crate O2O, CSRF header wiring, malicious-`next` rejection, login flows.

**Coverage holes (all NEW):**
- **Zero `permcheck` authz tests** — no test that a staff user without `change_<model>` gets 403 on edit/delete/action (the entire #75 gate is unverified).
- **No `.at()` custom-base-path test** — would have caught the two hardcoded-`/admin/` bugs (`inline_edit.rs:85`, `fk_picker.rs:201`).
- **No non-i64-PK test** — bulk action drop, history 400, audit NULL object_id all unexercised (every model under test uses i64 `id`).
- **No soft-delete-on-admin test** — `tests/` never asserts an admin Delete on a `#[umbral(soft_delete)]` model soft-deletes vs hard-deletes (the #35 blast radius).
- **No inline-children test** — consistent with the feature being a stub.
- `table_pref_malformed_json_in_db_reads_as_none` confirms the pref decode swallows corrupt JSON to `None` (intentional here, but the session-side sibling is the logged-warning gap in #71).
