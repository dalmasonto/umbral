//! `umbral gen-client` — a typed TypeScript client generated from the REST
//! surface (gaps3 #38 / Kikosi #1).
//!
//! `umbral typegen` gives a frontend the *shapes*. This gives it the *client*:
//! `new Umbral(url).from("post").filter({ status: "published" }).list()`, where
//! the filter object autocompletes to exactly this model's filterable fields and
//! their value types — because the same model registry that renders the OpenAPI
//! document knows every column's type, choices, FK target, and which lookups
//! (`__gte`, `__in`, `__contains`, `__isnull`) the REST list endpoint accepts.
//!
//! The generated code is plain, dependency-free TypeScript in two files:
//!
//! - `models.ts` — the row interfaces + choice unions (`umbral typegen` output,
//!   scoped to the REST-exposed models).
//! - `client.ts` — a `Filters`/`Ordering` type per model, the list envelope that
//!   matches the configured paginator, and the `Umbral` runtime.
//!
//! It reads the live registry + umbral-rest's per-resource config
//! (`filters_enabled_for`, `is_hidden`, `registered_base_path`,
//! `registered_pagination_style`), which are populated when the plugins' routes
//! are built — so `gen-client` runs as an offline CLI step with no server and no
//! database, and reflects the *exact* surface the app serves.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use umbral::migrate::{Column, ModelMeta};
use umbral_casing::pascal_case_from_ident;
use umbral_rest::PaginationStyle;

/// Generate the two client files: `(models_ts, client_ts)`.
///
/// Reads every registered model, keeps the REST-exposed ones, and resolves FK
/// value types against the *full* set (so a filter on an FK column gets the
/// target's real PK type even when the target model isn't itself exposed).
pub fn generate() -> (String, String) {
    let all: Vec<ModelMeta> = umbral::migrate::registered_plugins()
        .iter()
        .flat_map(|p| umbral::migrate::models_for_plugin(p))
        .collect();
    generate_for(&all)
}

/// [`generate`] over an explicit model list. `all` is every model the app knows
/// (used to resolve FK value types); the REST-exposed subset is what gets
/// emitted, decided by `umbral_rest::is_exposed`. Tests drive this so they don't
/// need a full `App::build`; the umbral-rest readers default sensibly when their
/// config isn't published (exposed, filters on, base path `/api`).
pub fn generate_for(all: &[ModelMeta]) -> (String, String) {
    let mut exposed: Vec<ModelMeta> = all
        .iter()
        .filter(|m| umbral_rest::is_exposed(&m.table))
        .cloned()
        .collect();
    exposed.sort_by(|a, b| a.name.cmp(&b.name));

    let models_ts = umbral::typegen::typescript_for(&exposed);
    let client_ts = client_ts(all, &exposed);
    (models_ts, client_ts)
}

/// The TypeScript type of a filter key's value for one (column, lookup) pair.
///
/// Mirrors the REST filter contract (see `umbral-rest/src/filtering.rs`):
/// comparisons carry the field's own type, `__in` is an array of it (the client
/// joins to the CSV the backend expects), substring lookups are `string`, and
/// `__isnull` is `boolean`.
fn filter_value_type(all: &[ModelMeta], model: &ModelMeta, col: &Column, lookup: &str) -> String {
    match lookup {
        "in" => format!("{}[]", umbral::typegen::ts_base_type(all, model, col)),
        "isnull" => "boolean".to_string(),
        "contains" | "icontains" | "startswith" => "string".to_string(),
        _ => umbral::typegen::ts_base_type(all, model, col),
    }
}

/// The filter key for a (column, lookup): bare name for `eq`, `col__lookup`
/// otherwise — exactly the query-param names the REST list endpoint parses.
fn filter_key(col_name: &str, lookup: &str) -> String {
    if lookup == "eq" {
        col_name.to_string()
    } else {
        format!("{col_name}__{lookup}")
    }
}

/// A column the client should see: not hidden by the REST layer.
fn visible(table: &str, col: &Column) -> bool {
    !umbral_rest::is_hidden(table, &col.name)
}

