//! `umbral gen-client` — a typed client generated from the REST surface
//! (gaps3 #38 / Kikosi #1).
//!
//! `umbral typegen` gives a frontend the *shapes*. This gives it the *client*:
//! `new Umbral(url).from("post").filter({ status: "published" }).list()`, where
//! the filter object autocompletes to exactly this model's filterable fields and
//! their value types — because the same model registry that renders the OpenAPI
//! document knows every column's type, choices, FK target, and which lookups
//! (`__gte`, `__in`, `__contains`, `__isnull`) the REST list endpoint accepts.
//!
//! # Two files, one runtime
//!
//! - `client.js` — a single-file, dependency-free ES module: `Umbral`, `Query`,
//!   `UmbralError`. Usable straight from a `<script type="module">` with no
//!   build step, and by any bundler.
//! - `client.d.ts` — every type: row interfaces, choice unions, per-model
//!   `Filters` / `Ordering` / `Create` / `Update`, the paginator's envelope, and
//!   the class declarations.
//!
//! There is no `.ts` runtime: TypeScript's types *erase*, so the row/filter
//! types produce no JavaScript at all. Emitting `.js` + `.d.ts` means the
//! runtime exists exactly once (no bundler, no transpile step, no second copy to
//! keep in step), while `import { Umbral } from "./api/client"` still type-checks
//! fully — TS resolves the `.d.ts` for types and the bundler resolves the `.js`
//! for code. It's the shape every published SDK ships.
//!
//! # Realtime is delegated, not reimplemented
//!
//! `Umbral.on(...)` does NOT open its own `EventSource`. It loads the realtime
//! plugin's already-served `{realtimePath}/client.js` and calls
//! `umbral.realtime.model(...)`, inheriting the hard parts: ONE SSE connection
//! shared across every tab via `SharedWorker` (union-routed), presence, and
//! graceful degradation. A per-subscription `EventSource` would open one
//! connection per model — six subscriptions exhausts the browser's per-origin
//! connection cap.
//!
//! It reads the live registry + umbral-rest's per-resource config
//! (`filters_enabled_for`, `is_hidden`, `registered_base_path`,
//! `registered_pagination_style`, `registered_pagination_schema`,
//! `registered_security_schemes`), which are populated when the plugins' routes
//! are built — so `gen-client` runs as an offline CLI step with no server and no
//! database, and reflects the *exact* surface the app serves.

use std::fmt::Write as _;

use serde_json::Value;
use umbral::migrate::{Column, ModelMeta};
use umbral_casing::pascal_case_from_ident;
use umbral_rest::{PaginationScalar, PaginationSchema, PaginationStyle};

/// The generated client: one runtime module + one declaration file.
#[derive(Debug, Clone)]
pub struct GeneratedClient {
    /// `client.js` — the single-file ES-module runtime.
    pub js: String,
    /// `client.d.ts` — every type, including declarations for the runtime's classes.
    pub dts: String,
}

/// Generate the client from the live registry + REST config.
///
/// Reads every registered model, keeps the REST-exposed ones, and resolves FK
/// value types against the *full* set (so a filter on an FK column gets the
/// target's real PK type even when the target model isn't itself exposed).
pub fn generate() -> GeneratedClient {
    let all: Vec<ModelMeta> = umbral::migrate::registered_plugins()
        .iter()
        .flat_map(|p| umbral::migrate::models_for_plugin(p))
        .collect();
    generate_for(&all)
}

/// [`generate`] over an explicit model list. `all` is every model the app knows
/// (used to resolve FK value types); the REST-exposed subset is what gets
/// emitted, decided by `umbral_rest::is_exposed`.
pub fn generate_for(all: &[ModelMeta]) -> GeneratedClient {
    generate_with(
        all,
        umbral_rest::registered_base_path(),
        umbral_rest::registered_pagination_style(),
        umbral_rest::registered_pagination_schema(),
        &umbral_rest::registered_security_schemes(),
        umbral::routes::registered_openapi_paths().unwrap_or_default(),
    )
}

/// [`generate_for`] with the REST config passed explicitly rather than read from
/// umbral-rest's `OnceLock`s — the base path, the paginator's [`PaginationStyle`]
/// and (for a custom paginator) its declared [`PaginationSchema`], and the
/// OpenAPI security schemes that drive the client's auth. Exposure / hidden /
/// filters-enabled are still read from the (gracefully-defaulting) readers.
///
/// Tests and the pagination/auth demos drive this so they can exercise every
/// paginator shape and auth scheme without a full `App::build` (which can only
/// run once per process because the config is process-global).
pub fn generate_with(
    all: &[ModelMeta],
    base_path: &str,
    style: PaginationStyle,
    schema: Option<PaginationSchema>,
    security_schemes: &[(String, Value)],
    openapi_paths: &[(String, Value)],
) -> GeneratedClient {
    let mut exposed: Vec<ModelMeta> = all
        .iter()
        .filter(|m| umbral_rest::is_exposed(&m.table))
        .cloned()
        .collect();
    exposed.sort_by(|a, b| a.name.cmp(&b.name));

    let auth = AuthModel::from_schemes(security_schemes);
    let schema = schema.as_ref();
    let methods = page_methods(style, schema);
    let session = AuthEndpoints::discover(openapi_paths);

    let emit = Emit {
        all,
        exposed: &exposed,
        base_path,
        style,
        schema,
        auth: &auth,
        methods: &methods,
        session: session.as_ref(),
    };
    GeneratedClient {
        js: emit_js(&emit),
        dts: emit_dts(&emit),
    }
}

