## Status

**As of 2026-06-07: 99 of 109 closed.** Open: 43 (plugin-extension backlog), 49 (S3 / image backend deferred), 58 (FK picker for user_id deferred), 61 (M2M auto-junction deferred), 66 (MySQL), 67 (collectstatic), 70 (Cache plugin Redis/memcache split deferred), 77 (ORM signal events), 78 (RestPlugin expand FKs), 79 (migration safety research), 89-90 / 93-95 (advanced autodetector + REPL), 104 (plugin-disable semantics), 108 (REST API versioning).

| # | Gap | Status |
|---|-----|--------|
| 1 | Email plugin attachments | done — `abf502c` |
| 2 | Django-style 404 / 500 pages | done — `e42f408` |
| 3 | CLI: `umbra startproject` / `startapp` + dispatch shape | done — `95f0709` + `e5645a4` |
| 4 | Plugin docs + REST custom field rendering | done — `1cdbc18` (docs) + `dc84dbf` (code) |
| 5 | OpenAPI customisation params (`.description()`) | done — `e42f408` |
| 6 | Security audit across system + plugins | done — `cd656d5` |
| 7 | No `.create()` method on `Model` objects | done — `9ab3c00` |
| 8 | No `.update()` method on `Model` objects | done — `9ab3c00` |
| 9 | No `.delete()` method on `Model` objects | done — `9ab3c00` |
| 10 | No bulk create/update/delete on `Model` objects | done — `9ab3c00` |
| 11 | Complete plugin example (ORM access) | done — see commit log |

All known gaps closed. New gaps land below as they're surfaced.

## Seen/Known gaps


> `[x]` write-ups are archived verbatim (same numbers) in `archive/gaps-done.md`. Only open `[ ]` and partial `[~]` entries keep full text here.