/// Every type name `models.ts` exports for the exposed models: the row interface
/// (`Post`) plus one union per single-choice column (`PostStatus`) — matching
/// typegen's naming exactly. Deduped and sorted for a stable import line.
fn model_type_names(exposed: &[ModelMeta]) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for model in exposed {
        names.insert(model.name.clone());
        for col in &model.fields {
            if !col.choices.is_empty() && !col.is_multichoice {
                names.insert(format!(
                    "{}{}",
                    model.name,
                    pascal_case_from_ident(&col.name)
                ));
            }
        }
    }
    names.into_iter().collect()
}

fn client_ts(all: &[ModelMeta], exposed: &[ModelMeta]) -> String {
    let base_path = umbral_rest::registered_base_path();
    let style = umbral_rest::registered_pagination_style();

    let mut out = String::new();
    out.push_str(&header(base_path, style));
    // Import the model-defined type names so the filter/resource types below can
    // reference them by their bare names (the same names `ts_base_type` emits),
    // and re-export everything so a consumer imports rows + client from one file.
    let names = model_type_names(exposed);
    if names.is_empty() {
        out.push_str("export * from \"./models\";\n");
    } else {
        let _ = writeln!(
            out,
            "import type {{ {} }} from \"./models\";",
            names.join(", ")
        );
        out.push_str("export * from \"./models\";\n");
    }

    // Per-model Filters + Ordering + write DTOs.
    for model in exposed {
        out.push('\n');
        push_filters_type(&mut out, all, model);
        push_ordering_type(&mut out, model);
        push_create_type(&mut out, all, model);
        push_update_type(&mut out, all, model);
    }

    // The list envelope (paginator-specific) + the resource map + runtime.
    out.push('\n');
    out.push_str(&envelope_type(style));
    out.push('\n');
    push_resource_map(&mut out, exposed);
    out.push('\n');
    out.push_str(&runtime(base_path, style));
    out
}

fn header(base_path: &str, style: PaginationStyle) -> String {
    format!(
        "// Code generated by `umbral gen-client`. DO NOT EDIT.\n\
         //\n\
         // Regenerate after any model or REST-resource change:\n\
         //     cargo run -- gen-client --out <this directory>\n\
         //\n\
         // A typed client for this app's REST API. Base path: {base_path}\n\
         // Pagination: {style:?}. Row types live in ./models.ts.\n\n"
    )
}

/// `export interface PostFilters {{ status?: PostStatus; views__gte?: number; ... }}`
fn push_filters_type(out: &mut String, all: &[ModelMeta], model: &ModelMeta) {
    let name = format!("{}Filters", model.name);
    if !umbral_rest::filters_enabled_for(&model.table) {
        let _ = writeln!(
            out,
            "/** Filtering is disabled for `{}`. */\nexport type {name} = Record<string, never>;",
            model.table,
        );
        return;
    }
    let _ = writeln!(
        out,
        "/** Filterable query parameters for `{}`. Every key is optional and \
         AND-combined server-side. */",
        model.table,
    );
    let _ = writeln!(out, "export interface {name} {{");
    for col in &model.fields {
        // The REST filter surface excludes the primary key and hidden columns.
        if col.primary_key || !visible(&model.table, col) {
            continue;
        }
        for lookup in umbral_rest::filtering::applicable_lookups(col) {
            let key = filter_key(&col.name, lookup);
            let ty = filter_value_type(all, model, col, lookup);
            // Every lookup key is optional.
            let _ = writeln!(out, "  \"{key}\"?: {ty};");
        }
    }
    out.push_str("}\n");
}

/// `export type PostOrdering = "id" | "-id" | "title" | "-title" | ...` — every
/// visible column, ascending or `-`-prefixed descending, matching the REST
/// `?ordering=` param.
fn push_ordering_type(out: &mut String, model: &ModelMeta) {
    let name = format!("{}Ordering", model.name);
    let mut variants: Vec<String> = Vec::new();
    for col in &model.fields {
        if !visible(&model.table, col) {
            continue;
        }
        variants.push(format!("\"{}\"", col.name));
        variants.push(format!("\"-{}\"", col.name));
    }
    if variants.is_empty() {
        let _ = writeln!(out, "export type {name} = never;");
    } else {
        let _ = writeln!(out, "export type {name} = {};", variants.join(" | "));
    }
}