/// Return `models` with every `hide(...)`-ed column removed from each model's
/// field list — the response shape, since hide is response-only.
fn strip_hidden(models: &[ModelMeta]) -> Vec<ModelMeta> {
    models
        .iter()
        .map(|m| {
            let table = m.table.clone();
            let mut stripped = m.clone();
            stripped
                .fields
                .retain(|c| !umbral_rest::is_hidden(&table, &c.name));
            stripped
        })
        .collect()
}

// =========================================================================
// Pagination methods — described once, rendered into BOTH the .js impl and
// the .d.ts signature so the two can't drift apart.
// =========================================================================

/// One query-builder paging method, e.g. `page(n: number)` → `?page=`.
struct PageMethod {
    /// The JS/TS method name (camelCase): `pageSize`.
    name: String,
    /// The TS type of its single argument: `number`.
    ty: &'static str,
    /// The wire query param it sets: `page_size`.
    wire: String,
    /// Doc line.
    doc: String,
}

/// The paging methods this paginator exposes. Built-ins are known; a `Custom`
/// paginator contributes one method per param it declared in its
/// [`PaginationSchema`], and one that declared nothing contributes none (the
/// generic `.param(...)` escape hatch covers it).
fn page_methods(style: PaginationStyle, schema: Option<&PaginationSchema>) -> Vec<PageMethod> {
    let m = |name: &str, ty: &'static str, wire: &str, doc: &str| PageMethod {
        name: name.to_string(),
        ty,
        wire: wire.to_string(),
        doc: doc.to_string(),
    };
    match (style, schema) {
        (PaginationStyle::PageNumber, _) => vec![
            m("page", "number", "page", "1-based page number (`?page=`)."),
            m(
                "pageSize",
                "number",
                "page_size",
                "Rows per page (`?page_size=`).",
            ),
        ],
        (PaginationStyle::LimitOffset, _) => vec![
            m("limit", "number", "limit", "Max rows (`?limit=`)."),
            m("offset", "number", "offset", "Rows to skip (`?offset=`)."),
        ],
        (PaginationStyle::Custom, Some(s)) => s
            .params
            .iter()
            .map(|p| PageMethod {
                name: camel_case(&p.name),
                ty: scalar_ts(p.ty),
                wire: p.name.clone(),
                doc: format!("Custom pagination param (`?{}=`).", p.name),
            })
            .collect(),
        _ => Vec::new(),
    }
}

// =========================================================================
// client.js — the runtime. One copy, no types, no imports.
// =========================================================================

/// The `AuthClient` class + the `this.auth = …` wiring, or empty when the app
/// serves no auth endpoints (no dead code in a REST-only app).
fn auth_runtime_js(session: Option<&AuthEndpoints>) -> (String, String) {
    let Some(s) = session else {
        return (String::new(), String::new());
    };
    let login = s.login.clone().unwrap_or_default();

    let register = match &s.register {
        Some(p) => format!(
            r#"
  /** Register, then adopt the returned token (same shape as login). */
  async register(body) {{
    const out = await this.client._request("POST", "{p}", body);
    if (out && out.token) this.client._setToken(out.token);
    return out;
  }}
"#
        ),
        None => String::new(),
    };

    let logout = match &s.logout {
        Some(p) => format!(
            r#"
  /** Clear the server session, then drop the token locally — even if the
      request fails, so a user pressing "log out" is never left holding one. */
  async logout() {{
    try {{ await this.client._request("POST", "{p}"); }}
    finally {{ this.client._setToken(null); }}
  }}
"#
        ),
        None => r#"
  /** No server logout endpoint; drop the token locally. */
  async logout() { this.client._setToken(null); }
"#
        .to_string(),
    };

    let me = match &s.me {
        Some(p) => format!(
            r#"
  /** The current user, or `null` when not signed in. A 401 is the ANSWER to
      "am I logged in?", not an error — so it resolves null instead of throwing.
      Any other failure still throws. */
  async me() {{
    try {{
      return await this.client._request("GET", "{p}");
    }} catch (err) {{
      if (err instanceof UmbralError && err.status === 401) return null;
      throw err;
    }}
  }}
"#
        ),
        None => String::new(),
    };

    let class = format!(
        r#"
/** The session client — `api.auth`. Wraps this app's auth endpoints and owns
    the bearer token, which every subsequent request picks up automatically. */
export class AuthClient {{
  constructor(client) {{ this.client = client; }}

  /** The bearer token currently in use, or null. */
  get token() {{ return this.client._token; }}

  /** Sign in. Stores the returned token; later calls send it automatically.
      (Browsers also get the server's session cookie, so either works.) */
  async login(credentials) {{
    const out = await this.client._request("POST", "{login}", credentials);
    this.client._setToken(out && out.token ? out.token : null);
    return out;
  }}
{register}{logout}{me}}}
"#
    );
    (class, "\n    this.auth = new AuthClient(this);".to_string())
}