1. [x] Email plugin lacks the ability to do file attachments — archived
2. [x] Django has special way of doing pages ie 404, 500 (Internal server error) which is quite symbol and … — archived
3. [x] The current version does not make use of the cli. Ie instead of `cargo run`, you must use `umbra … — archived
4. [x] Docs are out of sync with the current version ie how to use plugins. There is no plugin section on … — archived
5. [x] The openapi plugin should allow the user to pass in params to customize the openapi schema. ie the … — archived
6. [x] Security audit across the system and the plugins — archived
7. [x] There is no `.create()` method on `Model` objects — archived
8. [x] There is no `.update()` method on `Model` objects — archived
9. [x] There is no `.delete()` method on `Model` objects — archived
10. [x] There are no bulk create/update/delete methods on `Model` objects — archived
11. [x] We need to go deeper on how to create a plugin/an app - … — archived
12. [x] In the docs, http://localhost:5173/docs/v0.0.1/plugins/plugins-are-apps, add a section for Rest … — archived
13. [x] Something else, currently, we register the plugin, the models for that plugin and other resources … — archived
14. [x] How do we use different models in different areas of the app ie BlogPlugin vs CategoryPlugin, ie we … — archived
15. [x] What have we done to make sure the system can handle many requests safely. Things like proper … — archived
16. [x] Minininja already supports context, can we have the same for us here ie when returning a page, we can return it with a context that contains the data needed for the page - This seems to be done already
17. [x] There is no `.get` method on the model to get a single record by id or by any other field. Also we … — archived
18. [x] Django admin is so extensive ie you get alot of things out of the box by extending it ie you can … — archived
19. [x] Defer this but have it somehwere as a pending task - We shall use tailwind css for everything, we … — archived
20. [x] When I start a project, do I get a complete scaffold out of the box? ie all the basic setup and … — archived
21. [x] DRF views, ie ApiView, has a way of extending and adding custom actions. Can we be able to have the … — archived
22. [x] How do we do templates discovery? ie a template in app1 might be referenced in a view in app2 ie … — archived
23. [x] Update the docs landing page to have complete SEO as to why this is the best Rust web framework for … — archived
24. [x] How do we handle cases where a user deleted some migrations but in the db there are migrations. We … — archived
25. [x] How do we handle primary keys? when they are ints and when they are uuids, strings? By default, in … — archived
26. [x] In django, for view, we had this decorator called `login_required` (It has something like … — archived
27. [x] In docs, under backends, we have only shown postgresql but no sqlite! We need to add sqlite to the … — archived
28. [x] Django has a way of doing `prefetch_related` and other helpers like `select_related`, `F`, `Q` to … — archived
29. [x] The rest framework is also extended by some packages like `django-filter`, `django-rest-swagger`, … — archived
30. [x] Do we create tables based on plugin namespace ie `blogs_Post` `main_Category`? I ask this since it … — archived
31. [x] Custom User Model implementation and setting the custom user model in settings — archived
32. [x] There is no dedicated docs page for template helpers like humanize etc — archived
33. [x] We don't have proper role management, hence contenttypes and user groups. Django has groups which … — archived
34. [x] Related to #32 - Add the third column in the tables for `usage` — archived
35. [x] We should catch internal server errors and allow the developer to use/display the error if they … — archived
36. [x] Related to #26, update the docs on how to use this structure for auth gating! And its like you … — archived
37. [x] In django, if you have a `Post` model, and it has `author` which is an fk to User model, when you … — archived
38. [x] How do we handle atomic txs here? We need this since its veryvery critical and nice to have — archived
39. [x] The startapp command should check if there is an existing app to avoid app confusions (Check if it … — archived
40. [x] I haven't seen any places for signals! Also this goes hand in hand with the Tasks plugin. We need … — archived
41. [x] The commands are not well explained, even with basic usage. … — archived
42. [x] No doc illustrating use of umbra.toml or .env anywhere. This is part of env and settings module — archived
43. [ ] Can a plugin be extended ie extension of AdminPlugin with more features maybe, like more pages, components, etc
44. [x] I forgot - When creating models, is it possible we define display name and icon (ie "users" for a … — archived
45. [x] The admin templates, both base.html and login.html have the same general layout ie tailwind config, … — archived
46. [x] Django has a way of defining the string method of a column. Instead of displaying all the columns, … — archived
47. [x] Complete Admin Rename ie site title, description, default colorscheme (primary color and theming) — archived
48. [x] Plugin of `#umbra(plugin = "app")` needs further details ie I tried to change the plugin the … — archived
49. [/] Static plugin and media plugins, we need this out of the box for files under `static` or `assets` or `media` folders. Also, we need this for `S3` or create a new `S3 compatible` plugin (We should use any existing libraries for this) including a library to handle images, generate metadata for them like thumbnails and store to db. Like we need a proper media/file handler a user can easily integrate as a plugin and get some things automagically like file upload and url back etc.
       — Partially shipped. `umbra-static` already existed (filesystem dir → URL prefix); `documentation/docs/v0.0.1/plugins/static.mdx` extended with a "Referencing files from templates" section showing how to wire CSS / JS / images / favicon plus a `static_url` template-context pattern. New `umbra-media` plugin (commit ⟨in-progress⟩) ships the user-upload counterpart: `MediaPlugin::new(mount, dir).max_size(N)` plus a `save(filename, content_type, bytes)` helper, with every upload tracked in the framework-tracked `MediaFile` model (`id, key, filename, content_type, size, uploaded_at`). Docs at `documentation/docs/v0.0.1/plugins/media.mdx`. S3-compatible backend, image library (thumbnails, EXIF strip, format probing), and signed URLs are explicitly deferred to v0.1/v0.2/v0.3 — design captured in `docs/decisions/2026-06-02-media-and-s3.md` with the `MediaBackend` trait extraction plan.