/// Whether a column can appear in a *create* request body.
///
/// Excludes the server-managed columns the REST write path fills or refuses:
/// the primary key (assigned on insert), `#[umbral(noform)]` (stripped from
/// every write body), `#[umbral(privileged)]` (the mass-assignment guard strips
/// it by default), and `auto_now`/`auto_now_add` (stamped by the server).
/// A `#[umbral(noedit)]` column IS creatable — you can set a username on create,
/// you just can't change it later (see [`in_update`]).
fn in_create(col: &Column) -> bool {
    !(col.primary_key || col.noform || col.privileged || col.auto_now || col.auto_now_add)
}

/// Whether a column can appear in an *update* body: everything creatable, minus
/// `#[umbral(noedit)]` — the "set once, then read-only" fields.
fn in_update(col: &Column) -> bool {
    in_create(col) && !col.noedit
}

/// The TS type of a writable field's value: the same base type as the row (FK →
/// target PK, choices → union, scalar otherwise), plus `| null` for a nullable
/// column (you may write null to it).
fn write_field_type(all: &[ModelMeta], model: &ModelMeta, col: &Column) -> String {
    let base = umbral::typegen::ts_base_type(all, model, col);
    if col.nullable {
        format!("{base} | null")
    } else {
        base
    }
}

/// `export interface PostCreate {{ title: string; author: string; body?: string | null; ... }}`
///
/// A field is required (no `?`) only when it is non-nullable and has no server
/// default — otherwise the server can fill it, so the client may omit it.
fn push_create_type(out: &mut String, all: &[ModelMeta], model: &ModelMeta) {
    let name = format!("{}Create", model.name);
    let _ = writeln!(
        out,
        "/** Body for creating a `{}`. Server-managed columns (id, auto-timestamps, \
         privileged, no-form) are omitted. */",
        model.table,
    );
    let _ = writeln!(out, "export interface {name} {{");
    for col in &model.fields {
        if !in_create(col) {
            continue;
        }
        let optional = col.nullable || !col.default.is_empty();
        let q = if optional { "?" } else { "" };
        let _ = writeln!(
            out,
            "  {}{q}: {};",
            col.name,
            write_field_type(all, model, col)
        );
    }
    out.push_str("}\n");
}

/// `export interface PostUpdate {{ title?: string; ... }}` — a PATCH body. Every
/// field is optional (partial update), and `#[umbral(noedit)]` columns are gone:
/// the type won't let you change a set-once field.
fn push_update_type(out: &mut String, all: &[ModelMeta], model: &ModelMeta) {
    let name = format!("{}Update", model.name);
    let _ = writeln!(
        out,
        "/** Body for updating a `{}` (PATCH; all fields optional). `noedit` \
         columns are excluded — they can be set on create but not changed. */",
        model.table,
    );
    let _ = writeln!(out, "export interface {name} {{");
    for col in &model.fields {
        if !in_update(col) {
            continue;
        }
        let _ = writeln!(
            out,
            "  {}?: {};",
            col.name,
            write_field_type(all, model, col)
        );
    }
    out.push_str("}\n");
}

/// The list-response wrapper, shaped to the configured paginator.
fn envelope_type(style: PaginationStyle) -> String {
    let extra = match style {
        PaginationStyle::PageNumber => {
            "  total_pages: number;\n  current_page: number;\n  page_size: number;\n  \
             next: number | null;\n  previous: number | null;\n"
        }
        PaginationStyle::LimitOffset => {
            "  limit: number;\n  offset: number;\n  next: number | null;\n  \
             previous: number | null;\n"
        }
        PaginationStyle::None | PaginationStyle::Custom => "",
    };
    format!(
        "/** A list response. `results` + `count` are always present; the rest \
         depend on the paginator. */\n\
         export interface Paginated<T> {{\n  results: T[];\n  count: number;\n{extra}}}\n"
    )
}