fn emit_js(e: &Emit<'_>) -> String {
    let (base_path, auth, methods, session) = (e.base_path, e.auth, e.methods, e.session);
    // Auth defaults, derived from the app's declared security schemes — not
    // hardcoded. `token` uses this Authorization prefix; `apiKey` uses this
    // header; a session (cookie) scheme sends credentials by default.
    let bearer_prefix = auth.bearer_prefix();
    let api_key_header = auth.api_key_header();
    let credentials_default = if auth.cookie {
        "\"include\""
    } else {
        "undefined"
    };
    let (auth_class, auth_wiring) = auth_runtime_js(session);

    let mut page_impls = String::new();
    for m in methods {
        let _ = write!(
            page_impls,
            "\n  /** {doc} */\n  {name}(v) {{ this.params.set(\"{wire}\", String(v)); return this; }}\n",
            doc = m.doc,
            name = m.name,
            wire = m.wire,
        );
    }

    format!(
        r#"{header}
/** Thrown when the API returns a non-2xx response. `body` is the parsed error. */
export class UmbralError extends Error {{
  constructor(status, body) {{
    super(`umbral: request failed with status ${{status}}`);
    this.name = "UmbralError";
    this.status = status;
    this.body = body;
  }}
}}

/** A list query. Built by `Umbral.from(table)`; see client.d.ts for the types. */
export class Query {{
  constructor(client, table) {{
    this.client = client;
    this.table = table;
    this.params = new URLSearchParams();
  }}

  /** Field filters — keys and value types are specific to this model. */
  filter(f) {{
    for (const [k, v] of Object.entries(f || {{}})) {{
      if (v === undefined) continue;
      this.params.set(k, Array.isArray(v) ? v.join(",") : String(v));
    }}
    return this;
  }}

  /** Full-text `?search=` across the model's searchable columns. */
  search(term) {{ this.params.set("search", term); return this; }}

  /** `?ordering=` — pass fields; prefix `-` for descending. */
  orderBy(...fields) {{ this.params.set("ordering", fields.join(",")); return this; }}
{page_impls}
  /** Set any raw query param — the escape hatch for params the typed builder
      methods don't cover (a custom paginator's cursor, a one-off flag). */
  param(key, value) {{ this.params.set(key, String(value)); return this; }}

  /** Sparse fieldset (`?fields=`) — fetch only these columns. */
  fields(...cols) {{ this.params.set("fields", cols.join(",")); return this; }}

  /** Fetch the list. Resolves to the envelope your paginator emits. */
  async list() {{
    const qs = this.params.toString();
    const path = `{base_path}/${{this.table}}/` + (qs ? `?${{qs}}` : "");
    return this.client._request("GET", path);
  }}
}}

{auth_class}
/** A typed client for this app's REST API. `new Umbral("https://api.example.com")`. */
export class Umbral {{
  constructor(baseUrl, opts = {{}}) {{
    this.baseUrl = String(baseUrl).replace(/\/+$/, "");
    this.opts = opts;
    this.realtimePath = (opts.realtimePath ?? "/realtime").replace(/\/+$/, "");
    this._rt = null;
    // The live bearer token: seeded from `opts.token` and replaced whenever it
    // changes. Every request reads it from here.
    this._token = opts.token ?? null;{auth_wiring}
  }}

  /** @internal Set the live token and notify the app so it can persist it.
   *  Deliberately NOT written to localStorage by the client: that is readable by
   *  any XSS on the page. Browsers already get an httpOnly session cookie; if you
   *  need the token across reloads, persist it yourself via `onToken` and decide
   *  the trade-off knowingly. */
  _setToken(token) {{
    this._token = token;
    if (this.opts.onToken) this.opts.onToken(token);
  }}

  /** Start a query against a REST-exposed table. */
  from(table) {{ return new Query(this, table); }}

  /** Retrieve one row by primary key. */
  get(table, id) {{ return this._request("GET", `{base_path}/${{table}}/${{id}}`); }}

  /** Create a row. */
  create(table, data) {{ return this._request("POST", `{base_path}/${{table}}/`, data); }}

  /** Partially update a row (PATCH). */
  update(table, id, data) {{ return this._request("PATCH", `{base_path}/${{table}}/${{id}}`, data); }}

  /** Delete a row by primary key. */
  async delete(table, id) {{ await this._request("DELETE", `{base_path}/${{table}}/${{id}}`); }}

  /** Subscribe to `created` / `updated` / `deleted` for a model.
   *
   *  Delegates to the realtime plugin's runtime (loaded once from
   *  `{{realtimePath}}/client.js`) rather than opening its own EventSource — so
   *  every subscription in every tab shares ONE server connection via
   *  SharedWorker, with presence and graceful degradation. Opening an
   *  EventSource per subscription would blow the browser's per-origin
   *  connection cap at ~6 models.
   *
   *  Returns synchronously; the underlying subscription attaches once the
   *  runtime loads, and `close()` before then cancels it.
   */
  on(table, handlers, opts) {{
    const group = opts && opts.group;
    if (!group) throw new Error("umbral: .on(...) requires opts.group — the group you expose(...)-d to");
    // SSR / non-browser: degrade to a no-op subscription, matching the realtime
    // runtime's own posture (no transport → a no-op unsubscribe, no noise). A
    // component that subscribes on mount then renders on the server must not
    // throw or log.
    if (typeof document === "undefined") return {{ close() {{}} }};
    let sub = null;
    let closed = false;
    this._realtime()
      .then((rt) => {{
        if (closed) return;
        sub = rt.model(String(table), handlers || {{}}, {{ group }});
      }})
      .catch((err) => {{
        if (typeof console !== "undefined" && console.error) console.error(err);
      }});
    return {{
      close() {{
        closed = true;
        if (sub) {{ try {{ sub.unsubscribe(); }} catch (_) {{}} sub = null; }}
      }},
    }};
  }}

  /** @internal Load the realtime runtime once, memoised. It is a classic
   *  script (sets `window.umbral.realtime`), so it is injected via a script tag
   *  — that works cross-origin without CORS, unlike a dynamic `import()`. */
  _realtime() {{
    const g = globalThis;
    if (g.umbral && g.umbral.realtime) return Promise.resolve(g.umbral.realtime);
    if (this._rt) return this._rt;
    if (typeof document === "undefined") {{
      return Promise.reject(new Error("umbral: realtime requires a browser environment"));
    }}
    const src = `${{this.baseUrl}}${{this.realtimePath}}/client.js`;
    this._rt = new Promise((resolve, reject) => {{
      const done = () => {{
        if (g.umbral && g.umbral.realtime) resolve(g.umbral.realtime);
        else reject(new Error(`umbral: loaded ${{src}} but umbral.realtime is missing`));
      }};
      let el = document.querySelector('script[data-umbral-realtime]');
      if (!el) {{
        el = document.createElement("script");
        el.src = src;
        el.async = true;
        el.setAttribute("data-umbral-realtime", "");
        el.addEventListener("load", done);
        el.addEventListener("error", () =>
          reject(new Error(`umbral: failed to load ${{src}} — is RealtimePlugin mounted at ${{this.realtimePath}}?`)));
        document.head.appendChild(el);
      }} else {{
        el.addEventListener("load", done);
        el.addEventListener("error", () => reject(new Error(`umbral: failed to load ${{src}}`)));
        done();
      }}
    }});
    return this._rt;
  }}

  /** @internal */
  async _request(method, path, body) {{
    const doFetch = this.opts.fetch ?? fetch;
    const headers = {{ "Accept": "application/json", ...this.opts.headers }};
    // Auth, in ascending precedence: static token, static apiKey, then the
    // dynamic hook (so a fresh JWT overrides a stale static one). Defaults for
    // the prefix / header come from your API's declared security scheme.
    if (this._token) {{
      headers["Authorization"] = `${{this.opts.tokenPrefix ?? "{bearer_prefix}"}} ${{this._token}}`;
    }}
    if (this.opts.apiKey) {{
      headers[this.opts.apiKeyHeader ?? "{api_key_header}"] = this.opts.apiKey;
    }}
    if (this.opts.getAuthHeaders) {{
      Object.assign(headers, await this.opts.getAuthHeaders());
    }}
    const init = {{ method, headers }};
    const credentials = this.opts.credentials ?? {credentials_default};
    if (credentials) init.credentials = credentials;
    if (body !== undefined) {{
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }}
    const res = await doFetch(this.baseUrl + path, init);
    const parsed = res.status === 204 ? null : await res.json().catch(() => null);
    if (!res.ok) throw new UmbralError(res.status, parsed);
    return parsed;
  }}
}}
"#,
        header = header("client.js — the runtime (ES module)", base_path, auth),
    )
}