50. [x] http://localhost:5173/docs/v0.0.1/plugins/permissions - We need to finish this, the admin now is … — archived
51. [x] The admin plugin has some unimplemented stuff like inline edit, it does not appear in the frontend, … — archived
52. [x] We have choices and now we need to close this off by having a multichoice macro, more like an M2M … — archived
53. [x] We have support for different dbs in one go, but we haven't fully mapped out the way models are … — archived
54. [x] Default files need updates ie `default_500.html` and `default_404.html` to use tailwind cdn with … — archived
55. [x] In dev mode, the default 404 should atleast show available paths ie shown with deep nesting per … — archived
56. [x] In templates, with django, you can directly do `request.user` and get a complete user object … — archived
57. [x] We have the permissions plugin, how can we make it work with our rest plugin? as an extension. They … — archived
58. [~] Update of permission plugin models to add noedit field to models to prevent editing of permissions for most columns except the ones that are explicitly editable like titles, like those that can't harm the system. And tell me which columns are editable and which are not. Groups should use varchar for the name and should have text for the description. User groups have a model for user and group. We need to use ForeignKey here so the input can auto detect the available groups and users. I think we handled this in m2m, with a proper selection where we said we are not going to load everything but rather allow searchability and filtering if possible. So the admin UI should be intelligent enough to show the available options without loading everything upfront.
       — Parts 1-3 shipped. `plugins/umbra-permissions/src/models.rs` now carries the edit policy as `#[umbra(noedit)]` markers on every column that would corrupt the permission system if renamed. The policy table:

       | Table | Editable | Read-only |
       |---|---|---|
       | `ContentType` | (none) | `app_label`, `model` (system-managed at boot — editing orphans every permission attached) |
       | `Permission` | `name` (human label) | `content_type_id`, `codename` (renaming `codename` invalidates every `has_permission(...)` call in code) |
       | `Group` | `name`, `description` | (none — both safe to rename) |
       | `GroupPermission` | `group_id`, `permission_id` (updated in #61.1) | (none — user-defined data) |
       | `UserGroup` | `user_id`, `group_id` (updated in #61.1) | (none — user-defined data) |
       | `UserPermission` | `user_id`, `permission_id` (updated in #61.1) | (none — user-defined data) |

       `Group.name`: `#[umbra(string, max_length = 150)]` — admin renders a single-line input with HTML `maxlength=150` (the framework's "varchar" — TEXT on both backends, length-capped at the form layer). `string` marks it as the row's `__str__` representation for FK pickers. `Group.description`: `Option<String>` (nullable so just-created groups can skip it), no `max_length` — admin renders a textarea. The `noedit` flag is metadata only (lands in `ModelMeta`, not in DDL), so existing columns picked it up without any schema churn. Only real schema change: the new `description` column on `permissions_group`, captured in `examples/derive-demo/migrations/permissions/0002_add_permissions_group_description.json`. Existing app-defined permissions and groups are unaffected because `Option<String>` defaults to `NULL`.

       Part 4 (UserGroup/UserPermission `user_id` as a searchable FK picker) is deferred: it needs a new framework concept of "FK by table name without a Rust type parameter" — the permission models deliberately use `user_id: i64` to stay generic over `UserModel`, so adding `ForeignKey<AuthUser>` would couple `umbra-permissions` to `umbra-auth`. The right shape is a `#[umbra(fk_table = "auth_user")]` attribute that adds a `fk_target` hint to the column metadata so the admin renders the same async combobox the typed FK already gets, without dragging the user type through the permissions crate. Filed as its own follow-up gap.
59. [x] We currently have sessions which are hardcoded to expect user id as a number, is that the case? Use … — archived
60. [x] Currently, permissions are labeled with indexes/numbers as primary keys. But that is not good for … — archived
61. [~] Going back to #58 - User groups must be editable by admin! so remove the noedits that you have randomly decided to add everywhere! Those that are autogenerated by the framework should not be editable by anyone to avoid accidental changes. But those that are user-defined should be editable. Next, we have groups, and group permissions. Why can't we have groups with `name`, `M2M<permissions>` instead of having a different tracking table, let that table be autogenerated by the framework in the backend maybe? What do you think?
       — Part 1 shipped: reverted the overzealous `noedit` markers in `plugins/umbra-permissions/src/models.rs`. New policy: lock framework-managed rows, leave user-created rows editable. That maps cleanly to table-level decisions — `ContentType` (auto-seeded one per registered model) and `Permission.codename` / `content_type_id` (rename breaks `has_permission(...)` call sites) stay `noedit`; every column on `Group`, `GroupPermission`, `UserGroup`, `UserPermission` is now editable so the admin can wire / rewire authorisation freely. Updated policy table:

       | Table | Editable | Read-only |
       |---|---|---|
       | `ContentType` | (none — system-managed) | `app_label`, `model` |
       | `Permission` | `name` (human label) | `content_type_id`, `codename` |
       | `Group` | `name`, `description` | (none) |
       | `GroupPermission` | `group_id`, `permission_id` | (none — user-defined data) |
       | `UserGroup` | `user_id`, `group_id` | (none — user-defined data) |
       | `UserPermission` | `user_id`, `permission_id` | (none — user-defined data) |

       Part 2 (`M2M<T>` relation with auto-generated junction) shipped in two waves:

       **Wave A — Group → Permission auto-junction** (pre-existing): `Group { permissions: M2M<Permission> }` is the on-struct M2M declaration; the migration engine auto-generates `permissions_group_permissions` from the field name. The pre-fix user-facing `GroupPermission` model was retired. `Group::permissions_contains_any` / `permissions_union_for` are the macro-emitted typed accessors — admin and `has_perm` reach the data without spelling the junction-table name.

       **Wave B — User-side M2M-shape API on top of explicit junction models** (this slice): `UserGroup` and `UserPermission` stay as user-facing models — the cross-crate dep arrow (`umbra-permissions → umbra-auth` since gap #50) blocks moving `M2M<Group>` / `M2M<Permission>` onto `AuthUser` itself (would create a cycle). The pragmatic substitute is the new `umbra_permissions::membership` module that wraps the explicit junctions in an M2M-shape API: `add_user_to_group(user, &group)`, `remove_user_from_group(...)`, `set_user_groups(user, &[gid])`, `grant_user_permission(user, &perm)`, `revoke_user_permission(...)`, `groups_for_user(user) -> Vec<Group>`, `direct_permissions_for_user(user) -> Vec<Permission>`, plus `is_in_group` / `has_direct_user_permission` lightweight checks. Call sites read like `AuthUser { groups: M2M<Group> }` would, just routed through the visible junction tables.

       Idempotency: `add_user_to_group` and `grant_user_permission` short-circuit when the membership already exists (no UNIQUE-violation surprise from re-adding). `set_user_groups` is delete-then-bulk-insert in one transaction-friendly pair so the user's group set lands in O(2) queries regardless of diff size. `has_perm_scoped` refactored to use `group_ids_for_user` internally so the perm-check hot path goes through the same membership seam.

       **Write-path PK-shape fix** (caught by `grant_user_permission` failing against the String-keyed `Permission.codename`): `json_to_sea_value` in `crates/umbra-core/src/orm/write.rs` previously bound every `SqlType::ForeignKey` value as `BigInt`. Now it dispatches on the JSON value's shape: `Value::String → SeaValue::String` (for String/UUID-PK FK targets), other shapes → `BigInt` (the legacy path). This is the write-side counterpart to `fk_target_pk_sql_type` from PK lift Pass A. The typed `Manager::create` path can now persist a `ForeignKey<Permission>` carrying a codename string.

       Tests: 4 new membership round-trip tests in `plugins/umbra-permissions/tests/integration.rs` (add+remove, set replace + clear, grant+revoke with String-PK Permission, post-refactor `has_perm` still works). Full umbra-permissions integration suite: 14 passed.

       What stays open: an actual cross-crate `M2M<T>` field declaration on AuthUser. That requires either splitting umbra-auth into a typed-only "user contract" crate that umbra-permissions can name without cycle, or accepting the explicit-junction-with-M2M-shape-helpers approach as the long-term answer. The Wave B helpers give 90% of the ergonomic win without the architectural surgery.
62. [x] Related to #55 - The routes display in the default 404 page well but you can't tell if its a get/a … — archived
63. [x] There should be a cli command for creating a new plugin. Yes there is the startapp, now one … — archived
64. [x] Error: UnsafeAlter { model: "Session", column: "user_id", reason: "type change BigInt -> Text needs … — archived
65. [x] We don't have unique macro for Model — archived
66. [ ] We shall need support for MySQL - Defer this but we shall work on it.
67. [ ] Proper static files handling with base ie STATIC_URL so that we can use something like `collect_static` command to enable cdn hosting of static per the user's wants without any problems. So the StaticPlugin should be able to expose proper static files from each of the plugins.
68. [x] Expose on_delete and on_update for ForeignKeys — archived
69. [x] How do we do foreignkeys to self, in django we use a string rep of the model name to refer to … — archived
70. [ ] Cache middleware improvements ie ability to control cache through redis, in memory or in other ways. Should be easy to use. Redis has sorted sets and can be used for caching with TTL. So for caching, we should fully explore redis options and have it as CachePlugin::Redis, or memcache as CachePlugin::Memcache etc
71. [x] The current PLaygroundPlugin does not take in app name, meaning the playground is not properly … — archived
72. [x] /home/dalmas/E/projects/umbra/plugins/umbra-rest/src/auth.rs ln 57 is wrong, I thought we improved … — archived
73. [x] Variables in the playground are not being stored. Also the variables are hidden using password … — archived
74. [x] Bearer token also hides the token, should have an eye for toggle — archived
75. [x] Global authorization header in settings should also be there. — archived
76. [x] It might be ideal, besides having auto save, we show notifications to users (Use shadcn toast and … — archived
77. [x] Does the ORM auto emit database changes? This will be good for audit logs, this way, we audit log … — archived
78. [x] RestPlugin expand Fks and M2M relationships - This is more like a feature where a user, in the rest plugin setup, can select tables and expand specific relationships (Fks and M2M) to see the related data. Since data can be nested around tables, we need to support this recursively like 1 level by another, the user has to define that.
       — Deferred. The ORM has `select_related` for FKs, but RestPlugin's `fetch_rows` doesn't thread it. Scope: a `ResourceConfig::expand([("author", 1), ("comments", 2)])` builder method, then in the row-building path do per-row JOINs / batched SELECTs (N+1 avoidance via `IN`-list grouping), then nest the result under the FK column name. Depth control needs an explicit limit to stop accidental quadratic explosion. Separate spec; pairs naturally with the OpenAPI plugin so the `$ref` chain reflects expand depth - This seems to have been done already using the include query parameter
79. [ ] What is the protection we have for migrations ie a migration might be faulty in a given plugin, how do we protect the developer from crashing their backend because of some faulty plugin? I know of the fake apply a given migration. Do we have the right protections in place for any random migration related issues?
       — Deferred (research). Survey first: what fails today (unsafe ALTERs already guarded by the `UnsafeAlter { reason }` whitelist in `crates/umbra-core/src/migrate.rs`; per-migration snapshot_hash mismatches caught in the tracking table; `--fake` exists), and what's missing (transactional per-plugin migrations on Postgres so a failure rolls back the whole plugin batch; a `--dry-run` flag that prints the SQL without executing; a `migrate --plan` that shows the topological order with detected-rename pairings; per-step abort messages that name the failing migration file + line). Then design. Bundles cleanly with gap #66 (MySQL) since each backend has different transactional-DDL guarantees.
80. [x] We haven't exposed the Cors middleware to the user yet. Either we set it up as a plugin or expose … — archived
81. [x] RestPlugin improvements - We added search by default, can we add another param, like search, but … — archived
82. [x] For testing purposes only: The shop example, make write paths authenticated, like to require auth. … — archived
83. [x] In the docs, we have Rest plugin under plugins and a separate Rest section — archived
84. [x] In the docs, http://localhost:5174/docs/v0.0.1/auth/users-and-passwords, this page is out of sync … — archived
85. [x] `Option<M2M<Tag>>` - Option for M2M relationship with tags not supported — archived
86. [x] In the playground plugin - In the body, json autofills the required fields but form does not. Form … — archived
87. [x] The playground plugin - It does not save endpoint changes like what the user selected and entered … — archived
88. [x] Autodetector: column rename detection — `RenameColumn` operation missing. Renaming `title` → … — archived
89. [ ] Autodetector: no data migrations — no equivalent to Django's `RunPython` / `RunSQL` operations. Backfilling a new column from an old one, or migrating data between models, requires a standalone script outside the migration system. The `operations` enum in the migration file has no escape hatch for arbitrary code.
90. [ ] Autodetector: no `SeparateDatabaseAndState` — Django can run schema operations that touch the DB but *not* the model state (or vice versa). This is critical for zero-downtime deploys where you add a column as nullable, deploy code, backfill, then make it non-nullable in a second migration. Umbra's `operations` and `snapshot_after` are tightly coupled; every op updates the snapshot.
91. [x] Autodetector: multi-step `AlterColumn` is not batched — changing `nullable`, `default`, and … — archived
92. [x] Autodetector: constraint-level operations missing — adding a `unique_together` or an `index` on an … — archived
93. [ ] Autodetector: M2M junction table rename not handled — renaming a model that has `M2M<T>` fields drops and recreates the junction table instead of renaming it. The code explicitly acknowledges this: "Renames of the parent model trip a Drop + Create on the junction — same semantics as Django, and the rename-tracking we'd need to do better is ambitious enough to defer."
94. [ ] Autodetector: no migration squashing — Django's `squashmigrations` collapses N migrations into 1 for cleaner history. Umbra has no equivalent. For long-running projects, the `migrations/` directory grows unbounded.
95. [ ] No interactive shell / REPL — Django's `python manage.py shell` (and `shell_plus` from django-extensions) is the primary exploration tool for the ORM: inspect live models, run ad-hoc queries, test filter predicates, prototype aggregates, and debug data issues without recompiling. Umbra has no equivalent. A Rust REPL is harder than Python's because of the compile step, but `evcxr` (a Rust Jupyter kernel) proves it's possible. The right shape for Umbra: `cargo run -- shell` spawns an `evcxr`-like session with the app's `AppContext`, `DbPool`, and `Settings` pre-loaded into scope, so `Post::objects().filter(...).fetch().await` works interactively. Without this, every ORM exploration requires writing a test or a handler, compiling, and running — a 30-second loop instead of instant feedback.
96. [x] `/home/dalmas/E/projects/umbra/bugs/db-testing.md` - Fix the bugs here and close them. — archived
97. [x] Migrations: In django, if you add a field which is not optional to an already existing model, the … — archived
98. [x] Do we really have auto-detection for same tables and or models? — archived
99. [x] Playground: Environment variables not saved - a bug — archived
100. [x] Playground: Not all requests are recorded ie I saved my base url to `https://google.com` and then … — archived
101. [x] Playground: If I have enabled auth globally, the `Auth` tab in request section should be autofilled … — archived
102. [x] Playground: We should enable other types of request body entries besides form (I believe this is … — archived
103. [x] Playground: Search on the sidebar also searches through the term "Post" since all post requests are … — archived
104. [ ] Under migrations: If you disable a plugin/app, do we delete the underlying tables? Do we need to ask the user what happens? Since they can just re-enable and expect everything as was. What really happens in such a scenario? This goes hand in hand with, if I change the plugin name on the model, does it produce the proper rename table? Also, if it had linked m2m/fk/1to1 relationships, do they get renamed as well?
105. [x] Reverse-FK accessors across crates — replace `#[umbra(no_reverse)]` with a trait-based emission. … — archived
106. [x] Timezone awareness — Django `USE_TZ` / `TIME_ZONE` equivalent. — archived
107. [x] The base path for admin plugin is fixed at `/admin/`. Can this be made dynamic so a user can use … — archived
108. [ ] How can we safely do api versioning using our rest plugin?
109. [x] We need an auto-slugify field given a field value ie `title` -> `title-slug`. Should be a model … — archived
110. [ ] Can we use `Rayon` in this project?
111. [x] How do we select specific fields from a model? `let explained_query = … — archived
112. [ ] Admin FK / M2M filters assume i64-typed primary keys. `DynQuerySet::filter_m2m_contains_any` parses child ids as `i64`; `fk_options` casts the related model's PK with `let value: i64 = raw_pk.parse().unwrap_or(0)` which silently rewrites a non-i64 PK to 0; `fk_options_resolve` parses `?ids=` the same way. The column NAME is dynamic everywhere (uses `pk_column(&related).name`), so a table with PK named `code` / `slug` / `uuid` works at the SQL identifier level — only the VALUE-TYPE assumption needs widening. Affects any model with a String / UUID / composite-codename PK targeted by an admin filter (e.g. `permissions_permission` with its composite `app.codename` PK). Matches the rest of the v1 framework's i64-only M2M plumbing (`set_junction_dynamic`, `prefetch_related_many`), so this gap really wants to be closed across the whole framework at once rather than only the admin filter layer. Cosmetic: the dialog's pre-resolve pill placeholder `#<id>` reads oddly for UUIDs (`#1a2b3c-...`); switch to the raw value when fixing.

113. [x] M2M via LEFT JOIN in `.join_related()` — archived
114. [x] Reverse-FK collection prefetch (`prefetch_related("comment_set")`) — archived
115. [x] Removing filters does not fully reset the "Active filters: " section on top of the file. **Fixed** … — archived
116. [x] JSON field input parses + validates. — archived