/// `interface UmbralResources {{ "post": {{ row: models.Post; filters: PostFilters; ordering: PostOrdering }}; ... }}`
fn push_resource_map(out: &mut String, exposed: &[ModelMeta]) {
    out.push_str(
        "/** Maps each REST-exposed table to its row, filter, and ordering types. \
         `Umbral.from` keys off this. */\n",
    );
    out.push_str("export interface UmbralResources {\n");
    for model in exposed {
        let _ = writeln!(
            out,
            "  \"{table}\": {{ row: {name}; filters: {name}Filters; ordering: {name}Ordering; \
             create: {name}Create; update: {name}Update }};",
            table = model.table,
            name = model.name,
        );
    }
    out.push_str("}\n");
    // The PK type union, for `.get(table, id)`.
    let pk_types: BTreeSet<String> = exposed
        .iter()
        .filter_map(|m| m.pk_column().map(|c| ts_scalar_for_pk(c)))
        .collect();
    let union = if pk_types.is_empty() {
        "string | number".to_string()
    } else {
        pk_types.into_iter().collect::<Vec<_>>().join(" | ")
    };
    let _ = writeln!(out, "export type UmbralId = {union};");
}

/// The PK's TS scalar. A PK is never an FK or a choices column, so the plain
/// SqlType→TS scalar is right; `number` for int PKs, `string` for uuid/slug.
fn ts_scalar_for_pk(col: &Column) -> String {
    // Reuse ts_base_type with an empty model list — a PK has no FK/choice to
    // resolve, so it maps by its own SqlType.
    umbral::typegen::ts_base_type(
        &[],
        &ModelMeta {
            name: String::new(),
            table: String::new(),
            fields: vec![col.clone()],
            ..ModelMeta::default()
        },
        col,
    )
}

/// The fixed runtime — the `Umbral` class + query builder — parameterised by the
/// base path and paginator. Emitted verbatim; no per-model branching lives here,
/// so the type layer (above) is what makes each `.from(...)` call specific.
fn runtime(base_path: &str, style: PaginationStyle) -> String {
    let page_methods = match style {
        PaginationStyle::PageNumber => {
            "  /** 1-based page number (`?page=`). */\n  \
             page(n: number): this { this.params.set(\"page\", String(n)); return this; }\n  \
             /** Rows per page (`?page_size=`). */\n  \
             pageSize(n: number): this { this.params.set(\"page_size\", String(n)); return this; }\n"
        }
        PaginationStyle::LimitOffset => {
            "  /** Max rows (`?limit=`). */\n  \
             limit(n: number): this { this.params.set(\"limit\", String(n)); return this; }\n  \
             /** Rows to skip (`?offset=`). */\n  \
             offset(n: number): this { this.params.set(\"offset\", String(n)); return this; }\n"
        }
        PaginationStyle::None | PaginationStyle::Custom => "",
    };

    format!(
        r#"export interface UmbralOptions {{
  /** Sent as `Authorization: Bearer <token>` on every request. */
  token?: string;
  /** Extra headers merged into every request. */
  headers?: Record<string, string>;
  /** Custom fetch (SSR / tests). Defaults to the global `fetch`. */
  fetch?: typeof fetch;
  /** Base path of the realtime plugin, for `.on(...)`. Defaults to "/realtime". */
  realtimePath?: string;
}}

/** A live subscription started by `Umbral.on`. Call `close()` to stop it. */
export interface Subscription {{
  close(): void;
}}

/** Handlers for model-change events. Each receives the row your `expose(...)`
    projection carries (the id by default), so you know which row to refetch. */
export interface ModelEvents<Row> {{
  created?(row: Row): void;
  updated?(row: Row): void;
  deleted?(row: Row): void;
}}

/** Thrown when the API returns a non-2xx response. `body` is the parsed error. */
export class UmbralError extends Error {{
  constructor(public status: number, public body: unknown) {{
    super(`umbral: request failed with status ${{status}}`);
    this.name = "UmbralError";
  }}
}}

class Query<Row, Filters, Ordering> {{
  private params = new URLSearchParams();
  constructor(private client: Umbral, private table: string) {{}}

  /** Field filters — keys and value types are specific to this model. */
  filter(f: Filters): this {{
    for (const [k, v] of Object.entries(f as Record<string, unknown>)) {{
      if (v === undefined) continue;
      this.params.set(k, Array.isArray(v) ? v.join(",") : String(v));
    }}
    return this;
  }}

  /** Full-text `?search=` across the model's searchable columns. */
  search(term: string): this {{ this.params.set("search", term); return this; }}

  /** `?ordering=` — pass fields; prefix `-` for descending. */
  orderBy(...fields: Ordering[]): this {{
    this.params.set("ordering", (fields as unknown as string[]).join(","));
    return this;
  }}

{page_methods}
  /** Sparse fieldset (`?fields=`) — fetch only these columns. */
  fields(...cols: string[]): this {{ this.params.set("fields", cols.join(",")); return this; }}