// =========================================================================
// client.d.ts — every type, plus declarations for the runtime's classes.
// =========================================================================

/// Auth types + the `AuthClient` declaration, or empty when the app serves no
/// auth endpoints. Types come from the *published* request/response schemas, so
/// they track the real contract.
fn auth_types_dts(session: Option<&AuthEndpoints>) -> (String, String) {
    let Some(s) = session else {
        return (String::new(), String::new());
    };
    let mut out = format!(
        "/** The signed-in user, from this app's `/me` + login response schema. */\n\
         export type AuthUser = {user};\n\n\
         /** Credentials the login endpoint accepts. */\n\
         export type LoginCredentials = {login};\n\n\
         /** What a successful login returns. The token is stored on the client \
         automatically; browsers additionally get an httpOnly session cookie. */\n\
         export interface LoginResult {{\n  user: AuthUser;\n  token: string;\n}}\n",
        user = s.user,
        login = s.login_body,
    );
    let mut methods = String::from(
        "  /** The bearer token currently in use, or null. */\n  \
         readonly token: string | null;\n  \
         /** Sign in. Stores the token; later requests send it automatically. */\n  \
         login(credentials: LoginCredentials): Promise<LoginResult>;\n",
    );
    if s.register.is_some() {
        let _ = write!(
            out,
            "\n/** Fields the register endpoint accepts. */\nexport type RegisterCredentials = {};\n",
            s.register_body,
        );
        methods.push_str(
            "  /** Register, then adopt the returned token. */\n  \
             register(body: RegisterCredentials): Promise<LoginResult>;\n",
        );
    }
    methods.push_str(
        "  /** Clear the server session and drop the token locally. */\n  \
         logout(): Promise<void>;\n",
    );
    if s.me.is_some() {
        methods.push_str(
            "  /** The current user, or `null` when not signed in (a 401 is the\n      \
             answer to \"am I logged in?\", not an error). */\n  \
             me(): Promise<AuthUser | null>;\n",
        );
    }
    let _ = write!(
        out,
        "\n/** The session client — reachable as `client.auth`. */\n\
         export declare class AuthClient {{\n{methods}}}\n"
    );
    (
        out,
        "\n  /** The session client: login / logout / me. */\n  readonly auth: AuthClient;\n"
            .to_string(),
    )
}

/// Everything the two emitters need — passed as one context so the signature
/// doesn't grow a new positional argument every time the surface does.
struct Emit<'a> {
    /// Every model the app knows (FK value types resolve against this).
    all: &'a [ModelMeta],
    /// The REST-exposed subset, sorted — what actually gets emitted.
    exposed: &'a [ModelMeta],
    base_path: &'a str,
    style: PaginationStyle,
    schema: Option<&'a PaginationSchema>,
    auth: &'a AuthModel,
    methods: &'a [PageMethod],
    session: Option<&'a AuthEndpoints>,
}

fn emit_dts(e: &Emit<'_>) -> String {
    let (all, exposed, base_path, style, schema, auth, methods, session) = (
        e.all,
        e.exposed,
        e.base_path,
        e.style,
        e.schema,
        e.auth,
        e.methods,
        e.session,
    );
    let mut out = String::new();
    out.push_str(&header("client.d.ts — the types", base_path, auth));

    // Row types describe RESPONSES, and `hide(...)` is response-only — a hidden
    // column (a `password_hash`, an internal `cost`) is never in the JSON the API
    // returns. So strip hidden columns from the row interfaces. They stay
    // settable in the create/update DTOs (a hidden field can be write-only), and
    // FK value types still resolve against the full, unstripped model set.
    out.push_str(&umbral::typegen::typescript_for(&strip_hidden(exposed)));

    for model in exposed {
        out.push('\n');
        push_filters_type(&mut out, all, model);
        push_ordering_type(&mut out, model);
        push_create_type(&mut out, all, model);
        push_update_type(&mut out, all, model);
    }

    out.push('\n');
    out.push_str(&envelope_type(style, schema));
    out.push('\n');
    push_resource_map(&mut out, exposed);
    out.push('\n');
    out.push_str(&options_types(auth));
    let (auth_types, auth_member) = auth_types_dts(session);
    if !auth_types.is_empty() {
        out.push('\n');
        out.push_str(&auth_types);
    }
    out.push('\n');
    out.push_str(&class_declarations(methods, &auth_member));
    out
}

fn header(what: &str, base_path: &str, auth: &AuthModel) -> String {
    format!(
        "// Code generated by `umbral gen-client`. DO NOT EDIT.\n\
         //\n\
         // {what}\n\
         //\n\
         // Regenerate after any model or REST-resource change:\n\
         //     cargo run -- gen-client --out <this directory>\n\
         //\n\
         // Base path: {base_path}. Auth: {auth}.\n\n",
        auth = auth.summary(),
    )
}

/// `UmbralOptions` + the realtime interfaces.
fn options_types(auth: &AuthModel) -> String {
    let bearer_prefix = auth.bearer_prefix();
    let api_key_header = auth.api_key_header();
    let creds = if auth.cookie {
        "\"include\""
    } else {
        "undefined"
    };
    format!(
        r#"export interface UmbralOptions {{
  /** Token sent as `{bearer_prefix} <token>` in the `Authorization` header
      (the prefix comes from your API's declared security scheme). Override the
      prefix with `tokenPrefix` — e.g. an API that expects `Token <key>`. */
  token?: string;
  /** Overrides the `Authorization` prefix for `token`. Defaults to
      "{bearer_prefix}", read from the security scheme. */
  tokenPrefix?: string;
  /** API key sent in the `{api_key_header}` header (the header name comes from
      your API's declared apiKey scheme). Override with `apiKeyHeader`. */
  apiKey?: string;
  /** Overrides the header `apiKey` is sent in. Defaults to "{api_key_header}",
      read from the security scheme. */
  apiKeyHeader?: string;
  /** Called whenever the live token changes — `auth.login()` sets it,
      `auth.logout()` clears it (null). Persist it here if you need it across
      reloads. The client deliberately does NOT write it to localStorage: that is
      readable by any XSS on the page, and browsers already hold an httpOnly
      session cookie. Make that trade-off knowingly. */
  onToken?: (token: string | null) => void;
  /** Dynamic auth: called per request, its headers merged in last (they win).
      Use for a rotating JWT, a refresh flow, request signing — anything the
      static options above can't express. */
  getAuthHeaders?: () => Record<string, string> | Promise<Record<string, string>>;
  /** Extra static headers merged into every request. */
  headers?: Record<string, string>;
  /** `fetch` credentials mode (cookies). Defaults to {creds}. */
  credentials?: RequestCredentials;
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
"#
    )
}