  /** Fetch the list. */
  async list(): Promise<Paginated<Row>> {{
    const qs = this.params.toString();
    const path = `{base_path}/${{this.table}}/` + (qs ? `?${{qs}}` : "");
    return this.client._request<Paginated<Row>>("GET", path);
  }}
}}

/** A typed client for this app's REST API. `new Umbral("https://api.example.com")`. */
export class Umbral {{
  private realtimePath: string;
  constructor(private baseUrl: string, private opts: UmbralOptions = {{}}) {{
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.realtimePath = (opts.realtimePath ?? "/realtime").replace(/\/+$/, "");
  }}

  /** Subscribe to `created` / `updated` / `deleted` events for a model over the
      realtime stream. The model must be `expose(...)`-d to `group` on the server.
      `"post"` autocompletes to exposed tables; each handler gets a typed row.
      Returns a {{@link Subscription}} — call `close()` to unsubscribe. */
  on<K extends keyof UmbralResources>(
    table: K,
    handlers: ModelEvents<Partial<UmbralResources[K]["row"]>>,
    opts: {{ group: string }},
  ): Subscription {{
    void table; // present for the type layer; the group carries the model's events
    const url =
      `${{this.baseUrl}}${{this.realtimePath}}/sse?groups=` + encodeURIComponent(opts.group);
    const es = new EventSource(url, {{ withCredentials: true }});
    const onEvent = (ev: MessageEvent) => {{
      let env: {{ c?: string; e?: string; d?: unknown }};
      try {{ env = JSON.parse(ev.data); }} catch {{ return; }}
      if (env.c !== opts.group) return;
      const cb = (handlers as Record<string, ((row: unknown) => void) | undefined>)[env.e ?? ""];
      if (cb) cb(env.d);
    }};
    es.addEventListener("u", onEvent as EventListener);
    return {{ close: () => es.close() }};
  }}

  /** Start a query against a REST-exposed table. */
  from<K extends keyof UmbralResources>(
    table: K,
  ): Query<UmbralResources[K]["row"], UmbralResources[K]["filters"], UmbralResources[K]["ordering"]> {{
    return new Query(this, table as string);
  }}

  /** Retrieve one row by primary key. */
  get<K extends keyof UmbralResources>(table: K, id: UmbralId): Promise<UmbralResources[K]["row"]> {{
    return this._request(`GET`, `{base_path}/${{table as string}}/${{id}}/`);
  }}

  /** Create a row. The body type omits server-managed fields; `noedit` fields
      are allowed here (settable on create). */
  create<K extends keyof UmbralResources>(
    table: K,
    data: UmbralResources[K]["create"],
  ): Promise<UmbralResources[K]["row"]> {{
    return this._request(`POST`, `{base_path}/${{table as string}}/`, data);
  }}

  /** Partially update a row (PATCH). The body type excludes `noedit` fields. */
  update<K extends keyof UmbralResources>(
    table: K,
    id: UmbralId,
    data: UmbralResources[K]["update"],
  ): Promise<UmbralResources[K]["row"]> {{
    return this._request(`PATCH`, `{base_path}/${{table as string}}/${{id}}/`, data);
  }}

  /** Delete a row by primary key. */
  async delete<K extends keyof UmbralResources>(table: K, id: UmbralId): Promise<void> {{
    await this._request(`DELETE`, `{base_path}/${{table as string}}/${{id}}/`);
  }}

  /** @internal */
  async _request<T>(method: string, path: string, body?: unknown): Promise<T> {{
    const doFetch = this.opts.fetch ?? fetch;
    const headers: Record<string, string> = {{ "Accept": "application/json", ...this.opts.headers }};
    if (this.opts.token) headers["Authorization"] = `Bearer ${{this.opts.token}}`;
    const init: RequestInit = {{ method, headers }};
    if (body !== undefined) {{
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }}
    const res = await doFetch(this.baseUrl + path, init);
    const parsed = res.status === 204 ? null : await res.json().catch(() => null);
    if (!res.ok) throw new UmbralError(res.status, parsed);
    return parsed as T;
  }}
}}
"#
    )
}