/// Declarations for the classes `client.js` exports.
fn class_declarations(methods: &[PageMethod], auth_member: &str) -> String {
    let mut page_sigs = String::new();
    for m in methods {
        let _ = write!(
            page_sigs,
            "\n  /** {doc} */\n  {name}(v: {ty}): this;\n",
            doc = m.doc,
            name = m.name,
            ty = m.ty,
        );
    }
    format!(
        r#"/** Thrown when the API returns a non-2xx response. `body` is the parsed error. */
export declare class UmbralError extends Error {{
  readonly status: number;
  readonly body: unknown;
  constructor(status: number, body: unknown);
}}

/** A list query. Keys and value types are specific to the model it came from. */
export declare class Query<Row, Filters, Ordering> {{
  /** Field filters — the keys are exactly the (field, lookup) pairs this model accepts. */
  filter(f: Filters): this;
  /** Full-text `?search=` across the model's searchable columns. */
  search(term: string): this;
  /** `?ordering=` — pass fields; prefix `-` for descending. */
  orderBy(...fields: Ordering[]): this;
{page_sigs}
  /** Set any raw query param — the escape hatch for params the typed methods don't cover. */
  param(key: string, value: string | number | boolean): this;
  /** Sparse fieldset (`?fields=`) — fetch only these columns. */
  fields(...cols: string[]): this;
  /** Fetch the list. */
  list(): Promise<Paginated<Row>>;
}}

/** A typed client for this app's REST API. */
export declare class Umbral {{
  constructor(baseUrl: string, opts?: UmbralOptions);
{auth_member}
  /** Start a query against a REST-exposed table. */
  from<K extends keyof UmbralResources>(
    table: K,
  ): Query<UmbralResources[K]["row"], UmbralResources[K]["filters"], UmbralResources[K]["ordering"]>;

  /** Retrieve one row by primary key. */
  get<K extends keyof UmbralResources>(
    table: K,
    id: UmbralResources[K]["id"],
  ): Promise<UmbralResources[K]["row"]>;

  /** Create a row. The body type omits server-managed fields; `noedit` fields
      are allowed here (settable on create). */
  create<K extends keyof UmbralResources>(
    table: K,
    data: UmbralResources[K]["create"],
  ): Promise<UmbralResources[K]["row"]>;

  /** Partially update a row (PATCH). The body type excludes `noedit` fields. */
  update<K extends keyof UmbralResources>(
    table: K,
    id: UmbralResources[K]["id"],
    data: UmbralResources[K]["update"],
  ): Promise<UmbralResources[K]["row"]>;

  /** Delete a row by primary key. */
  delete<K extends keyof UmbralResources>(table: K, id: UmbralResources[K]["id"]): Promise<void>;

  /** Subscribe to `created` / `updated` / `deleted` for a model, over the
      realtime plugin's shared connection (one SSE stream across all tabs). The
      model must be `expose(...)`-d to `group` on the server. */
  on<K extends keyof UmbralResources>(
    table: K,
    handlers: ModelEvents<Partial<UmbralResources[K]["row"]>>,
    opts: {{ group: string }},
  ): Subscription;
}}
"#
    )
}

// =========================================================================
// Type emission (shared by the .d.ts): filters, ordering, write DTOs,
// envelope, resource map.
// =========================================================================

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

/// The TS scalar for a declared pagination field.
fn scalar_ts(s: PaginationScalar) -> &'static str {
    match s {
        PaginationScalar::String => "string",
        PaginationScalar::Number => "number",
        PaginationScalar::Boolean => "boolean",
    }
}

/// `page_size` / `next_cursor` → `pageSize` / `nextCursor` for a builder method
/// name. Envelope keys keep their wire name (they're response JSON keys); only
/// the *method* names are camelCased.
fn camel_case(s: &str) -> String {
    let pascal = pascal_case_from_ident(s);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// The list-response wrapper, shaped to the configured paginator.
///
/// Built-in styles have a known shape. A `Custom` paginator that declared a
/// [`PaginationSchema`] gets a *typed* envelope (its declared keys); a `Custom`
/// paginator that didn't gets an honest permissive envelope — `results`/`count`
/// optional plus an index signature — so reading a custom key still type-checks
/// without the generator pretending to know a shape it doesn't.
fn envelope_type(style: PaginationStyle, schema: Option<&PaginationSchema>) -> String {
    let doc = "/** A list response, shaped to this API's paginator. */";
    match (style, schema) {
        (PaginationStyle::Custom, Some(s)) => {
            let mut fields = String::from("  results: T[];\n");
            for f in &s.envelope {
                let null = if f.nullable { " | null" } else { "" };
                let _ = writeln!(fields, "  {}: {}{null};", f.name, scalar_ts(f.ty));
            }
            format!("{doc}\nexport interface Paginated<T> {{\n{fields}}}\n")
        }
        (PaginationStyle::Custom, None) => format!(
            "{doc}\n/** This app uses a custom paginator that did not declare its shape, \
             so the envelope is left open: read known keys, set params via `.param(...)`. */\n\
             export interface Paginated<T> {{\n  results?: T[];\n  count?: number;\n  \
             [key: string]: unknown;\n}}\n"
        ),
        (style, _) => {
            let extra = match style {
                PaginationStyle::PageNumber => {
                    "  total_pages: number;\n  current_page: number;\n  page_size: number;\n  \
                     next: number | null;\n  previous: number | null;\n"
                }
                PaginationStyle::LimitOffset => {
                    "  limit: number;\n  offset: number;\n  next: number | null;\n  \
                     previous: number | null;\n"
                }
                _ => "",
            };
            format!(
                "/** A list response. `results` + `count` are always present; the rest \
                 depend on the paginator. */\n\
                 export interface Paginated<T> {{\n  results: T[];\n  count: number;\n{extra}}}\n"
            )
        }
    }
}

/// `interface UmbralResources {{ "post": {{ row: Post; filters: PostFilters; ... }}; ... }}`
fn push_resource_map(out: &mut String, exposed: &[ModelMeta]) {
    out.push_str(
        "/** Maps each REST-exposed table to its row, filter, and ordering types. \
         `Umbral.from` keys off this. */\n",
    );
    out.push_str("export interface UmbralResources {\n");
    for model in exposed {
        // Each resource carries its OWN primary-key type — `number` for an i64
        // PK, `string` for a Uuid or String PK — so `.get`/`.update`/`.delete`
        // take exactly that model's id, not a union across every model.
        let id = model
            .pk_column()
            .map(ts_scalar_for_pk)
            .unwrap_or_else(|| "string | number".to_string());
        let _ = writeln!(
            out,
            "  \"{table}\": {{ row: {name}; filters: {name}Filters; ordering: {name}Ordering; \
             create: {name}Create; update: {name}Update; id: {id} }};",
            table = model.table,
            name = model.name,
        );
    }
    out.push_str("}\n");
}

/// The PK's TS scalar. A PK is never an FK or a choices column, so the plain
/// SqlType→TS scalar is right; `number` for int PKs, `string` for uuid/slug.
fn ts_scalar_for_pk(col: &Column) -> String {
    umbral::typegen::ts_base_type(
        &[],
        &ModelMeta {
            view: None,
            materialized: false,
            name: String::new(),
            table: String::new(),
            fields: vec![col.clone()],
            ..ModelMeta::default()
        },
        col,
    )
}

// =========================================================================
// Auth, derived from the declared OpenAPI security schemes.
// =========================================================================

/// How the client authenticates, derived from the app's OpenAPI security schemes
/// (`registered_security_schemes()`) — nothing is hardcoded. A `http`/bearer-style
/// scheme yields the `Authorization` prefix from its `scheme` field (`bearer` →
/// `Bearer`, `token` → `Token`); an `apiKey`/header scheme yields its header
/// `name` (`x-umbral-api-key`, whatever the API declared); an `apiKey`/cookie
/// scheme (session auth) flips fetch to send credentials. These become the
/// *defaults* baked into the generated client; every one stays overridable.
#[derive(Debug, Default)]
struct AuthModel {
    /// `Authorization` prefix for a bearer-style scheme (title-cased `scheme`).
    bearer_prefix: Option<String>,
    /// Header name for an `apiKey`-in-header scheme.
    api_key_header: Option<String>,
    /// An `apiKey`-in-cookie (session) scheme is present → send credentials.
    cookie: bool,
}

impl AuthModel {
    fn from_schemes(schemes: &[(String, Value)]) -> Self {
        let mut m = AuthModel::default();
        for (_name, v) in schemes {
            match v.get("type").and_then(Value::as_str) {
                Some("http") => {
                    let scheme = v.get("scheme").and_then(Value::as_str).unwrap_or("bearer");
                    // Basic auth has no token/key option — it goes through
                    // `headers` / `getAuthHeaders`. Any other http scheme is
                    // token-bearing; its `scheme` IS the Authorization prefix.
                    if !scheme.eq_ignore_ascii_case("basic") && m.bearer_prefix.is_none() {
                        m.bearer_prefix = Some(title_case(scheme));
                    }
                }
                Some("apiKey") => match v.get("in").and_then(Value::as_str) {
                    Some("header") => {
                        if m.api_key_header.is_none() {
                            if let Some(name) = v.get("name").and_then(Value::as_str) {
                                m.api_key_header = Some(name.to_string());
                            }
                        }
                    }
                    Some("cookie") => m.cookie = true,
                    _ => {}
                },
                _ => {}
            }
        }
        m
    }

    /// The `Authorization` prefix the generated client defaults to for `token`.
    fn bearer_prefix(&self) -> &str {
        self.bearer_prefix.as_deref().unwrap_or("Bearer")
    }

    /// The header the generated client defaults to for `apiKey`.
    fn api_key_header(&self) -> &str {
        self.api_key_header.as_deref().unwrap_or("X-API-Key")
    }

    /// One-line human summary for the file header.
    fn summary(&self) -> String {
        let mut parts = vec![format!("{} token", self.bearer_prefix())];
        parts.push(format!("apiKey ({})", self.api_key_header()));
        if self.cookie {
            parts.push("session cookie".to_string());
        }
        parts.join(" / ")
    }
}

/// Title-case a security-scheme token: `bearer` → `Bearer`, `token` → `Token`.
/// Only the first byte is upper-cased — HTTP auth schemes are single words.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

// =========================================================================
// The session client — discovered from the app's published OpenAPI paths.
// =========================================================================

/// The auth endpoints this app actually serves, discovered from the paths every
/// plugin publishes via `Plugin::openapi_paths()`.
///
/// Keyed off `operationId` (`auth_login`, `auth_logout`, `auth_me`,
/// `auth_register`), NOT off path spelling — so a plugin mounted at a custom
/// prefix (`.at("/accounts")`) still generates a working session client, and an
/// app with no auth plugin generates none at all (no dead code).
#[derive(Debug, Default)]
struct AuthEndpoints {
    login: Option<String>,
    logout: Option<String>,
    me: Option<String>,
    register: Option<String>,
    /// TS type of the login request body, from the published schema.
    login_body: String,
    /// TS type of the register request body.
    register_body: String,
    /// TS type of the user object (`/me`, and `login.user`).
    user: String,
}

impl AuthEndpoints {
    /// `Some` only when the app serves a login endpoint — the session client is
    /// meaningless without one.
    fn discover(paths: &[(String, Value)]) -> Option<Self> {
        let mut e = AuthEndpoints::default();
        for (path, item) in paths {
            // A path item maps method → operation.
            let Some(ops) = item.as_object() else {
                continue;
            };
            for (_method, op) in ops {
                let Some(id) = op.get("operationId").and_then(Value::as_str) else {
                    continue;
                };
                match id {
                    "auth_login" => {
                        e.login = Some(path.clone());
                        e.login_body = request_body_ts(op).unwrap_or_else(|| "unknown".into());
                        // login returns `{ user, token }` — lift the user shape.
                        if let Some(schema) = response_schema(op) {
                            if let Some(user) = schema.get("properties").and_then(|p| p.get("user"))
                            {
                                e.user = schema_to_ts(user);
                            }
                        }
                    }
                    "auth_logout" => e.logout = Some(path.clone()),
                    "auth_register" => {
                        e.register = Some(path.clone());
                        e.register_body = request_body_ts(op).unwrap_or_else(|| "unknown".into());
                    }
                    "auth_me" => {
                        e.me = Some(path.clone());
                        if e.user.is_empty() {
                            if let Some(schema) = response_schema(op) {
                                e.user = schema_to_ts(&schema);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if e.user.is_empty() {
            e.user = "Record<string, unknown>".to_string();
        }
        e.login.as_ref()?;
        Some(e)
    }
}

/// The `application/json` request-body schema of an operation, as a TS type.
fn request_body_ts(op: &Value) -> Option<String> {
    let schema = op
        .get("requestBody")?
        .get("content")?
        .get("application/json")?
        .get("schema")?;
    Some(schema_to_ts(schema))
}

/// The `200` `application/json` response schema of an operation.
fn response_schema(op: &Value) -> Option<Value> {
    op.get("responses")?
        .get("200")?
        .get("content")?
        .get("application/json")?
        .get("schema")
        .cloned()
}

/// Render a (simple, inline) JSON Schema as a TypeScript type.
///
/// Covers what the auth surface publishes: objects with `properties` +
/// `required`, and the scalar leaves. Anything it can't model degrades to
/// `unknown` rather than guessing — a wrong type is worse than an honest one.
fn schema_to_ts(schema: &Value) -> String {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(props) = schema.get("properties").and_then(Value::as_object) else {
                return "Record<string, unknown>".to_string();
            };
            let required: Vec<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let mut out = String::from("{ ");
            for (name, sub) in props {
                let opt = if required.contains(&name.as_str()) {
                    ""
                } else {
                    "?"
                };
                let _ = write!(out, "{name}{opt}: {}; ", schema_to_ts(sub));
            }
            out.push('}');
            out
        }
        Some("array") => match schema.get("items") {
            Some(items) => format!("{}[]", schema_to_ts(items)),
            None => "unknown[]".to_string(),
        },
        Some("string") => "string".to_string(),
        Some("integer") | Some("number") => "number".to_string(),
        Some("boolean") => "boolean".to_string(),
        _ => "unknown".to_string(),
    }
}
